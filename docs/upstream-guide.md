# 上游管理指南

上游代表实际接收模型请求的账号或服务. Codex Switch 支持 `Relay API Key` 和 `Codex OAuth` 两类上游. 只要服务提供 OpenAI Responses 或 Chat Completions 兼容接口, 就可以作为 API Key 上游接入.

Codex OAuth 的完整流程见[OAuth 使用指南](oauth-guide.md). 本文重点说明 API Key 上游和通用调度字段.

## 添加 OpenAI 兼容上游

打开 `上游`, 在 `添加 OpenAI 兼容上游` 区域填写:

| 字段 | 说明 |
| --- | --- |
| 名称 | 本地展示名称, 必填 |
| Base URL | 中转站或供应商地址, 必填 |
| 代理 URL | 该上游单独使用的代理, 留空使用系统代理 |
| API Key | 上游认证密钥, 必填 |
| Wire API | 上游原生使用 Responses 或 Chat Completions |
| 支持 compact | 该上游能否处理 Responses compact 请求 |

Base URL 可以填写站点根地址或以 `/v1` 结尾的地址. 构建上游端点时, Codex Switch 会在需要时补上 `/v1`.

代理 URL 会应用到该上游的模型请求和余额查询. 填写无效 URL 时无法保存. 留空时使用系统代理设置.

## 选择 Wire API

优先按照上游真实能力选择:

- 原生支持 `/responses` 时选择 `Responses`.
- 只支持 `/chat/completions` 时选择 `Chat Completions`.

选择 Chat Completions 后, 入站 Responses 会转换为标准 Chat Completions 请求. provider 私有工具, 私有 reasoning 参数和非 OpenAI 内容类型不会自动转换.

当前转换覆盖 Codex 常用的以下语义:

- `instructions`, system 消息和多轮文本消息.
- Responses 根级 `tools`, Codex `additional_tools`, function tools, custom tools, namespace tools, tool search, tool call 和 tool output.
- Chat Completions 的文本, `tool_calls`, `reasoning_content`, `reasoning` 和 `reasoning_details` 输出.
- SSE 文本增量, 工具参数增量, 输出项生命周期和 usage.
- OpenAI 格式的文本加 `image_url` 内容. 上游模型本身仍需支持视觉输入.

custom tool 会包装成一个接收 `input` 字符串的标准 function tool. 上游返回调用后, Codex Switch 会恢复成 Responses custom tool call.

namespace tool 会展开成 `namespace__name` 形式的 Chat function. 上游返回调用后, Codex Switch 会恢复原始 `namespace` 和 `name`. Chat function 名称超过 64 字节时会使用稳定哈希缩短, 回程语义不变.

## 接入国产模型

大多数国产模型平台同时提供 OpenAI Chat Completions 兼容地址. 接入时使用下面的通用配置:

| 字段 | 建议值 |
| --- | --- |
| Base URL | 服务商给出的 OpenAI 兼容根地址, 可带或不带 `/v1` |
| API Key | 服务商创建的 API Key |
| Wire API | `Chat Completions` |
| 支持 compact | 先关闭, 确认长上下文压缩可用后再开启 |
| 余额 provider | 已知平台可自动识别, 其他平台可选 `unsupported` |

DeepSeek, Qwen, GLM, Kimi, MiniMax 等品牌本身不需要写入程序. 实际兼容性取决于所选服务地址是否实现以下协议能力:

- Bearer API Key 认证.
- `POST /chat/completions`.
- OpenAI 格式的 `messages` 和 `tools`.
- agent 场景需要标准 `tool_calls` 和流式 SSE.

服务商使用私有 JSON 结构, Anthropic Messages, WebSocket 专用接口或非标准签名认证时, 不能直接作为 Chat Completions 上游. 这类服务需要新增独立 Wire API 适配器.

### 模型名

Codex 发来的模型名必须是上游认识的模型 ID. 可以选择下面任一方式:

- 启动 Codex 时直接选择上游模型 ID.
- 在模型映射调度组中增加规则, 将 Codex 模型模式改写为上游模型 ID.

例如, 可以把 `gpt-*` 映射到某个 coder 模型, 也可以用 `*` 将该调度组的所有模型请求固定改写到一个模型. 模型名和版本应以服务商的 `/models` 返回或当前文档为准.

## 编辑上游

在上游列表点击 `编辑`, 可以修改:

- 名称和启用状态.
- 优先级和权重.
- 独立代理 URL.
- Base URL, Wire API 和 compact 能力.
- API Key, 余额 provider, 余额提醒和缓存保持.

编辑时 API Key 留空表示保留旧值, 不会清空已有密钥. 权重最小为 `1`.

禁用上游后, 新请求不会再把它作为候选. 正在执行的请求不会因此自动终止.

## 优先级和权重

上游列表按优先级从高到低参与候选排序.

- 失败切换优先尝试排序靠前的健康上游.
- 随机模式按权重加权随机.
- 轮询模式按权重加权轮询.
- 权重不改变固定模式或模型映射直达上游的结果.

上游优先级和权重只有在当前调度组能访问该上游时才有意义.

## 查询余额

余额查询只支持 Relay API Key 上游. `余额 provider` 可选择:

| Provider | 说明 |
| --- | --- |
| `auto` | 按 Base URL 识别已知平台, 未识别时尝试通用面板接口 |
| `deepseek` | DeepSeek 官方余额 |
| `stepfun` | StepFun 官方余额 |
| `siliconflow_cn` | SiliconFlow 中国站 |
| `siliconflow_global` | SiliconFlow 全球站 |
| `openrouter` | OpenRouter credits |
| `novita` | Novita AI 余额 |
| `sub2api` | Sub2Api 面板 |
| `newapi` | NewApi 或 One API 面板 |
| `unsupported` | 明确标记为不支持, 点击查询会返回错误 |

已知官方 provider 使用固定余额端点, 不一定使用填写的 Base URL. 更换服务地址后应重新确认 provider.

显式选择 `newapi`, 或 `auto` 根据 Base URL 识别为 NewApi 时, `NewApi 用户 Key` 和 `NewApi 用户 ID` 都是必填项. 新增表单不显示这两个字段, 应先添加上游, 再进入编辑器补齐. 这两个字段只用于余额接口, 不替代模型请求使用的 API Key.

保存后, 可以先在上游列表点击 `查余额`. 成功结果会写入 SQLite 快照. Provider 返回了无效快照时, 失败详情可在余额状态的悬浮提示中查看. 缺凭据, 不支持 provider 或传输错误通常只显示在状态栏, 并保留旧快照.

系统提醒配置见[余额提醒指南](balance-alert-guide.md).

## 删除上游

删除操作不可撤销. 它会删除上游记录, 对应凭据, 缓存保持设置, 余额提醒设置, 调度组成员关系, 以及直接指向该上游的模型路由规则.

历史请求日志仍可能保留上游名称或 ID. 删除前应检查当前调度组, 避免入口组失去所有可用候选.

## 凭据安全

API Key 和 NewApi 凭据不会在编辑器中回显. 但是它们当前以明文写入本地 SQLite, 没有系统钥匙串或数据库加密.

不要共享数据库文件或未加密备份. 备份方式见[存储与备份指南](storage-guide.md).
