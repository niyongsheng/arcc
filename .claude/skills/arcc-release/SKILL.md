---
name: arcc-release
description: 发布 ARCC 新版本：检查 CI、bump workspace 版本、提交、打 tag、推送触发 CD、校验 4 平台产物并发布草稿 Release。触发词：发版、发布新版本、release、bump version、打 tag、arcc-release。
---

# ARCC Release

发布流程基于 `.github/workflows/cd.yml`：推送 `v*.*.*` tag → 4 平台构建 → 生成 changelog → 创建**草稿** Release（需人工确认后发布）。

## 0. 发布前检查

```bash
git status --short                 # 必须干净；有改动先提交或 stash
git branch --show-current          # 必须是 main
git fetch origin
git status -sb                     # main 必须与 origin/main 同步
gh run list --workflow=CI --commit "$(git rev-parse HEAD)" --limit 1   # HEAD 的 CI 必须通过
```

本地预检（CI 固定 Rust 1.99.0 且 `RUSTFLAGS=-Dwarnings`，本地未过不要发版）：

```bash
cargo check --all-targets
cargo test
cargo clippy --all-targets
```

## 1. 确定版本号

```bash
grep -A2 '^\[workspace.package\]' Cargo.toml | grep '^version'
```

规则：patch = bugfix，minor = 新功能，major = 破坏性变更。版本号不明确时**先问用户**，不要自行猜测。

## 2. Bump 版本

版本号只维护在根 `Cargo.toml`，需要同步改两处：

1. `[workspace.package] version`
2. `[workspace.dependencies]` 中 6 个内部 crate 的 `arcc-* = { version = "..." }`

```bash
OLD=$(awk -F'"' '/^version = "/{print $2; exit}' Cargo.toml)
NEW="0.x.0"

# macOS
sed -i '' "s/version = \"$OLD\"/version = \"$NEW\"/g" Cargo.toml
# Linux 用：sed -i "s/version = \"$OLD\"/version = \"$NEW\"/g" Cargo.toml
```

刷新 lockfile（**不要手改 Cargo.lock**）：

```bash
cargo check --workspace
git diff Cargo.lock     # 应只有 arcc* 的 version 行变化
```

检查其他版本引用：

```bash
grep -rn "$OLD" --include="*.md" --include="*.toml" --include="*.html" . \
  | grep -v target/ | grep -v Cargo.lock
```

## 3. 提交并打 tag

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore(release): bump version to $NEW"
git tag "v$NEW"        # 轻量 tag，与历史一致（v0.11.0 等均为 lightweight）
```

**推送前必须获得用户确认**（项目约定：默认只 commit，不 push）。确认后：

```bash
git push origin main
git push origin "v$NEW"
```

## 4. 跟踪 CD

```bash
RUN_ID=$(gh run list --workflow=CD --limit 1 --json databaseId --jq '.[0].databaseId')
gh run watch "$RUN_ID" --exit-status
```

预期 5 个 job：

- `build (aarch64-apple-darwin)`
- `build (x86_64-unknown-linux-gnu)`
- `build (x86_64-unknown-linux-musl)`
- `build (x86_64-pc-windows-msvc)`
- `create-release`

失败时：

```bash
gh run rerun "$RUN_ID" --failed
```

## 5. 校验草稿 Release

如果 CD 已成功但 Release 未发布（草稿遗留），直接从这一步继续。

```bash
gh release view "v$NEW" --json isDraft,assets --jq '{draft: .isDraft, assets: [.assets[].name]}'
```

必须包含 5 个资产：

- `arcc-aarch64-apple-darwin.tar.gz`
- `arcc-x86_64-unknown-linux-gnu.tar.gz`
- `arcc-x86_64-unknown-linux-musl.tar.gz`
- `arcc-x86_64-pc-windows-msvc.tar.gz`
- `checksums.txt`

检查 changelog（CD 由 `git log <prev_tag>..<tag>` 生成）：

```bash
gh release view "v$NEW" --json body --jq .body
```

## 6. 发布 Release

确认资产与 notes 无误、用户同意后取消草稿：

```bash
gh release edit "v$NEW" --draft=false
gh release view "v$NEW" --json isDraft,publishedAt --jq '{draft: .isDraft, publishedAt: .publishedAt}'
```

## 注意事项

- **不要复用已发布的版本号**。tag 已推送但 Release 未发布时，可修复后删除本地/远端 tag 重打；已发布则必须 bump 新版本。
- tag 必须是 `vX.Y.Z` 格式，否则 CD 不会触发。
- 工作区有未提交改动时禁止发版。
- CD 只构建二进制，不发布到 crates.io。
- 若 `docs/**` 有改动，确认 Deploy Pages 成功；未触发时执行 `gh workflow run "Deploy Pages" --ref main`。
- 发布后 `v0.11.0` 这类草稿若长期未发布，GitHub 上不会标记为 Latest。
