use crate::logging;

pub(crate) fn log_body(stage: &'static str, request_id: &str, target: &str, body: &[u8]) {
    if !logging::body_logging_enabled() {
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
