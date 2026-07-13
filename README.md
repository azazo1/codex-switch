# Codex Switch

Codex Switch 是一个本地桌面中转工具. 它提供 OpenAI 兼容代理, 上游账号管理, 模型调度, 请求统计和运行状态查看, 适合在一台个人电脑上集中管理 Codex OAuth 账号和 API Key 中转站.

应用使用 Rust, eframe/egui 和 SQLite 构建. 默认只监听 `127.0.0.1:15721`, 代理服务需要在界面中手动启动.

## 主要能力

- 管理 OpenAI 兼容 API Key 上游和 Codex OAuth 账号.
- 代理 Responses, Chat Completions, Images 和 Models 请求.
- 使用固定, 随机, 加权轮询, 失败切换或模型映射选择上游.
- 查看活跃请求, 流式输出尾部, token 用量, 首 token 延迟和请求耗时.
- 查询 Codex 额度和多个中转平台的余额.
- 为低余额发送系统通知, 为长上下文会话保持 prompt cache.
- 使用本地 SQLite 保存设置, 统计, 日志和凭据.

## 下载

从 [GitHub Releases](https://github.com/azazo1/codex-switch/releases) 下载与系统和架构匹配的产物.

| 系统 | x64 | arm64 |
| --- | --- | --- |
| Windows | `codex-switch-windows-x64.zip` | `codex-switch-windows-arm64.zip` |
| Linux | `codex-switch-linux-x64.tar.gz` | `codex-switch-linux-arm64.tar.gz` |
| macOS | `codex-switch-macos-x64.dmg` | `codex-switch-macos-arm64.dmg` |

Windows 解压 ZIP 后运行 `codex-switch.exe`. Linux 解压后直接运行 `codex-switch`. Linux 桌面环境需要 GTK 3, Ayatana AppIndicator 和 libxdo 等运行库.

macOS 打开 DMG 后, 将 `Codex Switch.app` 拖到 `Applications`. 当前 DMG 未进行 Developer ID 签名和 notarization, 首次启动可能需要在 Finder 中右键应用并选择"打开".

## 快速开始

1. 启动 Codex Switch.
2. 打开顶部 `上游`, 添加 API Key 上游, 或通过设备登录和 `auth.json` 导入一个或多个 Codex OAuth 账号.
3. 默认 `Default` 调度组会使用全部已启用上游. 需要模型分流时再打开 `调度组` 配置.
4. 回到 `仪表盘`, 保持默认监听地址或填写新的地址, 然后点击 `启动`.
5. 将客户端 Base URL 设置为界面显示的地址, 默认是 `http://127.0.0.1:15721/v1`.
6. 将客户端 API key 设置为仪表盘中的 `本地访问 key`.

本地访问 key 通过 Bearer 认证发送. 可以用下面的请求确认模型接口已经可用:

```powershell
$headers = @{ Authorization = "Bearer cs-替换为界面中的key" }
Invoke-RestMethod -Uri "http://127.0.0.1:15721/v1/models" -Headers $headers
```

关闭主窗口时, 应用会在系统托盘可用的情况下隐藏到托盘, 代理服务继续运行. 需要结束进程时, 使用托盘菜单中的 `退出`.

## 文档

| 模块 | 文档 |
| --- | --- |
| 桌面应用和仪表盘 | [应用使用指南](docs/app-guide.md) |
| 本地代理和客户端接入 | [代理接入指南](docs/proxy-guide.md) |
| API Key 上游 | [上游管理指南](docs/upstream-guide.md) |
| Codex OAuth | [OAuth 使用指南](docs/oauth-guide.md) |
| 模型调度和路由 | [调度组配置指南](docs/scheduler-guide.md) |
| Prompt cache 保活 | [缓存保持配置指南](docs/cache-keepalive-guide.md) |
| 余额查询和系统提醒 | [余额提醒指南](docs/balance-alert-guide.md) |
| 运行中的请求 | [活跃连接指南](docs/active-connections-guide.md) |
| 请求记录和筛选 | [日志使用指南](docs/logs-guide.md) |
| SQLite, 凭据和备份 | [存储与备份指南](docs/storage-guide.md) |
| 本地构建和 GitHub Release | [构建与发布指南](docs/build-release-guide.md) |

## 本地开发

项目使用 Rust stable 和 edition 2024. 安装 Rust 与 `just` 后, 常用命令如下:

```shell
just run
just test
just clippy
```

Linux 构建依赖和 macOS 打包方式见[构建与发布指南](docs/build-release-guide.md).

## 安全说明

- 默认回环监听适合本机使用. 服务本身不提供 TLS, 不应直接暴露到公网.
- API Key, OAuth token, NewApi 凭据和本地访问 key 当前都以明文保存在本地 SQLite 中. 数据库和备份应按密钥文件保护.
- 本地访问 key 刷新后立即生效, 使用旧 key 的客户端会返回 `401`.
- 请求日志不保存 prompt 或完整响应正文, 但会保存模型, endpoint, 错误文本, token 和耗时等元数据.
- macOS 和 Windows 发布产物当前都未进行代码签名.
