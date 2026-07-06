default:
    @just --list

# 运行测试.
test:
    cargo test

# 运行 clippy 检查.
clippy:
    cargo clippy

# 启动桌面应用.
run:
    cargo run

# just macos-app
# 打包 macOS .app 到 target/macos-app/Codex Switch.app.
macos-app:
    cargo build --release --bins
    @set -e; \
    app_dir="target/macos-app/Codex Switch.app"; \
    contents="$app_dir/Contents"; \
    macos="$contents/MacOS"; \
    resources="$contents/Resources"; \
    plist="$contents/Info.plist"; \
    iconset="target/macos-app/CodexSwitch.iconset"; \
    icns="target/macos-app/AppIcon.icns"; \
    version=$(awk -F '"' '/^version = / { print $2; exit }' Cargo.toml); \
    rm -rf "$app_dir"; \
    mkdir -p "$macos" "$resources"; \
    cp target/release/codex-switch "$macos/codex-switch"; \
    cp assets/app-icon.svg "$resources/app-icon.svg"; \
    target/release/macos-iconset; \
    cp "$icns" "$resources/AppIcon.icns"; \
    rm -rf "$iconset"; \
    rm -f "$icns"; \
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
        > "$plist"; \
    echo "created $app_dir"
