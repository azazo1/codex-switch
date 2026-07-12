# 存储与备份指南

Codex Switch 使用一个本地 SQLite 数据库保存设置和历史数据. 实际路径以仪表盘 `SQLite 数据库` 区域显示的值为准.

## 常见路径

不同系统的典型数据库路径如下:

| 系统 | 路径 |
| --- | --- |
| Windows | `%APPDATA%\codex-switch\data\codex-switch.sqlite` |
| macOS | `$HOME/Library/Application Support/codex-switch/codex-switch.sqlite` |
| Linux | `${XDG_DATA_HOME:-$HOME/.local/share}/codex-switch/codex-switch.sqlite` |

系统目录规则可能受运行环境影响. 不要仅根据表格猜测, 应优先使用仪表盘的 `打开位置`.

## 保存的内容

数据库包含:

- 上游信息和启用状态.
- Relay API Key, OAuth token 和 NewApi 凭据.
- 监听地址, 本地访问 key 和当前调度组.
- 调度组, 成员关系和模型路由规则.
- 请求日志和用量汇总.
- Codex 额度和 Relay 余额快照.
- 模型价格缓存.
- 缓存保持和余额提醒设置.

缓存保持的运行时会话和调度器的亲和, 轮询, 失败状态只在内存中, 重启后不会恢复.

## 凭据风险

Relay, OAuth 和 NewApi 上游凭据当前以明文保存在 SQLite `credentials` 表中. 本地访问 key 也以明文保存在 `settings` 表中. 整个数据库都没有加密, 系统钥匙串或单独的主密码.

数据库文件和它的备份应按密钥文件保护:

- 不要提交到 Git.
- 不要上传到公开网盘或 Issue.
- 共享诊断信息前不要直接发送整个数据库.
- 设备丢失或数据库泄露后, 应轮换 Relay API Key 并撤销 OAuth 授权.

## 备份

推荐备份整个数据目录:

1. 使用系统托盘菜单 `退出`, 确认 Codex Switch 进程已经结束.
2. 复制数据库所在的整个目录到受保护位置.
3. 保留主数据库以及可能存在的 `-wal` 和 `-shm` 文件.

退出后再复制可以避免遗漏尚未合并的 WAL 数据. 当前没有内置备份, 导出或导入功能.

## 恢复

1. 确认 Codex Switch 已退出.
2. 备份目标机器上已有的数据目录.
3. 用完整备份替换目标数据目录.
4. 启动应用并检查上游, 调度组和日志.

应用启动时会自动执行向前 schema migration. 当前没有降级 migration, 使用较新版本打开数据库后, 不保证旧版本还能读取.

## 空间管理

请求日志不会自动过期. 使用日志页面的清理功能删除旧记录, 并同步重建用量汇总.

SQLite 删除记录后通常只增加空闲页面, 不会自动缩小主文件. 当前界面没有 `VACUUM` 操作.

Windows 的 `codex-switch.log` 位于同一数据目录并持续追加, 当前也没有自动轮转或清理.
