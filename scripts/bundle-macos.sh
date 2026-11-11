#!/bin/bash
# Builds dist/Volant.app (universal: Apple Silicon + Intel), ad-hoc signs it,
# and packages dist/Volant.dmg and dist/Volant-mac.zip. Run on macOS.
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
export MACOSX_DEPLOYMENT_TARGET=12.0

rustup target add aarch64-apple-darwin x86_64-apple-darwin
cargo build --release --locked -p volant --target aarch64-apple-darwin
cargo build --release --locked -p volant --target x86_64-apple-darwin

rm -rf dist
APP="dist/Volant.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
lipo -create -output "$APP/Contents/MacOS/Volant" \
  target/aarch64-apple-darwin/release/volant \
  target/x86_64-apple-darwin/release/volant
sed "s/__VERSION__/$VERSION/g" macos/Info.plist > "$APP/Contents/Info.plist"
cp macos/Volant.icns "$APP/Contents/Resources/Volant.icns"
plutil -lint "$APP/Contents/Info.plist"

# Ad-hoc signature (no Apple developer account needed). Required on Apple Silicon.
codesign --force --deep --sign - "$APP"
codesign --verify --verbose "$APP"

ditto -c -k --keepParent "$APP" dist/Volant-mac.zip

mkdir -p dist/dmg
cp -R "$APP" dist/dmg/
ln -s /Applications dist/dmg/Applications
hdiutil create -volname "Volant" -srcfolder dist/dmg -ov -format UDZO dist/Volant.dmg
rm -rf dist/dmg

lipo -info "$APP/Contents/MacOS/Volant"
echo "Apple Silicon binary: $(stat -f %z target/aarch64-apple-darwin/release/volant) bytes"
ls -lh dist
