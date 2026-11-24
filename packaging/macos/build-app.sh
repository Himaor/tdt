#!/bin/bash
# Builds dist/TDT.app and dist/TDT-macOS.dmg.
#
#   packaging/macos/build-app.sh [--version vX.Y.Z] [--universal]
#
# --version    The release tag. The build stops when it does not match desktop/Cargo.toml.
# --universal  One app for Apple silicon and Intel Macs (needs both Rust targets).
#
# Signing:
#   MACOS_SIGN_IDENTITY   A "Developer ID Application" identity in the keychain.
#                         Without it the app gets an ad-hoc signature: it runs on
#                         this Mac, and other Macs ask the user to allow it in
#                         System Settings > Privacy & Security.
#   MACOS_NOTARY_PROFILE  A `xcrun notarytool store-credentials` profile. With it
#                         (and an identity), the disk image is notarized and stapled.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DESKTOP="$ROOT/desktop"
DIST="$ROOT/dist"
HERE="$ROOT/packaging/macos"

TAG=""
UNIVERSAL=0
while [ $# -gt 0 ]; do
    case "$1" in
        --version) TAG="${2:-}"; shift 2 ;;
        --universal) UNIVERSAL=1; shift ;;
        *) echo "error: unknown argument $1" >&2; exit 2 ;;
    esac
done

VERSION="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$DESKTOP/Cargo.toml" | head -n 1)"
if [ -n "$TAG" ] && [ "${TAG#[vV]}" != "$VERSION" ]; then
    echo "error: tag $TAG does not match desktop/Cargo.toml version $VERSION" >&2
    exit 1
fi
case "$VERSION" in
    *[!0-9.]*|"") echo "error: version '$VERSION' is not N.N.N" >&2; exit 1 ;;
esac

build() {
    cargo build --release --locked --bin TDT --manifest-path "$DESKTOP/Cargo.toml" "$@"
}

echo "Building TDT $VERSION..."
if [ "$UNIVERSAL" = 1 ]; then
    build --target aarch64-apple-darwin
    build --target x86_64-apple-darwin
    BIN="$(mktemp -d)/TDT"
    lipo -create -output "$BIN" \
        "$DESKTOP/target/aarch64-apple-darwin/release/TDT" \
        "$DESKTOP/target/x86_64-apple-darwin/release/TDT"
else
    build
    BIN="$DESKTOP/target/release/TDT"
fi

APP="$DIST/TDT.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/TDT"
sed "s/@VERSION@/$VERSION/g" "$HERE/Info.plist" > "$APP/Contents/Info.plist"
plutil -lint "$APP/Contents/Info.plist" >/dev/null

# The app icon, at each size that macOS asks for, from the 512 px orb.
ICONSET="$(mktemp -d)/TDT.iconset"
mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$ROOT/branding/tdt-app-icon.png" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    if [ "$double" -le 512 ]; then
        sips -z "$double" "$double" "$ROOT/branding/tdt-app-icon.png" --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
    fi
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/TDT.icns"

if [ -n "${MACOS_SIGN_IDENTITY:-}" ]; then
    echo "Signing with $MACOS_SIGN_IDENTITY..."
    codesign --force --timestamp --options runtime \
        --entitlements "$HERE/TDT.entitlements" \
        --sign "$MACOS_SIGN_IDENTITY" "$APP"
else
    echo "warning: MACOS_SIGN_IDENTITY is not set; the app gets an ad-hoc signature." >&2
    codesign --force --options runtime --entitlements "$HERE/TDT.entitlements" --sign - "$APP"
fi
codesign --verify --strict --verbose=2 "$APP"

# The disk image: the app and a link to /Applications to drag it onto.
STAGE="$(mktemp -d)"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
DMG="$DIST/TDT-macOS.dmg"
rm -f "$DMG"
hdiutil create -quiet -volname "TDT $VERSION" -srcfolder "$STAGE" -ov -format UDZO "$DMG"

if [ -n "${MACOS_SIGN_IDENTITY:-}" ]; then
    codesign --force --timestamp --sign "$MACOS_SIGN_IDENTITY" "$DMG"
    if [ -n "${MACOS_NOTARY_PROFILE:-}" ]; then
        echo "Notarizing..."
        xcrun notarytool submit "$DMG" --keychain-profile "$MACOS_NOTARY_PROFILE" --wait
        xcrun stapler staple "$DMG"
    fi
fi

(cd "$DIST" && shasum -a 256 "TDT-macOS.dmg" > "TDT-macOS.dmg.sha256")
echo "Built $APP and $DMG"
