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

# The Liquid Glass icon (macOS 26 and later): the Icon Composer document in
# macos/AppIcon.icon compiled into an asset catalog. Volant.icns above stays as
# the icon for macOS 12–25.
if xcrun --find actool >/dev/null 2>&1; then
  rm -rf build/icon && mkdir -p build/icon
  if xcrun actool macos/AppIcon.icon --compile build/icon --app-icon AppIcon \
      --platform macosx --target-device mac --minimum-deployment-target 12.0 \
      --enable-on-demand-resources NO --development-region en \
      --output-partial-info-plist build/icon/partial.plist \
      --output-format human-readable-text --notices --warnings --errors \
      && [ -f build/icon/Assets.car ]; then
    cp build/icon/Assets.car "$APP/Contents/Resources/Assets.car"
    plutil -insert CFBundleIconName -string AppIcon "$APP/Contents/Info.plist"
    # As Xcode does: the .icns actool rendered from the same document is the
    # icon file too, so nothing ever falls back to the old flat artwork (which
    # macOS 26 and later would shrink into a grey frame).
    if [ -f build/icon/AppIcon.icns ]; then
      cp build/icon/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"
      rm -f "$APP/Contents/Resources/Volant.icns"
      plutil -replace CFBundleIconFile -string AppIcon "$APP/Contents/Info.plist"
    fi
    echo "Liquid Glass icon compiled."
    cat build/icon/partial.plist
  else
    echo "warning: actool couldn't compile macos/AppIcon.icon; shipping the .icns only"
  fi
fi

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
