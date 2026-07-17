use crate::core::models::ErrorRetryPolicy;
use axum::http::StatusCode;
use serde_json::Value;

const RETRYABLE_ERROR_CODE: &str = "rate_limit_exceeded";
const TRANSIENT_ERROR_CODES: [&str; 2] = ["server_is_overloaded", "slow_down"];
const TERMINAL_ERROR_CODES: [&str; 8] = [
    "server_is_overloaded",
    "slow_down",
    "context_length_exceeded",
    "insufficient_quota",
    "usage_not_included",
    "cyber_policy",
    "invalid_prompt",
    "bio_policy",
];
const HARD_LIMIT_TYPES: [&str; 2] = ["usage_limit_reached", "usage_not_included"];

pub(super) struct JsonRewriteOutput {
    pub status: StatusCode,
    pub body: Vec<u8>,
}

pub(super) struct SseRewriteOutput {
    pub bytes: Vec<u8>,
    pub rewrite_count: usize,
}

pub(super) struct SseErrorRewriter {
    policy: ErrorRetryPolicy,
    buffer: Vec<u8>,
}

impl SseErrorRewriter {
    pub fn new(policy: ErrorRetryPolicy) -> Self {
        Self {
            policy,
            buffer: Vec::new(),
        }
    }

    pub fn push(&mut self, chunk: &[u8]) -> SseRewriteOutput {
        self.buffer.extend_from_slice(chunk);
        let mut output = SseRewriteOutput {
            bytes: Vec::new(),
            rewrite_count: 0,
        };
        while let Some((index, separator_len)) = find_sse_block_separator(&self.buffer) {
            let block = self.buffer[..index].to_vec();
            let separator = self.buffer[index..index + separator_len].to_vec();
            self.buffer.drain(..index + separator_len);
            append_rewritten_block(&mut output, &block, self.policy);
            output.bytes.extend_from_slice(&separator);
        }
        output
    }

    pub fn finish(&mut self) -> SseRewriteOutput {
        let block = std::mem::take(&mut self.buffer);
        let mut output = SseRewriteOutput {
            bytes: Vec::new(),
            rewrite_count: 0,
        };
        append_rewritten_block(&mut output, &block, self.policy);
        output
    }
}

pub(super) fn rewrite_json_response(
    status: StatusCode,
    body: &[u8],
    policy: ErrorRetryPolicy,
) -> Option<JsonRewriteOutput> {
    if policy == ErrorRetryPolicy::Off {
        return None;
    }
    let mut value = serde_json::from_slice::<Value>(body).ok();
    let error_code = value
        .as_ref()
        .and_then(error_code)
        .map(str::to_string);
    let error_type = value
        .as_ref()
        .and_then(error_type)
        .map(str::to_string);
    let status_rewritten = match status {
        StatusCode::TOO_MANY_REQUESTS => {
            policy == ErrorRetryPolicy::All
                || !is_hard_limit(error_code.as_deref(), error_type.as_deref())
        }
        StatusCode::BAD_REQUEST => policy == ErrorRetryPolicy::All,
        _ => false,
    };
    let body_rewritten = value
        .as_mut()
        .is_some_and(|value| rewrite_error_codes(value, policy));
    if !status_rewritten && !body_rewritten {
        return None;
    }
    let body = if body_rewritten {
        serde_json::to_vec(value.as_ref()?).ok()?
    } else {
        body.to_vec()
    };
    Some(JsonRewriteOutput {
        status: if status_rewritten {
            StatusCode::INTERNAL_SERVER_ERROR
        } else {
            status
        },
        body,
    })
}

fn append_rewritten_block(
    output: &mut SseRewriteOutput,
    block: &[u8],
    policy: ErrorRetryPolicy,
) {
    if let Some(rewritten) = rewrite_sse_block(block, policy) {
        output.bytes.extend_from_slice(&rewritten);
        output.rewrite_count += 1;
    } else {
        output.bytes.extend_from_slice(block);
    }
}

fn rewrite_sse_block(block: &[u8], policy: ErrorRetryPolicy) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(block).ok()?;
    let mut event_type = None;
    let mut data = String::new();
    for line in text.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(value) = line.strip_prefix("event:") {
            event_type = Some(value.trim());
        }
        if let Some(value) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(value.trim_start());
        }
    }
    let mut value = serde_json::from_str::<Value>(&data).ok()?;
    let response_failed = event_type == Some("response.failed")
        || value.get("type").and_then(Value::as_str) == Some("response.failed");
    if !response_failed || !rewrite_error_codes(&mut value, policy) {
        return None;
    }
    let rewritten_data = serde_json::to_string(&value).ok()?;
    let mut rewritten = String::new();
    let mut data_written = false;
    for line in text.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.starts_with("data:") {
            if !data_written {
                rewritten.push_str("data: ");
                rewritten.push_str(&rewritten_data);
                rewritten.push('\n');
                data_written = true;
            }
        } else {
            rewritten.push_str(line);
            rewritten.push('\n');
        }
    }
    rewritten.pop();
    Some(rewritten.into_bytes())
}

