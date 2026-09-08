// 版本号由发布构建路径 (just dist 与 CI) 通过 CODEX_SWITCH_BUILD_VERSION 注入,
// 格式在注入前已按 tag, 短 commit 和脏工作区状态生成, 这里只负责展示.
const BUILD_VERSION: &str = env!("CODEX_SWITCH_BUILD_VERSION");

pub fn display_version() -> &'static str {
    BUILD_VERSION
}
