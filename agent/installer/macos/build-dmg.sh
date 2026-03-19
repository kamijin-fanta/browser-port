#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
AGENT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
REPO_DIR="$(cd "$AGENT_DIR/.." && pwd)"

TARGET_TRIPLE="${TARGET_TRIPLE:-aarch64-apple-darwin}"
APP_NAME="${APP_NAME:-BrowserPort}"
APP_BUNDLE_NAME="${APP_BUNDLE_NAME:-BrowserPort.app}"
APP_BUNDLE="$AGENT_DIR/target/${APP_BUNDLE_NAME}"
APP_RESOURCES="$APP_BUNDLE/Contents/Resources"
APP_EXECUTABLE="$APP_BUNDLE/Contents/MacOS/browser-port"
APP_FRAMEWORKS="$APP_BUNDLE/Contents/Frameworks"
APP_INFO_PLIST="$APP_BUNDLE/Contents/Info.plist"
APP_ICON_FILE="BrowserPort.icns"
APP_ICON_OUTPUT="$AGENT_DIR/target/$APP_ICON_FILE"
APP_VERSION="${APP_VERSION:-0.1.0}"
APP_IDENTIFIER="${APP_IDENTIFIER:-io.browserport.browser-port}"
DMG_NAME="${DMG_NAME:-browser-port-${APP_VERSION}.dmg}"
DMG_PATH="$AGENT_DIR/target/$DMG_NAME"
BUILD_BIN="$AGENT_DIR/target/$TARGET_TRIPLE/release/browser-port"
SYMPHON_FRAMEWORK_SOURCE="$AGENT_DIR/target/release/Frameworks/Syphon.framework"
EXTENSION_ICON_SOURCE="$REPO_DIR/extention/icons/icon128.png"
ICONSET_DIR="$(mktemp -d "$AGENT_DIR/target/browser-port-iconset.XXXXXX.iconset")"
STAGING_DIR="$(mktemp -d "$AGENT_DIR/target/browser-port-dmg.XXXXXX")"

cleanup() {
  rm -rf "$ICONSET_DIR"
  rm -f "$APP_ICON_OUTPUT"
  rm -rf "$STAGING_DIR"
}
trap cleanup EXIT

cd "$AGENT_DIR"
cargo build --release --target "$TARGET_TRIPLE"
"$AGENT_DIR/scripts/embed_syphon_framework.sh"

if [[ ! -x "$BUILD_BIN" ]]; then
  echo "Built binary not found: $BUILD_BIN" >&2
  exit 1
fi
if [[ ! -d "$SYMPHON_FRAMEWORK_SOURCE" ]]; then
  echo "Syphon framework not found: $SYMPHON_FRAMEWORK_SOURCE" >&2
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
cp -R "$SYMPHON_FRAMEWORK_SOURCE" "$APP_FRAMEWORKS/Syphon.framework"
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

mkdir -p "$STAGING_DIR"
cp -R "$APP_BUNDLE" "$STAGING_DIR/$APP_BUNDLE_NAME"
ln -s /Applications "$STAGING_DIR/Applications"
rm -f "$DMG_PATH"
hdiutil create \
  -volname "$APP_NAME" \
  -srcfolder "$STAGING_DIR" \
  -ov \
  -format UDZO \
  "$DMG_PATH"

echo "Created dmg: $DMG_PATH"
