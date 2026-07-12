# OAuth 使用指南

Codex OAuth 上游使用设备授权流程登录 ChatGPT 账号, 并把 Codex Responses 请求转发到该账号.

## 添加账号

1. 打开 `上游` 页面.
2. 在 `Codex OAuth` 区域点击 `开始登录`.
3. 等待界面显示验证地址, 用户码, 轮询间隔和有效期.
4. 手动打开显示的 `https://auth.openai.com/codex/device`.
5. 登录目标账号并输入用户码.
6. 回到 Codex Switch, 点击 `轮询授权`.

应用当前不会自动打开浏览器, 也不会按照界面中的间隔自动轮询. 如果授权尚未完成, 状态栏会提示继续等待. 设备码过期后需要重新点击 `开始登录`.

授权成功后, 上游列表会新增一个 `codex_oauth` 上游. 名称通常使用账号邮箱, 并保存账号 ID 和套餐信息.

## 请求能力

Codex OAuth 上游只处理 Responses 请求:

- 支持普通 Responses 和 compact 子路径.
- 不处理 Chat Completions.
- 不处理 Images.
- 模型列表使用以上游名称构造的占位条目, 不是账号的完整远端模型列表.

转发时会设置 Codex 所需的账号和认证头. 普通请求会强制 `store=false` 并使用流式响应. Compact 请求会移除上游不接受的部分字段.

## Token 刷新

access token 距离过期不足 `60` 秒时, 应用会自动使用 refresh token 获取新 token. 新 token 和过期时间会写回 SQLite.

下面的情况会导致请求失败:

- refresh token 缺失或失效.
- 账号授权被撤销.
- token 接口无法访问.
- 保存的账号 ID 不再有效.

当前没有重新授权现有上游的按钮. 遇到无法恢复的刷新错误时, 删除该 OAuth 上游并重新完成设备登录.

## 代理设置

OAuth 设备登录, token 交换, token 刷新和手动额度查询使用应用的全局 HTTP 客户端, 也就是系统代理设置.

OAuth 上游编辑器中的独立 `代理 URL` 只用于实际 Codex 模型请求, 不会应用到设备登录或 token 刷新.

## Codex 额度

OAuth 上游会显示 `5h` 和 `7d` 已用百分比. 点击 `查 Codex 额度` 可以主动查询. 正常模型响应中的限额头也可能被动更新快照.

当前界面不显示重置时间, 也没有额度不足通知. 查询失败不会清空旧快照, 因此页面中的数值可能来自较早一次成功请求.

## 凭据安全

access token, refresh token 和 id token 当前以明文保存在 SQLite `credentials` 表中. 项目没有系统钥匙串或数据库加密.

数据库文件等价于登录凭据, 不应上传, 分享或放入未加密的公共同步目录. 备份前请阅读[存储与备份指南](storage-guide.md).
