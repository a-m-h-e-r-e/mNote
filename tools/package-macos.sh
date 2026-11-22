#!/bin/sh
# Builds mNote.app and a drag-to-install disk image in dist/.
#
#   tools/package-macos.sh                 Apple Silicon build, signed ad hoc
#   UNIVERSAL=1 tools/package-macos.sh     Apple Silicon and Intel in one app
#   SIGN_IDENTITY="Developer ID Application: Name (TEAMID)" \
#   NOTARY_PROFILE=mnote tools/package-macos.sh
#                                          signed and notarized, so it opens
#                                          on any Mac without a warning
#
# NOTARY_PROFILE is a keychain profile made once with
# `xcrun notarytool store-credentials`.
set -eu

cd "$(dirname "$0")/.."
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
DIST=dist
APP="$DIST/mNote.app"

if [ "${UNIVERSAL:-0}" = 1 ]; then
    rustup target add aarch64-apple-darwin x86_64-apple-darwin >/dev/null
    cargo build --release -p mnote-app --target aarch64-apple-darwin
    cargo build --release -p mnote-app --target x86_64-apple-darwin
    ARCH=universal
else
    cargo build --release -p mnote-app
    ARCH=$(uname -m)
fi

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
if [ "${UNIVERSAL:-0}" = 1 ]; then
    lipo -create -output "$APP/Contents/MacOS/mnote" \
        target/aarch64-apple-darwin/release/mnote \
        target/x86_64-apple-darwin/release/mnote
else
    cp target/release/mnote "$APP/Contents/MacOS/mnote"
fi

# The icon, drawn from the design's logo, at every size macOS asks for.
ICONSET="$DIST/mNote.iconset"
rm -rf "$ICONSET"
mkdir -p "$ICONSET"
python3 -I tools/make_icon.py "$DIST/icon-1024.png"
for size in 16 32 128 256 512; do
    sips -z $size $size "$DIST/icon-1024.png" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    sips -z $double $double "$DIST/icon-1024.png" --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/mNote.icns"
rm -rf "$ICONSET" "$DIST/icon-1024.png"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>mNote</string>
    <key>CFBundleDisplayName</key><string>mNote</string>
    <key>CFBundleIdentifier</key><string>com.addisbyte.mnote</string>
    <key>CFBundleExecutable</key><string>mnote</string>
    <key>CFBundleIconFile</key><string>mNote</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>CFBundleVersion</key><string>$VERSION</string>
    <key>LSMinimumSystemVersion</key><string>12.0</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.productivity</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSHumanReadableCopyright</key><string>mNote $VERSION</string>
</dict>
</plist>
PLIST

# Ad hoc ("-") unless a Developer ID is given; the hardened runtime is what
# notarization requires.
IDENTITY=${SIGN_IDENTITY:--}
if [ "$IDENTITY" = "-" ]; then
    codesign --force --sign - "$APP"
else
    codesign --force --options runtime --timestamp --sign "$IDENTITY" "$APP"
fi
codesign --verify --strict "$APP"

# The disk image: the app beside an Applications shortcut.
STAGE="$DIST/dmg"
DMG="$DIST/mNote-$VERSION-macos-$ARCH.dmg"
rm -rf "$STAGE" "$DMG"
mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
hdiutil create -quiet -volname "mNote $VERSION" -srcfolder "$STAGE" -ov -format UDZO "$DMG"
rm -rf "$STAGE"

if [ "$IDENTITY" != "-" ]; then
    codesign --force --timestamp --sign "$IDENTITY" "$DMG"
    if [ -n "${NOTARY_PROFILE:-}" ]; then
        xcrun notarytool submit "$DMG" --keychain-profile "$NOTARY_PROFILE" --wait
        xcrun stapler staple "$DMG"
    fi
fi

echo "Built $DMG ($(du -h "$DMG" | cut -f1 | tr -d ' '))"
