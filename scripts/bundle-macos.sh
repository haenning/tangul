#!/usr/bin/env bash
# Builds target/bundle/tangul.app and a drag-to-Applications DMG.
#
#   scripts/bundle-macos.sh              # this Mac's architecture
#   scripts/bundle-macos.sh --universal  # Apple Silicon + Intel in one app
#
# Uses only tools that ship with macOS (sips, iconutil, lipo, codesign, hdiutil), plus
# Xcode 26's actool for the icon when available.
# The app is ad-hoc signed: fine on your own Macs, but a downloaded copy has to be
# allowed once in System Settings → Privacy & Security (see INSTALL.md). Real signing + notarization needs
# an Apple Developer ID.
set -euo pipefail
cd "$(dirname "$0")/.."

universal=false
[[ "${1:-}" == "--universal" ]] && universal=true

version=$(cargo metadata --no-deps --format-version 1 \
  | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "tangul"))')
out=target/bundle
app="$out/tangul.app"

if $universal; then
  targets=(aarch64-apple-darwin x86_64-apple-darwin)
  rustup target add "${targets[@]}" >/dev/null
  for t in "${targets[@]}"; do cargo build --release -p tangul --target "$t"; done
  arch=universal
else
  cargo build --release -p tangul
  arch=$(uname -m)
fi

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
if $universal; then
  lipo -create -output "$app/Contents/MacOS/tangul" \
    target/aarch64-apple-darwin/release/tangul target/x86_64-apple-darwin/release/tangul
else
  cp target/release/tangul "$app/Contents/MacOS/tangul"
fi
sed "s/@VERSION@/$version/g" packaging/macos/Info.plist > "$app/Contents/Info.plist"
# License texts ship with the app (AGPL-3.0 and the dependencies' notices).
cp LICENSE THIRD_PARTY_LICENSES.md "$app/Contents/Resources/"

# Icon: the Icon Composer document (light/dark/clear variants and the system glass,
# macOS 26+) compiled by actool into Assets.car plus tangul.icns for older macOS.
# Without Xcode 26 the .icns is built from the 1024 px PNG instead.
if xcrun actool --version >/dev/null 2>&1 &&
  xcrun actool packaging/macos/tangul.icon --compile "$app/Contents/Resources" \
    --platform macosx --minimum-deployment-target 11.0 --app-icon tangul \
    --output-partial-info-plist "$out/icon-info.plist" >/dev/null 2>&1 &&
  [[ -f "$app/Contents/Resources/Assets.car" ]]; then
  echo "icon: compiled packaging/macos/tangul.icon"
else
  echo "icon: actool unavailable (needs Xcode 26), using packaging/macos/icon.png"
  iconset="$out/tangul.iconset"
  rm -rf "$iconset" && mkdir -p "$iconset"
  for size in 16 32 128 256 512; do
    sips -z $size $size packaging/macos/icon.png --out "$iconset/icon_${size}x${size}.png" >/dev/null
    sips -z $((size * 2)) $((size * 2)) packaging/macos/icon.png --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
  done
  iconutil -c icns "$iconset" -o "$app/Contents/Resources/tangul.icns"
  rm -rf "$iconset"
fi
rm -f "$out/icon-info.plist"

# Ad-hoc signature (required on Apple Silicon; no developer account needed).
codesign --force --sign - --timestamp=none "$app"

# DMG with the app and an Applications shortcut to drag it onto.
dmg="$out/tangul-$version-$arch.dmg"
staging="$out/dmg"
rm -rf "$staging" "$dmg" && mkdir -p "$staging"
cp -R "$app" "$staging/"
ln -s /Applications "$staging/Applications"
hdiutil create -volname "tangul $version" -srcfolder "$staging" -ov -format UDZO "$dmg" >/dev/null
rm -rf "$staging"

echo "app: $app"
echo "dmg: $dmg"
