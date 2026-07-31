#!/usr/bin/env python3
"""End-to-end test for `arcc --acp` — the ACP (Agent Client Protocol v1)
stdio server.

Drives the real binary over stdin/stdout with a mock provider (no
DEEPSEEK_API_KEY → arcc falls back to its built-in mock providers) and a
temporary ARCC_HOME, then asserts the full client lifecycle:

    initialize → session/new → set_mode/set_config_option
    → prompt (streamed chunks + end_turn + usage)
    → second prompt in the same session
    → cancel mid-turn (stopReason "cancelled")
    → error cases (-32601 / -32001)
    → session/close → batch → EOF → clean exit

Exit code 0 = all checks passed. Every stdout line must parse as JSON —
any stray non-JSON output on stdout fails the run.

Usage: python3 scripts/acp_e2e_test.py
"""

import json
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path

BIN = Path(__file__).resolve().parent.parent / "target" / "debug" / "arcc"


class AcpClient:
    """Line-delimited JSON-RPC 2.0 client over a subprocess's stdio."""

    def __init__(self, proc: subprocess.Popen):
        self.proc = proc
        self.next_id = 1
        # Notifications collected across the run (for assertions).
        self.notifications = []
        # Responses that arrived before their `expect_response` call.
        self.pending_responses = {}

    # -- low-level -------------------------------------------------------

    def send(self, obj) -> None:
        self.proc.stdin.write(json.dumps(obj) + "\n")
        self.proc.stdin.flush()

    def _read_message(self, timeout: float) -> dict:
        """Read one stdout line. Auto-responds to permission requests
        (allow once) so the turn never blocks on us."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            line = self.proc.stdout.readline()
            if not line:
                raise RuntimeError("arcc exited early; stderr below\n" + self.stderr())
            msg = json.loads(line)  # non-JSON stdout fails the run here
            if msg.get("method") == "session/request_permission":
                self.send({
                    "jsonrpc": "2.0",
                    "id": msg["id"],
                    "result": {"outcome": {"outcome": "selected", "optionId": "allow_once"}},
                })
                continue
            return msg
        raise RuntimeError(f"timeout reading stdout; stderr below\n{self.stderr()}")

    def read_notification(self, predicate=None, timeout: float = 10.0) -> dict:
        """Read until a notification matching `predicate` arrives."""
        while True:
            msg = self._read_message(timeout)
            if "method" not in msg:
                raise RuntimeError(f"expected notification, got: {msg}")
            self.notifications.append(msg)
            if predicate is None or predicate(msg):
                return msg

    def expect_response(self, mid, timeout: float = 10.0) -> dict:
        """Read until the response with id == mid arrives, collecting
        notifications on the way."""
        if mid in self.pending_responses:
            return self.pending_responses.pop(mid)
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            msg = self._read_message(timeout)
            if "method" in msg:
                self.notifications.append(msg)
            elif msg.get("id") == mid:
                return msg
            else:
                self.pending_responses[msg.get("id")] = msg
        raise RuntimeError(f"timeout waiting for response {mid}; stderr below\n{self.stderr()}")

    # -- JSON-RPC --------------------------------------------------------

    def request(self, method: str, params=None, timeout: float = 10.0) -> dict:
        mid = self.next_id
        self.next_id += 1
        self.send({"jsonrpc": "2.0", "id": mid, "method": method, "params": params or {}})
        return self.expect_response(mid, timeout)

    def notify(self, method: str, params=None) -> None:
        self.send({"jsonrpc": "2.0", "method": method, "params": params or {}})

    def stderr(self) -> str:
        return self.proc.stderr.read()


def main() -> int:
    failures: list[str] = []

    def check(cond: bool, msg: str) -> None:
        if cond:
            print(f"  ✓ {msg}")
        else:
            failures.append(msg)
            print(f"  ✗ {msg}")

    if not BIN.exists():
        print(f"binary not found at {BIN} — run `cargo build` first", file=sys.stderr)
        return 2

    with tempfile.TemporaryDirectory(prefix="arcc-acp-e2e-") as tmp:
        env = os.environ.copy()
        env["ARCC_HOME"] = tmp
        env.pop("DEEPSEEK_API_KEY", None)  # force the mock provider

        proc = subprocess.Popen(
            [str(BIN), "--acp"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=env,
        )
        client = AcpClient(proc)

        # ---- initialize -------------------------------------------------
        print("== initialize ==")
        r = client.request("initialize")["result"]
        check(r.get("protocolVersion") == 1, "protocolVersion == 1")
        check(r.get("authMethods") == [], "authMethods empty (no auth)")
        check(r["agentCapabilities"].get("loadSession") is False, "loadSession disabled")
        check(r["agentCapabilities"]["sessionCapabilities"].get("close") is not None,
              "sessionCapabilities.close advertised")
        check(r["agentInfo"].get("name") == "arcc" and r["agentInfo"].get("version"),
              f"agentInfo: {r['agentInfo']}")
        check("serverCapabilities" in r and "serverInfo" in r,
              "fake-acp dual-write fields present (serverCapabilities/serverInfo)")

        # ---- session/new -------------------------------------------------
        print("== session/new ==")
        r = client.request("session/new", {"cwd": tmp})["result"]
        sid = r["sessionId"]
        check(isinstance(sid, str) and "-" in sid, f"uuid session id: {sid}")
        check(r["modes"] == ["default"], "modes == [default]")
        check(any(o.get("key") == "model" for o in r["configOptions"]),
              "model config option exposed")
        check(len(r["models"]["availableModels"]) == 2, "pro + flash available")

        # ---- set_mode / set_config_option --------------------------------
        print("== set_mode / set_config_option ==")
        resp = client.request("session/set_mode", {"sessionId": sid, "mode": "default"})
        check("result" in resp, "set_mode default accepted")
        resp = client.request("session/set_mode", {"sessionId": sid, "mode": "bogus"})
        check(resp.get("error", {}).get("code") == -32602, "set_mode bogus → -32602")
        resp = client.request("session/set_config_option",
                              {"sessionId": sid, "key": "model", "value": "pro"})
        check(resp["result"]["configOptions"][0]["defaultValue"] == "pro",
              "set_config_option model=pro accepted")
        client.request("session/set_config_option",
                       {"sessionId": sid, "key": "model", "value": "flash"})

        # ---- prompt round 1 ----------------------------------------------
        print("== prompt round 1 (streaming) ==")
        before = len(client.notifications)
        resp = client.request("session/prompt", {
            "sessionId": sid,
            "prompt": [{"type": "text", "text": "hello from e2e"}],
        })
        r = resp["result"]
        check(r.get("stopReason") == "end_turn", f"stopReason == end_turn")
        check("usage" in r and r["usage"].get("inputTokens") is not None,
              f"usage reported: {r.get('usage')}")
        chunks = [
            m["params"]["update"]["content"]["text"]
            for m in client.notifications[before:]
            if m["params"]["update"]["sessionUpdate"] == "agent_message_chunk"
        ]
        check(len(chunks) > 0, f"{len(chunks)} agent_message_chunk notifications")
        check("hello from e2e" in "".join(chunks), "chunks carry the mock echo of the prompt")
        check(any(m["params"]["update"]["sessionUpdate"] == "usage_update"
                  for m in client.notifications[before:]),
              "usage_update notification emitted")

        # ---- prompt round 2 (same session) --------------------------------
        print("== prompt round 2 (context continues) ==")
        resp = client.request("session/prompt", {
            "sessionId": sid,
            "prompt": [{"type": "text", "text": "second turn"}],
        })
        r = resp["result"]
        check(r.get("stopReason") == "end_turn", "second turn end_turn")
        text = "".join(
            m["params"]["update"]["content"]["text"]
            for m in client.notifications
            if m["params"]["update"]["sessionUpdate"] == "agent_message_chunk"
        )
        check("second turn" in text, "second turn echoed by the same session")

        # ---- cancel mid-turn ----------------------------------------------
        print("== cancel mid-turn ==")
        mid = client.next_id
        client.next_id += 1
        client.send({
            "jsonrpc": "2.0", "id": mid, "method": "session/prompt",
            "params": {"sessionId": sid, "prompt": [{"type": "text", "text": "cancel me"}]},
        })
        client.read_notification(
            lambda m: m["params"]["update"]["sessionUpdate"] == "agent_message_chunk"
        )
        check(True, "first chunk streamed (turn is running)")
        client.notify("session/cancel", {"sessionId": sid})
        resp = client.expect_response(mid)
        check(resp["result"]["stopReason"] == "cancelled",
              f"stopReason == cancelled (got {resp['result'].get('stopReason')})")

        # ---- error cases ---------------------------------------------------
        print("== error cases ==")
        resp = client.request("session/load", {"sessionId": sid})
        check(resp.get("error", {}).get("code") == -32601, "session/load → -32601")
        resp = client.request("session/prompt", {
            "sessionId": "does-not-exist",
            "prompt": [{"type": "text", "text": "hi"}],
        })
        check(resp.get("error", {}).get("code") == -32001, "unknown session → -32001")

        # ---- session/close -------------------------------------------------
        print("== session/close ==")
        resp = client.request("session/close", {"sessionId": sid})
        check(resp.get("result") == {}, "session/close → {}")
        resp = client.request("session/close", {"sessionId": sid})
        check(resp.get("error", {}).get("code") == -32001, "re-close → -32001")

        # ---- batch ----------------------------------------------------------
        print("== batch ==")
        client.send([
            {"jsonrpc": "2.0", "id": 200, "method": "initialize"},
            {"jsonrpc": "2.0", "id": 201, "method": "session/new", "params": {"cwd": tmp}},
        ])
        r200 = client.expect_response(200)
        r201 = client.expect_response(201)
        check(r200["result"]["protocolVersion"] == 1, "batch element 1 answered")
        check(isinstance(r201["result"]["sessionId"], str), "batch element 2 answered")

        # ---- EOF → clean exit ----------------------------------------------
        print("== EOF / clean exit ==")
        client.proc.stdin.close()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            check(False, "process did not exit after stdin EOF")
        stderr = client.proc.stderr.read()
        check(proc.returncode == 0, f"exit code 0 (got {proc.returncode})")
        check("panicked" not in stderr, "no panic in stderr")

    if failures:
        print(f"\n{failures} FAILED:")
        for f in failures:
            print(f"  - {f}")
        return 1
    print("\nall ACP e2e checks passed ✔")
    return 0


if __name__ == "__main__":
    sys.exit(main())
