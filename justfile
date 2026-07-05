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
