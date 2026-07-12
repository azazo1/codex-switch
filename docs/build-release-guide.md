# 构建与发布指南

本项目使用 Rust stable 和 edition 2024. 常用任务通过 `justfile` 提供.

## 本地构建

安装 Rust 与 `just` 后运行:

```shell
just run
just test
just clippy
```

构建 release 二进制文件:

```shell
cargo build --locked --release --bins
```

输出位于 `target/release`.

## Linux 依赖

Ubuntu 或 Debian 构建环境可以安装:

```shell
sudo apt-get update
sudo apt-get install --no-install-recommends -y libayatana-appindicator3-dev libgtk-3-dev libwayland-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libxdo-dev libxkbcommon-dev pkg-config
```

Linux 程序需要图形桌面和可用的托盘实现. 如果中文显示为方框, 安装系统 CJK 字体后重新启动应用.

CI 使用 Ubuntu 22.04 构建 release, 并动态链接系统库. 对更旧 Linux 发行版的兼容性没有保证, 实际要求取决于二进制引用的 glibc 符号和运行时动态库.

## macOS 打包

生成 `.app`:

```shell
just macos-app
```

输出目录是 `target/macos-app/Codex Switch.app`.

生成 DMG:

```shell
just macos-dmg
```

输出文件名包含 `uname -m` 返回的架构. DMG 内含 `Codex Switch.app` 和指向 `/Applications` 的快捷入口.

Bundle 最低系统版本为 macOS 12.0. Bundle 版本读取 `Cargo.toml` 中的 package version.

## GitHub Actions

`.github/workflows/ci.yml` 在普通 push 和 pull request 上构建以下矩阵:

| 系统 | x64 target | arm64 target |
| --- | --- | --- |
| Linux | `x86_64-unknown-linux-gnu` | `aarch64-unknown-linux-gnu` |
| Windows | `x86_64-pc-windows-msvc` | `aarch64-pc-windows-msvc` |
| macOS | `x86_64-apple-darwin` | `aarch64-apple-darwin` |

只有 tag push 会上传产物并进入 GitHub Release job. Release 完全由 Actions 创建或更新, 本地 `gh` 不参与发布.

Release 标题使用 tag 名称, 正文使用 tag subject 和 body. Workflow 重跑时会更新正文并覆盖同名资产.

## 发布一个版本

先让 `Cargo.toml` 中的版本与计划 tag 一致, 再提交版本变更. 使用 annotated tag 保存 release 正文:

```shell
git tag -a v0.1.0 -m "v0.1.0" -m "这里填写发布内容"
git push origin v0.1.0
```

tag push 后, 在 GitHub Actions 中等待六个平台全部构建成功. Release job 会附加:

- Linux x64 和 arm64 的 `.tar.gz`.
- Windows x64 和 arm64 的 `.zip`.
- macOS x64 和 arm64 的 `.dmg`.

如果 `Cargo.toml` 版本没有同步, DMG 内的 Bundle 版本会与 tag 不一致.

## 发布边界

- macOS `.app` 和 DMG 当前未签名, 也未 notarize.
- Windows `.exe` 当前未进行代码签名.
- Linux 产物是动态链接的裸二进制压缩包, 不是发行版安装包.
- Workflow 没有上传校验和或 SBOM.

正式面向大量用户分发前, 应补充各平台签名, macOS notarization 和发布校验信息.
