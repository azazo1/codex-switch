[private]
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

# just dist
# 根据当前平台生成发布产物, 构建时注入自动生成的版本号.
[macos]
dist:
    CODEX_SWITCH_BUILD_VERSION="v$(bash scripts/build-version.sh)" cargo build --locked --release --bins
    bash scripts/dist-macos.sh target/release

[linux]
dist:
    CODEX_SWITCH_BUILD_VERSION="v$(bash scripts/build-version.sh)" cargo build --locked --release --bins
    bash scripts/dist-linux.sh target/release

[windows]
dist:
    CODEX_SWITCH_BUILD_VERSION="v$(powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-version.ps1 | tr -d '\r')" cargo build --locked --release --bins
    powershell -NoProfile -ExecutionPolicy Bypass -File scripts/dist-windows.ps1

# just macos-app
# 打包 macOS .app 到 target/macos-app/Codex Switch.app.
macos-app:
    cargo build --release --bins
    bash scripts/package-macos.sh

# 构建并打包 macOS .dmg 到 target/macos-app.
macos-dmg:
    cargo build --release --bins
    bash scripts/package-macos.sh target/release "target/macos-app/codex-switch-macos-$(uname -m).dmg"

# 以 v0.0.0 固定版本号走与 dist 完全相同的打包流程, 产物安装后用于反复测试自动更新.
# 测试运行时建议设置 CODEX_SWITCH_DATA_DIR 指向隔离数据目录, 避免读写全局数据.
# 使用示例: just fake-dist
fake-dist:
    CODEX_SWITCH_FAKE_VERSION=0.0.0 just dist
