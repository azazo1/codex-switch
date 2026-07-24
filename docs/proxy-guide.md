# 代理接入指南

本地代理提供 OpenAI 和 Anthropic 兼容 HTTP 端点, 并使用当前调度组为每个请求选择上游.

## 启动前准备

1. 在 `上游` 页面添加至少一个可用上游.
2. 在 `调度组` 页面确认当前入口组能够访问该上游.
3. 在 `仪表盘` 点击 `启动`.
4. 复制界面显示的 Base URL 和本地访问 key.

默认值:

```text
Base URL: http://127.0.0.1:15721/v1
Authorization: Bearer cs-<uuid>
```

## 支持的端点

| 请求 | 说明 |
| --- | --- |
| `GET /health` | 服务健康检查, 不需要认证 |
| `GET /v1/models` | 汇总当前路由可达上游的模型 |
| `GET /models` | 不带 `/v1` 的模型列表别名 |
| `GET /v1/models/<id>` | 模型详情 |
| `POST /v1/responses` | Responses API |
| `POST /v1/responses/<subpath>` | Responses 子路径, 包括 `compact` 和 `input_tokens` |
| `POST /v1/chat/completions` | Chat Completions API |
| `POST /v1/messages` | Anthropic Messages API |
| `POST /v1/messages/count_tokens` | Anthropic 原生 token 计数 |
| `POST /v1/images/<subpath>` | Images 子路径, 例如 `generations` 和 `edits` |

`/responses`, `/chat/completions`, `/messages`, `/messages/count_tokens` 和 `/models/<id>` 也提供不带 `/v1` 的别名. `/backend-api/codex/responses` 及其子路径可用于 Codex 风格请求.

当前没有 `POST /v1/images` 根路径, 客户端必须使用具体子路径. Responses 的 `GET` 和 WebSocket 模式尚未实现, 当前会返回 `501`.

## 认证

所有已实现的代理接口都需要精确的本地访问 key. `/health` 和尚未实现的 Responses `GET` 占位路由除外. OpenAI 客户端通常使用 Bearer, Anthropic 客户端可以使用 `x-api-key`:

```text
Authorization: Bearer <仪表盘本地访问 key>
x-api-key: <仪表盘本地访问 key>
```

缺失或错误的 key 返回 `401` 和 `authentication_error`. 这个 key 只负责保护本地代理, 不会转发给上游. Relay 上游使用各自保存的 API Key, OAuth 上游使用自动维护的 access token.

## 模型列表

`GET /v1/models` 会遍历当前调度路径可达的上游:

- Relay 上游实际请求其 `/models` 接口.
- Codex OAuth 上游返回一个以上游名称构造的占位模型.
- 重复模型 ID 会去重.
- 模型映射规则会尽量反向还原客户端可见的模型 ID.
- 单个上游查询失败时仍返回其他结果, 所有来源失败时返回代理错误.

模型列表会发起外部请求, 不等同于静态缓存.

请求包含 `anthropic-version` 时, `/models` 和 `/models/<id>` 返回 Anthropic 模型结构. 列表支持 `before_id`, `after_id` 和 `limit`, 并返回 `has_more`, `first_id` 与 `last_id`. 未携带该请求头时保持 OpenAI `object=list` 结构.

## API 转换

API Key 上游可以声明 `Responses`, `Chat Completions` 或 `Anthropic Messages` Wire API. 三种文本入站协议都可以选择三种文本上游. Codex OAuth 作为 Responses 上游参与文本协议转换.

| 入站协议 | Responses 上游 | Chat 上游 | Anthropic 上游 |
| --- | --- | --- | --- |
| Responses | 直通 | 转换 | 转换 |
| Chat Completions | 转换 | 直通 | 经 Responses 转换 |
| Anthropic Messages | 转换 | 经 Responses 转换 | 直通 |

转换覆盖文本, base64/URL 图片, system/developer 指令, function/custom/namespace tools, tool call/result, tool choice, web search, reasoning/thinking, 输出 token 上限, sampling 参数, usage 和 SSE 生命周期. 同协议请求保持直通, 因此 provider 私有字段, Anthropic `cache_control`, thinking signature 和 `anthropic-beta` 可以原样保留.

跨协议时, document/PDF, audio 和未知 server tool 会返回客户端协议对应的 `400 invalid_request_error`. thinking signature 不会跨协议转发. `anthropic-beta` 只在 Anthropic 同协议直通时发送给上游.

Images 不会选择 Anthropic 上游. Responses compact 也不会选择 Anthropic 上游.

## Token 计数

`/messages/count_tokens` 只选择支持原生计数的 Responses 或 Anthropic 候选. Responses 候选请求 `/responses/input_tokens`, Anthropic 候选请求 `/messages/count_tokens`. Chat 候选会跳过. 没有原生候选时返回 Anthropic `404 not_found_error`, 不做本地 token 估算.

## Compact 请求

路径中以 `/responses/compact` 开头的请求只会选择勾选 `支持 compact` 的上游. 如果路由直接指向不支持 compact 的上游, 请求会失败, 不会自动寻找备用目标.

Chat Completions 上游的 compact 依赖转换能力, 应在实际中转站上验证后再开启该标记.

## 错误和重试

无法选择上游或网络连接失败通常返回 `502`. 跨协议无法表达的请求返回 `400`. 错误 envelope 会匹配入站 OpenAI 或 Anthropic 协议. 同协议上游 HTTP 错误保持原始响应.

是否重试由最终解析到的目标组决定. 失败切换组可能在返回错误前尝试下一个候选. 随机, 轮询, 固定上游和模型映射直达上游只有一个候选. 模型映射或固定模式跳入失败切换组后, 仍会执行该组的重试策略. 具体行为见[调度组配置指南](scheduler-guide.md).

单个上游可以配置 `错误重试` 策略. 调度层识别普通 HTTP `429`, HTTP `529`, Anthropic `overloaded_error`, `server_is_overloaded` 和 `slow_down`. JSON 错误与 Responses/Anthropic/Chat SSE 错误事件会转换为客户端协议的失败事件. 流式响应开始后无法透明切换上游.

## 已知限制

- 客户端 query string 当前不会附加到上游 URL.
- 只转发经过筛选的请求头, 任意自定义头不保证保留.
- 服务没有 TLS, 速率限制或请求体大小限制.
- WebSocket Responses 尚未实现.
- 非回环监听需要自行提供可信网络边界.
