# 上游管理指南

上游代表实际接收模型请求的账号或服务. Codex Switch 支持 `Relay API Key` 和 `Codex OAuth` 两类上游.

Codex OAuth 的完整流程见[OAuth 使用指南](oauth-guide.md). 本文重点说明 API Key 上游和通用调度字段.

## 添加 API Key 上游

打开 `上游`, 在 `添加 API Key 上游` 区域填写:

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

选择 Chat Completions 后, 入站 Responses 会进行基础转换. 这不等价于完整的 Responses 实现, 复杂工具调用和多模态请求可能不兼容.

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
