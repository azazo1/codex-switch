**Codex Switch 本地中转工具**

**概要**
- 从零实现一个 Rust 桌面工具: egui 管理界面 + 本地 OpenAI 兼容代理服务 + SQLite 持久化。
- 支持两类上游: OpenAI 兼容中转站 API Key, OpenAI Codex/ChatGPT OAuth 账号。
- 本地代理目标是让 Codex 完整可用, 包括 Responses, Chat Completions, `/compact` 变体, 流式响应, 用量统计, OAuth 额度查询和中转站余额查询。
- `vendor/sub2api` 和 `vendor/cc-switch` 仅作为本地参考, 不作为运行依赖, 并将 `/vendor/` 加入 `.gitignore`。

**关键实现**
- 技术栈: `eframe/egui`, `tokio`, `axum`, `reqwest`, `sqlx(sqlite)`, `serde`, `tracing`, `tracing-subscriber`, `uuid`, `chrono`, `anyhow/thiserror`。
- 本地服务默认监听 `127.0.0.1:15721`, GUI 可启动/停止服务, 显示 base URL 和本地访问 key。
- 代理端点覆盖:
  - `POST /v1/responses`, `POST /responses`, `POST /backend-api/codex/responses`
  - 上述 Responses 路径的 `/*subpath`, 包括 `/compact`, `/compact/...`, `/input_tokens` 等
  - `GET /v1/responses`, `GET /responses`, `GET /backend-api/codex/responses` 预留给 Codex Responses WebSocket 模式
  - `POST /v1/chat/completions`, `POST /chat/completions`
  - `GET /v1/models`
- 上游账号模型:
  - `relay_api_key`: `name`, `base_url`, `api_key`, `wire_api`, `supports_compact`, `enabled`, `priority`, `weight`, `balance_provider`
  - `codex_oauth`: device code 登录, refresh token 刷新, `chatgpt_account_id`, `email`, `plan_type`, `enabled`, `supports_compact=true`
- Codex OAuth 转发:
  - 普通 Responses 转发到 `https://chatgpt.com/backend-api/codex/responses`
  - `/compact` 保留为 `https://chatgpt.com/backend-api/codex/responses/compact`
  - 注入 `Authorization`, `chatgpt-account-id`, `openai-beta: codex-1`, `originator`, `Session_Id`, `Version` 等必要头
  - OAuth 请求统一 `store=false`; `/compact` 请求去掉上游不接受的 `store`, `stream`, `prompt_cache_key`
- 中转站 API Key 转发:
  - Responses 原生上游保留 Responses 路径和 subpath
  - Chat Completions 上游将 Responses/compact 请求转换到 `/chat/completions`, 并转换非流式和 SSE 流式响应
  - 保留 Codex 常用请求头, 过滤明显不该透传的本地噪声头
- SQLite 表:
  - `upstreams`: 上游账号和状态
  - `secrets`: 加密后的 API key, refresh token, access token 缓存
  - `request_logs`: 每次请求的模型, endpoint, 上游, token, 状态码, 耗时, 首 token 时间, 错误
  - `usage_rollups`: 总量和按上游/日期聚合
  - `quota_snapshots`: Codex 5h/7d 用量快照
  - `balance_snapshots`: 中转站余额快照
  - `settings`: 监听地址, 本地访问 key, GUI 偏好
- 统计与查询:
  - 从 JSON 响应和 SSE 尾部提取 `input_tokens`, `output_tokens`, cache tokens
  - GUI 展示总请求数/总 token/今日 token/按上游拆分
  - Codex OAuth 主动查询 `https://chatgpt.com/backend-api/wham/usage`, 归一化为 5 小时和 7 天额度
  - 同时从响应头 `x-codex-primary-*`, `x-codex-secondary-*` 被动刷新 5h/7d 快照
  - 中转站余额参考 cc-switch: 内置 DeepSeek, StepFun, SiliconFlow CN/EN, OpenRouter, Novita AI, 未识别时显示不支持并允许后续加适配器
- GUI 页面:
  - 仪表盘: 服务状态, 监听地址, 总用量, 今日用量, 最近错误
  - 上游管理: 添加 API Key 上游, 添加 Codex OAuth 账号, 启停, 排序, 删除, 测试请求
  - 额度/余额: 查询 Codex 5h/7d, 查询中转站余额, 展示最后更新时间
  - 请求日志: 按时间, 上游, 模型, endpoint, 状态筛选

**测试计划**
- 单元测试:
  - Responses 路径归一化, `/compact` subpath 保留
  - Codex 5h/7d 快照归一化
  - Chat Completions 和 Responses usage 提取
  - 余额 provider 检测和解析
- 集成测试:
  - mock 上游验证 `/v1/responses`, `/responses`, `/backend-api/codex/responses`, `/compact` 都能正确转发
  - SSE 流式转发能保留事件并记录 usage
  - OAuth token 刷新和 quota 查询使用 mock HTTP 验证
- 验证命令:
  - `RUSTC_WRAPPER= cargo test`
  - `RUSTC_WRAPPER= cargo clippy`
  - 不运行格式化工具, 除非用户明确要求。

**默认假设**
- 本地代理默认只监听 loopback, 并要求本地访问 key, GUI 会显示给 Codex 配置使用。
- 账号和统计数据全部存 SQLite; secret 默认加密后入库, 若系统 keyring 不可用则在 GUI 明确警告后才允许明文存储。
- v1 不实现公共多用户计费后台, 只做本机 Codex 中转和本机统计。
