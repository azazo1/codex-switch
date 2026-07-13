# OAuth 使用指南

Codex OAuth 上游使用设备授权流程登录 ChatGPT 账号, 并把 Codex Responses 请求转发到该账号.

## 设备登录

1. 打开 `上游` 页面.
2. 在 `Codex OAuth` 区域点击 `新增登录任务`.
3. 等待界面显示验证地址, 用户码, 轮询间隔和有效期.
4. 手动打开显示的 `https://auth.openai.com/codex/device`.
5. 登录目标账号并输入用户码.
6. 回到 Codex Switch 查看任务状态.

每次点击都会创建独立任务, 因此可以连续生成多个用户码并分别登录不同账号. 应用会按照服务端返回的间隔自动轮询, 也可以点击 `立即检查`. 同一个任务不会同时发起多个轮询请求.

网络错误, 限流和服务端临时错误会保留任务并自动重试. 用户码过期或出现永久错误后, 可以在任务上点击 `重试` 获取新的用户码. 成功, 失败和过期任务会保留到点击 `移除` 或 `清理已结束`. 应用退出后不会恢复未完成任务.

应用不会自动打开浏览器. 授权成功后, 上游列表会新增一个 `codex_oauth` 上游. 名称通常使用账号邮箱, 并保存账号 ID 和套餐信息.

## 导入 Codex CLI 凭据

点击 `导入 auth.json`, 可以通过系统文件选择器一次选择多个 Codex CLI `auth.json` 文件. 应用逐个处理文件并显示进度, 单个文件无效不会阻止其他账号导入.

导入只支持包含 ChatGPT OAuth `tokens` 的当前 Codex CLI 文件格式. 文件必须提供 access token, 以及可从 `tokens.account_id` 或 id token 得到的账号 ID. refresh token 可选. 仅包含 `OPENAI_API_KEY` 的文件不会创建 OAuth 上游.

导入过程不会联网校验, 也不会修改或删除源文件. access token 无法解析到期时间时会按已到期处理. 缺少 refresh token 的账号只能使用到 access token 临近到期, 之后必须重新导入新 token. 界面会把这类结果标记为 `仅 access token`.

设备登录和文件导入都按 `chatgpt_account_id` 识别账号. 再次导入或授权同一账号时, 应用原地更新 token, 邮箱, 套餐和到期时间, 同时保留上游名称, 启用状态, 优先级, 权重, 代理 URL 和调度关系.

## 请求能力

Codex OAuth 上游只处理 Responses 请求:

- 支持普通 Responses 和 compact 子路径.
- 不处理 Chat Completions.
- 不处理 Images.
- 模型列表使用以上游名称构造的占位条目, 不是账号的完整远端模型列表.

转发时会设置 Codex 所需的账号和认证头. 普通请求会强制 `store=false` 并使用流式响应. Compact 请求会移除上游不接受的部分字段.

## Token 刷新

access token 距离过期不足 `60` 秒时, 应用会在存在 refresh token 的情况下自动获取新 token. 新 token 和过期时间会写回 SQLite. 缺少 refresh token 时, 当前 access token 在有效期内仍可使用, 临近到期后请求会失败.

下面的情况会导致请求失败:

- refresh token 缺失或失效.
- 账号授权被撤销.
- token 接口无法访问.
- 保存的账号 ID 不再有效.

遇到无法恢复的刷新错误时, 可以重新完成设备登录或导入新的 `auth.json`. 账号 ID 相同时会更新原上游, 不需要先删除调度配置.

## 代理设置

OAuth 设备登录, token 交换, token 刷新和手动额度查询使用应用的全局 HTTP 客户端, 也就是系统代理设置.

OAuth 上游编辑器中的独立 `代理 URL` 只用于实际 Codex 模型请求, 不会应用到设备登录或 token 刷新.

## Codex 额度

OAuth 上游会显示 `5h` 和 `7d` 已用百分比. 点击 `查 Codex 额度` 可以主动查询. 正常模型响应中的限额头也可能被动更新快照.

当前界面不显示重置时间, 也没有额度不足通知. 查询失败不会清空旧快照, 因此页面中的数值可能来自较早一次成功请求.

## 凭据安全

access token, refresh token 和 id token 当前以明文保存在 SQLite `credentials` 表中. 项目没有系统钥匙串或数据库加密. 导入结果界面不会显示 token 内容, tracing 日志也不会记录 token 或凭据文件内容.

数据库文件等价于登录凭据, 不应上传, 分享或放入未加密的公共同步目录. 备份前请阅读[存储与备份指南](storage-guide.md).
