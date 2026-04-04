#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
AGENT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

TARGET_TRIPLE="${TARGET_TRIPLE:-$(rustc -vV | awk '/host:/ {print $2}')}"
APP_ID="${APP_ID:-io.browserport.browser-port}"
MANIFEST_VERSION="${MANIFEST_VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' "$AGENT_DIR/Cargo.toml" | head -n1)}"
VERSION="${VERSION:-$MANIFEST_VERSION}"
PKG_VERSION="${PKG_VERSION:-$MANIFEST_VERSION}"
PKG_ROOT="$AGENT_DIR/target/pkgroot"
BIN_PATH="$AGENT_DIR/target/$TARGET_TRIPLE/release/browser-port"
OUTPUT_DIR="${OUTPUT_DIR:-$AGENT_DIR/target/dist}"
PKG_PATH="$OUTPUT_DIR/browser-port-$VERSION-$TARGET_TRIPLE-unsigned.pkg"
STANDALONE_PATH="$OUTPUT_DIR/browser-port-$VERSION-$TARGET_TRIPLE"

cd "$AGENT_DIR"
cargo build --release --bin browser-port --target "$TARGET_TRIPLE"

rm -rf "$PKG_ROOT"
mkdir -p "$PKG_ROOT/usr/local/bin"
cp "$BIN_PATH" "$PKG_ROOT/usr/local/bin/browser-port"
mkdir -p "$OUTPUT_DIR"

pkgbuild \
  --root "$PKG_ROOT" \
  --identifier "$APP_ID" \
  --version "$PKG_VERSION" \
  --install-location "/" \
  "$PKG_PATH"

cp "$BIN_PATH" "$STANDALONE_PATH"
chmod +x "$STANDALONE_PATH"

echo "Created unsigned pkg: $PKG_PATH"
echo "Created standalone binary: $STANDALONE_PATH"
