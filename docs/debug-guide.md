# 隔离调试指南

调试模式用于检查客户端请求, 协议转换和上游响应. 它不会打开日常使用的 SQLite 数据库.

## 普通模式调试日志

仪表盘新增 `调试日志` 区域. 开启 `启用完整调试日志` 后, 普通模式会持续在 `codex-switch-proxy.log` 中记录完整代理 body 和完整 tracing. 日志按每日和配置的单文件大小轮转, 超过 `轮转文件数` 后自动删除旧文件.

轮转大小和文件数修改后, 点击 `应用轮转设置` 会立即重建日志写入器并持久化设置. `打开日志位置` 可以查看日志目录.

下面的 recipe 会启动完整的隔离桌面应用:

```shell
just debug
```

该实例使用 `target/codex-switch-debug/codex-switch.sqlite`. 首次运行时需要在界面中单独添加待检查上游, 设置一个未占用的监听端口, 然后从客户端发送复现请求.

日志写入 `target/codex-switch-debug/codex-switch.log`, 模型调用日志写入 `target/codex-switch-debug/codex-switch-proxy.log`, 包括完整入站 body, 转换后的上游 body, 上游响应和流式块. 所有出站网络请求 (含 `/v1/models` 和余额查询) 会追加写入 `target/codex-switch-debug/codex-switch-network.har`, 详见[日志使用指南](logs-guide.md). 每次执行 `just debug` 都会覆盖上一次日志. Authorization 和保存的 API Key 不会输出.

完整 body 可能包含 prompt, tool arguments 和模型输出. 调试完成后应停止该实例, 不要公开日志文件. `target` 目录已被 Git 忽略.

## 环境开关

`just debug` 使用以下开关:

| 变量 | 作用 |
| --- | --- |
| `CODEX_SWITCH_DATA_DIR` | 覆盖 SQLite 和应用数据目录 |
| `CODEX_SWITCH_LOG_FILE` | 主日志写入指定文件, 模型调用日志写入同目录 `<名称>-proxy.log`, 网络请求写入同目录 `<名称>-network.har`, 每次启动覆盖旧文件 |
| `CODEX_SWITCH_LOG_BODIES` | 设置为 `1`, `true`, `yes` 或 `on` 时输出完整代理 body 和网络请求 HAR |
| `RUST_LOG` | 控制 tracing target 和级别 |

如果设置了上述环境变量, 以环境变量为准. 不设置这些变量时, 普通模式的日志开关和轮转设置才会生效.
