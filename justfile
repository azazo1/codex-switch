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
    bash scripts/package-macos.sh

# 构建并打包 macOS .dmg 到 target/macos-app.
macos-dmg:
    cargo build --release --bins
    bash scripts/package-macos.sh target/release "target/macos-app/codex-switch-macos-$(uname -m).dmg"
