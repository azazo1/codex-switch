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

生成当前平台的发布归档:

```shell
just dist
```

输出位于 `dist/`.

归档文件名中的版本号自动从 `Cargo.toml` 读取.

构建产物会自动显示版本和构建 commit: 精确 tag 显示 `vX.Y.Z`, 非 tag 显示 `vX.Y.Z-<6 位 commit>`, 工作区有未提交改动时使用 `^` 分隔.

Windows 构建会将 `assets/app-icon.ico` 内嵌到 `.exe` 中. 图标包含从 16x16 到 256x256 的多档尺寸.

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

`.github/workflows/ci.yml` 在普通 push, pull request, tag push 和 `workflow_dispatch` 上会在 Ubuntu 22.04 运行 `cargo test --locked`, 并构建以下矩阵:

| 系统 | x64 target | arm64 target |
| --- | --- | --- |
| Linux | `x86_64-unknown-linux-gnu` | `aarch64-unknown-linux-gnu` |
| Windows | `x86_64-pc-windows-msvc` | `aarch64-pc-windows-msvc` |
| macOS | `x86_64-apple-darwin` | `aarch64-apple-darwin` |

tag push 或手动填写已有 tag 的 `workflow_dispatch` 会进入发布流程. `workflow_dispatch` 留空 tag 时只构建并上传 Actions artifact, 不会创建 release.

Release job 会先重新获取远端 tag object, 校验它是 annotated tag 且 annotation 与 `docs/release-notes/<version>.md` 完全一致, 再生成 notes. Release 完全由 Actions 创建或更新, 本地 `gh` 不参与发布.

Release 标题包含项目名和版本. 正文优先读取 `docs/release-notes/<version>.md`, 后面附加 GitHub 自动生成的提交和 PR 说明. 可选的 `<version>-base.txt` 用于指定累计 notes 的起始 tag. Workflow 重跑时会更新正文并覆盖同名资产.

## 发布一个版本

先让 `Cargo.toml` 中的版本与计划 tag 一致, 再提交版本变更. Workflow 会通过 `cargo metadata` 严格校验 `v<package-version>` 格式, 版本不一致时不会执行构建矩阵. 使用 annotated tag 保存 release 正文:

```shell
git tag -a v0.8.0 --cleanup=verbatim -F docs/release-notes/0.8.0.md
git push origin main v0.8.0
```

tag push 后, 在 GitHub Actions 中等待六个平台全部构建成功. Release job 会附加:

- Linux x64 和 arm64 的 `.tar.gz`.
- Windows x64 和 arm64 的 `.zip`.
- macOS x64 和 arm64 的 `.dmg`.
- 包含全部归档 SHA-256 摘要的 `SHA256SUMS`.

归档名称包含包版本, 系统和架构. Release 创建前会检查六个归档是否齐全.

需要重跑已有 tag 的发布时, 在 Actions 页面选择 `workflow_dispatch` 并填写同一个 tag. 留空 tag 只构建并上传 artifact.

## 发布边界

- macOS `.app` 和 DMG 当前未签名, 也未 notarize.
- Windows `.exe` 当前未进行代码签名.
- Linux 产物是动态链接的裸二进制压缩包, 不是发行版安装包.
- Workflow 没有生成 SBOM.

正式面向大量用户分发前, 应补充各平台签名, macOS notarization 和发布校验信息.
