#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
AGENT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
REPO_DIR="$(cd "$AGENT_DIR/.." && pwd)"

TARGET_TRIPLE="${TARGET_TRIPLE:-$(rustc -vV | awk '/host:/ {print $2}')}"
APP_NAME="${APP_NAME:-BrowserPort}"
APP_BUNDLE_NAME="${APP_BUNDLE_NAME:-BrowserPort.app}"
APP_BUNDLE="$AGENT_DIR/target/${APP_BUNDLE_NAME}"
APP_RESOURCES="$APP_BUNDLE/Contents/Resources"
APP_EXECUTABLE="$APP_BUNDLE/Contents/MacOS/browser-port"
APP_FRAMEWORKS="$APP_BUNDLE/Contents/Frameworks"
APP_INFO_PLIST="$APP_BUNDLE/Contents/Info.plist"
APP_ICON_FILE="BrowserPort.icns"
APP_ICON_OUTPUT="$AGENT_DIR/target/$APP_ICON_FILE"
MANIFEST_VERSION="${MANIFEST_VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' "$AGENT_DIR/Cargo.toml" | head -n1)}"
VERSION="${VERSION:-$MANIFEST_VERSION}"
APP_VERSION="${APP_VERSION:-$MANIFEST_VERSION}"
APP_IDENTIFIER="${APP_IDENTIFIER:-io.browserport.browser-port}"
OUTPUT_DIR="${OUTPUT_DIR:-$AGENT_DIR/target/dist}"
DMG_NAME="${DMG_NAME:-browser-port-${VERSION}-${TARGET_TRIPLE}-unsigned.dmg}"
DMG_PATH="$OUTPUT_DIR/$DMG_NAME"
STANDALONE_PATH="$OUTPUT_DIR/browser-port-$VERSION-$TARGET_TRIPLE"
BUILD_BIN="$AGENT_DIR/target/$TARGET_TRIPLE/release/browser-port"
SYPHON_FRAMEWORK_SOURCE="$AGENT_DIR/target/release/Frameworks/Syphon.framework"
EXTENSION_ICON_SOURCE="$REPO_DIR/icons/trimed@4x.png"
CODESIGN_IDENTITY="${CODESIGN_IDENTITY:--}"
ICONSET_DIR="$(mktemp -d "$AGENT_DIR/target/browser-port-iconset.XXXXXX.iconset")"
STAGING_DIR="$(mktemp -d "$AGENT_DIR/target/browser-port-dmg.XXXXXX")"

cleanup() {
  rm -rf "$ICONSET_DIR"
  rm -f "$APP_ICON_OUTPUT"
  rm -rf "$STAGING_DIR"
}
trap cleanup EXIT

cd "$AGENT_DIR"
export BROWSER_PORT_APP_VERSION="${BROWSER_PORT_APP_VERSION:-$VERSION}"
cargo build --release --bin browser-port --target "$TARGET_TRIPLE"
"$AGENT_DIR/scripts/embed_syphon_framework.sh"

if [[ ! -x "$BUILD_BIN" ]]; then
  echo "Built binary not found: $BUILD_BIN" >&2
  exit 1
fi
if [[ ! -d "$SYPHON_FRAMEWORK_SOURCE" ]]; then
  echo "Syphon framework not found: $SYPHON_FRAMEWORK_SOURCE" >&2
  exit 1
fi
if [[ ! -f "$EXTENSION_ICON_SOURCE" ]]; then
  echo "Extension icon not found: $EXTENSION_ICON_SOURCE" >&2
  exit 1
fi

if ! command -v sips >/dev/null 2>&1; then
  echo "sips is required to generate the macOS app icon" >&2
  exit 1
fi
if ! command -v iconutil >/dev/null 2>&1; then
  echo "iconutil is required to generate the macOS app icon" >&2
  exit 1
fi
if ! command -v codesign >/dev/null 2>&1; then
  echo "codesign is required to create a valid macOS app bundle signature" >&2
  exit 1
fi

rm -rf "$ICONSET_DIR"
mkdir -p "$ICONSET_DIR"

generate_icon() {
  local size="$1"
  local file_name="$2"
  sips -z "$size" "$size" "$EXTENSION_ICON_SOURCE" --out "$ICONSET_DIR/$file_name" >/dev/null
}

generate_icon 16 "icon_16x16.png"
generate_icon 32 "icon_16x16@2x.png"
generate_icon 32 "icon_32x32.png"
generate_icon 64 "icon_32x32@2x.png"
generate_icon 128 "icon_128x128.png"
generate_icon 256 "icon_128x128@2x.png"
generate_icon 256 "icon_256x256.png"
generate_icon 512 "icon_256x256@2x.png"
generate_icon 512 "icon_512x512.png"
cp "$EXTENSION_ICON_SOURCE" "$ICONSET_DIR/icon_512x512@2x.png"

iconutil -c icns "$ICONSET_DIR" -o "$APP_ICON_OUTPUT"

rm -rf "$APP_BUNDLE"
mkdir -p "$APP_FRAMEWORKS" "$APP_BUNDLE/Contents/MacOS" "$APP_RESOURCES"
cp "$BUILD_BIN" "$APP_EXECUTABLE"
cp -R "$SYPHON_FRAMEWORK_SOURCE" "$APP_FRAMEWORKS/Syphon.framework"
cp "$APP_ICON_OUTPUT" "$APP_RESOURCES/$APP_ICON_FILE"

cat > "$APP_INFO_PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundleExecutable</key>
    <string>browser-port</string>
    <key>CFBundleIconFile</key>
    <string>${APP_ICON_FILE}</string>
    <key>CFBundleIdentifier</key>
    <string>${APP_IDENTIFIER}</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>${APP_NAME}</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>${APP_VERSION}</string>
    <key>CFBundleVersion</key>
    <string>${APP_VERSION}</string>
    <key>LSUIElement</key>
    <true/>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSPrincipalClass</key>
    <string>NSApplication</string>
</dict>
</plist>
EOF

# Re-sign the bundle after assembling all resources and embedded frameworks.
# Without this, rustc's linker-signed executable can cause Gatekeeper to treat
# the app bundle as damaged when downloaded from CI artifacts.
xattr -cr "$APP_BUNDLE"
if [[ "$CODESIGN_IDENTITY" == "-" ]]; then
  codesign --force --deep --sign - "$APP_BUNDLE"
else
  codesign --force --deep --options runtime --timestamp --sign "$CODESIGN_IDENTITY" "$APP_BUNDLE"
fi
codesign --verify --deep --strict --verbose=2 "$APP_BUNDLE"

mkdir -p "$STAGING_DIR"
cp -R "$APP_BUNDLE" "$STAGING_DIR/$APP_BUNDLE_NAME"
ln -s /Applications "$STAGING_DIR/Applications"
mkdir -p "$OUTPUT_DIR"
rm -f "$DMG_PATH"
hdiutil create \
  -volname "$APP_NAME" \
  -srcfolder "$STAGING_DIR" \
  -ov \
  -format UDZO \
  "$DMG_PATH"

cp "$BUILD_BIN" "$STANDALONE_PATH"
chmod +x "$STANDALONE_PATH"

echo "Created dmg: $DMG_PATH"
echo "Created standalone binary: $STANDALONE_PATH"
