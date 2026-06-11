#!/bin/bash
# ARCC install script — downloads the latest release binary for your platform.
# Usage: curl -fsSL https://raw.githubusercontent.com/niyongsheng/arcc/main/scripts/install.sh | bash

set -e

REPO="niyongsheng/arcc"
VERSION="${1:-latest}"

# Detect platform
ARCH="$(uname -m)"
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"

# ---- source build helper ----
build_from_source() {
  local reason="$1"
  echo "ℹ️  $reason"
  echo "   Building from source..."
  if ! command -v cargo &>/dev/null; then
    echo "❌ Rust not found. Install it first: https://rustup.rs"
    exit 1
  fi
  local tmpdir
  tmpdir=$(mktemp -d)
  trap "rm -rf $tmpdir" EXIT
  git clone --depth 1 https://github.com/niyongsheng/arcc.git "$tmpdir"
  cd "$tmpdir" && cargo build --release
  BINARY="target/release/arcc"
  if [ ! -f "$BINARY" ]; then
    echo "❌ Build failed"
    exit 1
  fi
  install_binary "$BINARY" "built from source"
}

# ---- binary installation helper ----
install_binary() {
  local src="$1"
  local label="$2"

  if [ -w /usr/local/bin ]; then
    mv "$src" /usr/local/bin/arcc
    echo "✅ Installed to /usr/local/bin/arcc ($label)"
  elif [ -w "$HOME/.local/bin" ] || mkdir -p "$HOME/.local/bin" 2>/dev/null; then
    mv "$src" "$HOME/.local/bin/arcc"
    echo "✅ Installed to $HOME/.local/bin/arcc ($label)"
    case ":$PATH:" in
      *:"$HOME/.local/bin":*) ;;
      *) echo "⚠️  Add to your shell: export PATH=\"\$HOME/.local/bin:\$PATH\"" ;;
    esac
  else
    sudo mv "$src" /usr/local/bin/arcc 2>/dev/null || {
      echo "❌ Cannot install. Try: sudo mv $src /usr/local/bin/arcc"
      exit 1
    }
    echo "✅ Installed to /usr/local/bin/arcc ($label, with sudo)"
  fi
}

# ---- platform dispatch ----
case "$OS-$ARCH" in
  darwin-arm64)
    TARGET="aarch64-apple-darwin"
    ;;
  darwin-x86_64)
    build_from_source "Intel Mac — no pre-built binary available."
    arcc --help && exit 0
    ;;
  linux-x86_64)
    # Detect libc — prefer musl binary when available (works everywhere),
    # fall back to glibc binary for glibc ≥ 2.28, source build for old glibc.
    if command -v ldd &>/dev/null; then
      GLIBC_VER=$(ldd --version 2>&1 | grep -oP 'glibc \K[0-9]+\.[0-9]+' | head -1)
    fi
    if [ -z "$GLIBC_VER" ]; then
      # musl-based system (Alpine, etc.) — use musl binary.
      echo "ℹ️  musl libc detected — using musl binary"
      TARGET="x86_64-unknown-linux-musl"
    else
      GLIBC_MAJOR="${GLIBC_VER%.*}"
      GLIBC_MINOR="${GLIBC_VER#*.}"
      if [ "$GLIBC_MAJOR" -lt 2 ] || { [ "$GLIBC_MAJOR" -eq 2 ] && [ "$GLIBC_MINOR" -lt 28 ]; }; then
        # glibc too old — use musl binary instead (statically linked,
        # works on any Linux regardless of glibc version).
        echo "ℹ️  glibc $GLIBC_VER detected — using musl binary for compatibility"
        TARGET="x86_64-unknown-linux-musl"
      else
        echo "ℹ️  glibc $GLIBC_VER detected — OK"
        TARGET="x86_64-unknown-linux-gnu"
      fi
    fi
    ;;
  *)
    echo "❌ Unsupported platform: $OS $ARCH"
    echo "   Supported: macOS (arm64), Linux (x86_64)"
    echo ""
    echo "   💻 Windows: run the PowerShell script instead:"
    echo "      irm https://raw.githubusercontent.com/niyongsheng/arcc/main/scripts/install.ps1 | iex"
    exit 1
    ;;
esac

# ---- download pre-built binary ----
if [ "$VERSION" = "latest" ]; then
  URL="https://github.com/$REPO/releases/latest/download/arcc-$TARGET.tar.gz"
else
  URL="https://github.com/$REPO/releases/download/$VERSION/arcc-$TARGET.tar.gz"
fi

echo "⬇️  Downloading ARCC ($TARGET) ..."
TMP_DIR=$(mktemp -d)
trap "rm -rf $TMP_DIR" EXIT

curl -sL "$URL" | tar xz -C "$TMP_DIR"
BINARY="$TMP_DIR/arcc"
[ ! -f "$BINARY" ] && { echo "❌ Download failed"; exit 1; }

# Detect version tag from download redirect
TAG=$(curl -sLI -o /dev/null -w '%{url_effective}' "$URL" 2>/dev/null | sed 's|.*/download/||;s|/arcc-.*||' || echo "")

install_binary "$BINARY" "$TAG"

echo "✅ ARCC $TAG installed successfully!"
arcc --help
