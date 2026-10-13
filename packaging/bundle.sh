#!/bin/zsh
# Builds dist/Tachyon.app (and dist/Tachyon.dmg with --dmg).
# Requires Xcode command line tools (iconutil, codesign, swift).
set -euo pipefail

ROOT=${0:A:h:h}
DIST=$ROOT/dist
APP=$DIST/Tachyon.app
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)

echo "→ building release binary"
cargo build --release -p tachyon --manifest-path "$ROOT/Cargo.toml"

echo "→ assembling $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$ROOT/target/release/tachyon" "$APP/Contents/MacOS/tachyon"
cp -R "$ROOT/extension" "$APP/Contents/Resources/extension"

echo "→ rendering icon"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
swift "$ROOT/packaging/render_icon.swift" "$ROOT/packaging/icon.svg" "$WORK/Tachyon.iconset"
iconutil -c icns "$WORK/Tachyon.iconset" -o "$APP/Contents/Resources/Tachyon.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Tachyon</string>
  <key>CFBundleDisplayName</key><string>Tachyon</string>
  <key>CFBundleIdentifier</key><string>app.tachyon.downloads</string>
  <key>CFBundleExecutable</key><string>tachyon</string>
  <key>CFBundleIconFile</key><string>Tachyon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
  <key>NSHumanReadableCopyright</key><string>Tachyon Download Manager</string>
</dict>
</plist>
PLIST

echo "→ signing (ad hoc)"
codesign --force --deep --sign - "$APP"

if [[ "${1:-}" == "--dmg" ]]; then
  echo "→ creating disk image"
  STAGE=$WORK/dmg
  mkdir -p "$STAGE"
  cp -R "$APP" "$STAGE/"
  ln -s /Applications "$STAGE/Applications"
  rm -f "$DIST/Tachyon.dmg"
  hdiutil create -volname Tachyon -srcfolder "$STAGE" -ov -format UDZO "$DIST/Tachyon.dmg" >/dev/null
  echo "✓ $DIST/Tachyon.dmg"
fi

echo "✓ $APP"
