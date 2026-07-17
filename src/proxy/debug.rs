use std::sync::OnceLock;

const LOG_BODIES_ENV: &str = "CODEX_SWITCH_LOG_BODIES";

pub(crate) fn log_body(stage: &'static str, request_id: &str, target: &str, body: &[u8]) {
    if !body_logging_enabled() {
        return;
    }
    tracing::trace!(
        target: "codex_switch::proxy::body",
        stage,
        request_id,
        target,
        body_bytes = body.len(),
        body = %String::from_utf8_lossy(body),
        "proxy body"
    );
}

fn body_logging_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var(LOG_BODIES_ENV)
            .ok()
            .is_some_and(|value| matches!(value.trim(), "1" | "true" | "yes" | "on"))
    })
}
