default:
    @just --list

# 运行测试.
test:
    cargo test

# 运行 clippy 检查.
clippy:
    cargo clippy

# 使用隔离数据库启动桌面应用并将完整代理 body 写入日志文件.
debug:
    CODEX_SWITCH_DATA_DIR=target/codex-switch-debug CODEX_SWITCH_LOG_FILE=target/codex-switch-debug/codex-switch.log CODEX_SWITCH_LOG_BODIES=1 RUST_LOG=codex_switch=trace,tower_http=debug cargo run

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
