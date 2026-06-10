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
  darwin-x86_64)
    echo "ℹ️  Intel Mac: building from source..."
    if command -v cargo &>/dev/null; then
      git clone https://github.com/niyongsheng/arcc.git /tmp/arcc-build
      cd /tmp/arcc-build && cargo build --release && sudo mv target/release/arcc /usr/local/bin/
      rm -rf /tmp/arcc-build
      echo "✅ Built from source!"
      arcc --help && exit 0
    else
      echo "❌ Intel Mac: install Rust first: https://rustup.rs"
      exit 1
    fi
    ;;
  linux-x86_64)  TARGET="x86_64-unknown-linux-gnu" ;;
  *)
    echo "❌ Unsupported platform: $OS $ARCH"
    echo "   Supported: macOS (arm64), Linux (x86_64)"
    echo "   Windows users: download from GitHub Releases manually"
    exit 1
    ;;
esac

# Build download URL (use `/latest/download` to avoid API rate limits)
if [ "$VERSION" = "latest" ]; then
  URL="https://github.com/$REPO/releases/latest/download/arcc-$TARGET.tar.gz"
else
  URL="https://github.com/$REPO/releases/download/$VERSION/arcc-$TARGET.tar.gz"
fi

echo "⬇️  Downloading ARCC ($TARGET) ..."
TMP_DIR=$(mktemp -d)
trap "rm -rf $TMP_DIR" EXIT

# Download and extract
curl -sL "$URL" | tar xz -C "$TMP_DIR"
BINARY="$TMP_DIR/arcc"
[ ! -f "$BINARY" ] && { echo "❌ Download failed"; exit 1; }

# Extract version tag from redirect (HEAD request, lightweight)
TAG=$(curl -sLI -o /dev/null -w '%{url_effective}' "$URL" 2>/dev/null | sed 's|.*/download/||;s|/arcc-.*||' || echo "")

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
