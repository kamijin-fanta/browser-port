#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
AGENT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

TARGET_TRIPLE="${TARGET_TRIPLE:-$(rustc -vV | awk '/host:/ {print $2}')}"
VERSION="${VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' "$AGENT_DIR/Cargo.toml" | head -n1)}"
OUTPUT_DIR="${OUTPUT_DIR:-$AGENT_DIR/target/dist}"
BIN_PATH="$AGENT_DIR/target/$TARGET_TRIPLE/release/browser-port"
STAGING_DIR="$AGENT_DIR/target/linux-installer-root"
INSTALLER_PATH="$OUTPUT_DIR/browser-port-$VERSION-$TARGET_TRIPLE-linux-installer.tar.gz"
STANDALONE_PATH="$OUTPUT_DIR/browser-port-$VERSION-$TARGET_TRIPLE"

cd "$AGENT_DIR"
cargo build --release --bin browser-port --target "$TARGET_TRIPLE"

rm -rf "$STAGING_DIR"
mkdir -p "$STAGING_DIR/usr/local/bin" "$OUTPUT_DIR"
cp "$BIN_PATH" "$STAGING_DIR/usr/local/bin/browser-port"
chmod +x "$STAGING_DIR/usr/local/bin/browser-port"

cat > "$STAGING_DIR/install.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
install -d /usr/local/bin
install -m 0755 ./usr/local/bin/browser-port /usr/local/bin/browser-port
echo "Installed /usr/local/bin/browser-port"
EOF
chmod +x "$STAGING_DIR/install.sh"

cat > "$STAGING_DIR/uninstall.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
rm -f /usr/local/bin/browser-port
echo "Removed /usr/local/bin/browser-port"
EOF
chmod +x "$STAGING_DIR/uninstall.sh"

tar -C "$STAGING_DIR" -czf "$INSTALLER_PATH" .

cp "$BIN_PATH" "$STANDALONE_PATH"
chmod +x "$STANDALONE_PATH"

echo "Created installer tarball: $INSTALLER_PATH"
echo "Created standalone binary: $STANDALONE_PATH"
