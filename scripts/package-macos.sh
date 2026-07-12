#!/usr/bin/env bash

set -euo pipefail

profile_dir="${1:-target/release}"
dmg_path="${2:-}"
package_dir="target/macos-app"
app_dir="$package_dir/Codex Switch.app"
contents_dir="$app_dir/Contents"
macos_dir="$contents_dir/MacOS"
resources_dir="$contents_dir/Resources"
plist_path="$contents_dir/Info.plist"
iconset_dir="$package_dir/CodexSwitch.iconset"
icns_path="$package_dir/AppIcon.icns"
dmg_root="$package_dir/dmg-root"
version="$(awk -F '"' '/^version = / { print $2; exit }' Cargo.toml)"

cleanup() {
    rm -rf "$iconset_dir" "$dmg_root"
    rm -f "$icns_path"
}

trap cleanup EXIT

rm -rf "$app_dir"
mkdir -p "$macos_dir" "$resources_dir"
cp "$profile_dir/codex-switch" "$macos_dir/codex-switch"
cp assets/app-icon.svg "$resources_dir/app-icon.svg"
"$profile_dir/macos-iconset"
cp "$icns_path" "$resources_dir/AppIcon.icns"

printf '%s\n' \
    '<?xml version="1.0" encoding="UTF-8"?>' \
    '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">' \
    '<plist version="1.0">' \
    '<dict>' \
    '  <key>CFBundleDevelopmentRegion</key>' \
    '  <string>zh_CN</string>' \
    '  <key>CFBundleDisplayName</key>' \
    '  <string>Codex Switch</string>' \
    '  <key>CFBundleExecutable</key>' \
    '  <string>codex-switch</string>' \
    '  <key>CFBundleIdentifier</key>' \
    '  <string>com.azazo1.codex-switch</string>' \
    '  <key>CFBundleIconFile</key>' \
    '  <string>AppIcon</string>' \
    '  <key>CFBundleInfoDictionaryVersion</key>' \
    '  <string>6.0</string>' \
    '  <key>CFBundleName</key>' \
    '  <string>Codex Switch</string>' \
    '  <key>CFBundlePackageType</key>' \
    '  <string>APPL</string>' \
    '  <key>CFBundleShortVersionString</key>' \
    "  <string>$version</string>" \
    '  <key>CFBundleVersion</key>' \
    "  <string>$version</string>" \
    '  <key>LSMinimumSystemVersion</key>' \
    '  <string>12.0</string>' \
    '  <key>NSHighResolutionCapable</key>' \
    '  <true/>' \
    '</dict>' \
    '</plist>' \
    > "$plist_path"

echo "已创建 $app_dir"

if [[ -z "$dmg_path" ]]; then
    exit 0
fi

rm -rf "$dmg_root"
mkdir -p "$(dirname "$dmg_path")" "$dmg_root"
cp -R "$app_dir" "$dmg_root/"
ln -s /Applications "$dmg_root/Applications"
hdiutil create \
    -volname "Codex Switch" \
    -srcfolder "$dmg_root" \
    -ov \
    -format UDZO \
    "$dmg_path"

echo "已创建 $dmg_path"
