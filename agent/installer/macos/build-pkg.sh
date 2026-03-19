#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
AGENT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

TARGET_TRIPLE="${TARGET_TRIPLE:-aarch64-apple-darwin}"
APP_ID="${APP_ID:-io.browserport.browser-port}"
VERSION="${VERSION:-0.1.0}"
PKG_ROOT="$AGENT_DIR/target/pkgroot"
BIN_PATH="$AGENT_DIR/target/$TARGET_TRIPLE/release/browser-port"

cd "$AGENT_DIR"
cargo build --release --target "$TARGET_TRIPLE"

rm -rf "$PKG_ROOT"
mkdir -p "$PKG_ROOT/usr/local/bin"
cp "$BIN_PATH" "$PKG_ROOT/usr/local/bin/browser-port"

pkgbuild \
  --root "$PKG_ROOT" \
  --identifier "$APP_ID" \
  --version "$VERSION" \
  --install-location "/" \
  "$AGENT_DIR/target/browser-port-$VERSION-unsigned.pkg"

echo "Created unsigned pkg: $AGENT_DIR/target/browser-port-$VERSION-unsigned.pkg"
