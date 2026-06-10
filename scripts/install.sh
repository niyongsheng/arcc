#!/bin/bash
# ARCC install script — downloads the latest release binary for your platform.
# Usage: curl -fsSL https://raw.githubusercontent.com/niyongsheng/arcc/main/scripts/install.sh | bash

set -e

REPO="niyongsheng/arcc"
VERSION="${1:-latest}"

# Prefer Homebrew on macOS if available
if command -v brew &>/dev/null && [ "$(uname -s)" = "Darwin" ]; then
  echo "🍺 Homebrew detected — installing via brew formula..."
  FORMULA_URL="https://raw.githubusercontent.com/$REPO/main/scripts/arcc.rb"
  brew install "$FORMULA_URL" && echo "✅ Installed via Homebrew!" && arcc --help && exit 0
  echo "⚠️  Homebrew install failed, falling back to binary download..."
fi

# Detect platform
ARCH="$(uname -m)"
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"

case "$OS-$ARCH" in
  darwin-arm64)  TARGET="aarch64-apple-darwin" ;;
  darwin-x86_64) TARGET="x86_64-apple-darwin" ;;
  linux-x86_64)  TARGET="x86_64-unknown-linux-gnu" ;;
  *)
    echo "❌ Unsupported platform: $OS $ARCH"
    echo "   Supported: macOS (arm64/x86_64), Linux (x86_64)"
    exit 1
    ;;
esac

# Resolve version to a tag
if [ "$VERSION" = "latest" ]; then
  TAG=$(curl -sS "https://api.github.com/repos/$REPO/releases/latest" | grep '"tag_name"' | cut -d'"' -f4)
  [ -z "$TAG" ] && { echo "❌ Failed to resolve latest version"; exit 1; }
else
  TAG="$VERSION"
fi

echo "⬇️  Downloading ARCC $TAG ($TARGET) ..."

URL="https://github.com/$REPO/releases/download/$TAG/arcc-$TARGET.tar.gz"
TMP_DIR=$(mktemp -d)
trap "rm -rf $TMP_DIR" EXIT

curl -sL "$URL" | tar xz -C "$TMP_DIR"
BINARY="$TMP_DIR/arcc"
[ ! -f "$BINARY" ] && { echo "❌ Download failed"; exit 1; }

# Install — try /usr/local/bin first, fallback to ~/.local/bin
if [ -w /usr/local/bin ]; then
  mv "$BINARY" /usr/local/bin/arcc
  echo "✅ Installed to /usr/local/bin/arcc"
elif [ -w "$HOME/.local/bin" ] || mkdir -p "$HOME/.local/bin" 2>/dev/null; then
  mv "$BINARY" "$HOME/.local/bin/arcc"
  echo "✅ Installed to $HOME/.local/bin/arcc"
  case ":$PATH:" in
    *:"$HOME/.local/bin":*) ;;
    *) echo "⚠️  Add to your shell: export PATH=\"\$HOME/.local/bin:\$PATH\"" ;;
  esac
else
  sudo mv "$BINARY" /usr/local/bin/arcc 2>/dev/null || {
    echo "❌ Cannot install. Try: sudo mv $BINARY /usr/local/bin/arcc"
    exit 1
  }
  echo "✅ Installed to /usr/local/bin/arcc (with sudo)"
fi

echo "✅ ARCC $TAG installed successfully!"
arcc --help
