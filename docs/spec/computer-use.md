# Computer Use (macOS / TUI)

## Scope

v1 gives the ARCC TUI the ability to observe the screen and synthesize mouse /
keyboard input. It is available on macOS only and disabled by default.

Non-goals: CLI/ACP/Server modes, Linux/Windows capture, OCR/Accessibility-tree
fallback, and rendering screenshots inside the terminal.

## Enabling

```toml
[computer_use]
enabled = false                 # startup default
require_confirmation = true     # y/a/n for every input action
confirmation_timeout_seconds = 120
max_rounds = 20                 # screenshot→action rounds per prompt
max_screenshot_width = 1280
keep_screenshots = 1            # images kept in the request context
capture_after_action = true
action_settle_ms = 250
vision_model = ""               # empty = model.flash_model
save_screenshots = false
```

In the TUI:

- `/computer status` — show flags, vision model, platform support.
- `/computer on` — enable for this process; resets session trust.
- `/computer off` — disable immediately and drop cached screenshots.
- `a` at a confirmation prompt trusts all further input actions until
  `/computer off`, `/clear`, or process exit.

`--dangerously-skip-permissions` does **not** bypass computer-use confirmation.

## Tool

A single tool named `computer`:

```json
{
  "action": "screenshot | status | click | double_click | right_click | move |
             drag | scroll | type | key | wait",
  "x": 0, "y": 0, "to_x": 0, "to_y": 0,
  "button": "left | right | middle",
  "text": "...", "press_enter": false,
  "keys": "cmd+shift+4",
  "direction": "up | down | left | right", "amount": 3,
  "milliseconds": 500,
  "display": 0,
  "include_screenshot": true
}
```

The tool result is JSON text. `action=screenshot` and post-action captures
attach an image to the *next user message* (DeepSeek only accepts images on
user-role messages).

## Screenshot pipeline

1. `xcap::Monitor::all()` → select primary or `display`.
2. Read logical geometry (`x/y/width/height`) and physical scale factor.
3. Capture physical pixels, downscale to `max_screenshot_width` (<= ~1300²
   total pixels so DeepSeek does not downscale again).
4. Encode PNG → base64 → `ChatMessage.images`.
5. If the frame is uniformly black, warn about Screen Recording permission.

Coordinate mapping uses the geometry of the screenshot the model actually saw:

```
screen_x = monitor.x + image_x * (monitor.width  / image_width)
screen_y = monitor.y + image_y * (monitor.height / image_height)
```

`enigo` absolute coordinates use the same global logical-point space on macOS.

## Model routing

`deepseek-v4-pro` is text-only. Any request whose message list contains an
image is routed to `[computer_use].vision_model` (default `flash_model`,
`deepseek-flash`), falling back to `providers.flash()` and finally the
originally selected provider. Text-only rounds keep the normal Pro/Flash
selection.

## Multi-round loop

Computer-use sessions stay in the tool-calling phase for up to `max_rounds`
rounds, then perform one final no-tool turn. The loop remains single-round for
non-computer sessions, preserving existing behavior.

Images are pruned before every request (`keep_screenshots`, default 1) and all
images are dropped when a final answer is produced. Base64 never reaches
SQLite; only the `[Screenshot] ...` text label is persisted.

## Safety

- Screenshot/status are read-only and auto-approved.
- Every input action prompts `y` (approve), `a` (trust session), `n` (reject);
  only explicit `y/yes/a/always` approves, everything else rejects. Timeouts
  reject and clear the prompt UI.
- Each action is logged as `AuditEvent::ComputerAction` with action, arguments
  (no image data), risk level, approval source, and result.
- The system prompt forbids typing passwords/2FA, clicking security dialogs,
  and driving a terminal with synthetic input (use `execute_command` instead).
- Screenshots are sent to the model provider; `/computer on` warns about this.

## macOS permissions

Grant the **terminal application** (or the `arcc` binary when launched
directly):

- **Screen Recording** — required by xcap.
- **Accessibility** — required by enigo for synthetic events.

System Settings → Privacy & Security → Screen Recording / Accessibility, then
restart the terminal.