fn rewrite_error_codes(value: &mut Value, policy: ErrorRetryPolicy) -> bool {
    let mut rewritten = false;
    for pointer in ["/error/code", "/response/error/code"] {
        let Some(code) = value.pointer_mut(pointer) else {
            continue;
        };
        if code.as_str().is_some_and(|code| match policy {
            ErrorRetryPolicy::Off => false,
            ErrorRetryPolicy::Transient => TRANSIENT_ERROR_CODES.contains(&code),
            ErrorRetryPolicy::All => TERMINAL_ERROR_CODES.contains(&code),
        }) {
            *code = Value::String(RETRYABLE_ERROR_CODE.to_string());
            rewritten = true;
        }
    }
    rewritten
}

fn error_code(value: &Value) -> Option<&str> {
    value
        .pointer("/error/code")
        .or_else(|| value.pointer("/response/error/code"))
        .and_then(Value::as_str)
}

fn error_type(value: &Value) -> Option<&str> {
    value
        .pointer("/error/type")
        .or_else(|| value.pointer("/response/error/type"))
        .and_then(Value::as_str)
}

fn is_hard_limit(code: Option<&str>, error_type: Option<&str>) -> bool {
    code.is_some_and(|code| TERMINAL_ERROR_CODES[2..].contains(&code))
        || error_type.is_some_and(|error_type| HARD_LIMIT_TYPES.contains(&error_type))
}

fn find_sse_block_separator(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer.windows(2).position(|window| window == b"\n\n");
    let crlf = buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n");
    match (lf, crlf) {
        (Some(left), Some(right)) => Some(if left <= right { (left, 2) } else { (right, 4) }),
        (Some(index), None) => Some((index, 2)),
        (None, Some(index)) => Some((index, 4)),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_policy_rewrites_fragmented_overload_event() {
        let event = b"event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"error\":{\"code\":\"server_is_overloaded\",\"message\":\"busy\"}}}\n\n";
        let mut rewriter = SseErrorRewriter::new(ErrorRetryPolicy::Transient);
        let first = rewriter.push(&event[..43]);
        let second = rewriter.push(&event[43..]);

        assert!(first.bytes.is_empty());
        assert_eq!(second.rewrite_count, 1);
        assert_eq!(
            sse_error_code(&second.bytes).as_deref(),
            Some(RETRYABLE_ERROR_CODE)
        );
    }

    #[test]
    fn transient_policy_keeps_terminal_event_unchanged() {
        let event = b"event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"error\":{\"code\":\"context_length_exceeded\"}}}\n\n";
        let mut rewriter = SseErrorRewriter::new(ErrorRetryPolicy::Transient);
        let output = rewriter.push(event);

        assert_eq!(output.rewrite_count, 0);
        assert_eq!(output.bytes, event);
    }

    #[test]
    fn all_policy_rewrites_terminal_event() {
        let event = b"event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"error\":{\"code\":\"context_length_exceeded\"}}}\n\n";
        let mut rewriter = SseErrorRewriter::new(ErrorRetryPolicy::All);
        let output = rewriter.push(event);

        assert_eq!(output.rewrite_count, 1);
        assert_eq!(
            sse_error_code(&output.bytes).as_deref(),
            Some(RETRYABLE_ERROR_CODE)
        );
    }

    #[test]
    fn transient_policy_rewrites_generic_http_rate_limit() {
        let body = br#"{"error":{"code":"rate_limit_exceeded","type":"rate_limit_error"}}"#;
        let output = rewrite_json_response(
            StatusCode::TOO_MANY_REQUESTS,
            body,
            ErrorRetryPolicy::Transient,
        )
        .unwrap();

        assert_eq!(output.status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn transient_policy_keeps_hard_http_limit() {
        let body = br#"{"error":{"code":"insufficient_quota","type":"usage_limit_reached"}}"#;
        let output = rewrite_json_response(
            StatusCode::TOO_MANY_REQUESTS,
            body,
            ErrorRetryPolicy::Transient,
        );

        assert!(output.is_none());
    }

    #[test]
    fn all_policy_rewrites_bad_request_status() {
        let output = rewrite_json_response(
            StatusCode::BAD_REQUEST,
            br#"{"error":{"code":"invalid_prompt"}}"#,
            ErrorRetryPolicy::All,
        )
        .unwrap();

        assert_eq!(output.status, StatusCode::INTERNAL_SERVER_ERROR);
        let value: Value = serde_json::from_slice(&output.body).unwrap();
        assert_eq!(error_code(&value), Some(RETRYABLE_ERROR_CODE));
    }

    fn sse_error_code(bytes: &[u8]) -> Option<String> {
        let text = std::str::from_utf8(bytes).ok()?;
        let data = text
            .lines()
            .find_map(|line| line.strip_prefix("data: "))?;
        let value: Value = serde_json::from_str(data).ok()?;
        error_code(&value).map(str::to_string)
    }
}
