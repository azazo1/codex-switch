# 隔离调试指南

调试模式用于检查客户端请求, 协议转换和上游响应. 它不会打开日常使用的 SQLite 数据库.

下面的 recipe 会启动完整的隔离桌面应用:

```shell
just debug
```

该实例使用 `target/codex-switch-debug/codex-switch.sqlite`. 首次运行时需要在界面中单独添加待检查上游, 设置一个未占用的监听端口, 然后从客户端发送复现请求.

日志写入 `target/codex-switch-debug/codex-switch.log`, 包括完整入站 body, 转换后的上游 body, 上游响应和流式块. 每次执行 `just debug` 都会覆盖上一次日志. Authorization 和保存的 API Key 不会输出.

完整 body 可能包含 prompt, tool arguments 和模型输出. 调试完成后应停止该实例, 不要公开日志文件. `target` 目录已被 Git 忽略.

## 环境开关

`just debug` 使用以下开关:

| 变量 | 作用 |
| --- | --- |
| `CODEX_SWITCH_DATA_DIR` | 覆盖 SQLite 和应用数据目录 |
| `CODEX_SWITCH_LOG_FILE` | 将 tracing 日志写入指定文件, 每次启动覆盖旧文件 |
| `CODEX_SWITCH_LOG_BODIES` | 设置为 `1`, `true`, `yes` 或 `on` 时输出完整代理 body |
| `RUST_LOG` | 控制 tracing target 和级别 |

不设置这些变量时, 应用继续使用系统默认数据目录和普通日志级别.
