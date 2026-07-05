use axum::{
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::Response,
};
use serde_json::{Value, json};

pub(super) fn build_response(
    status: StatusCode,
    headers: reqwest::header::HeaderMap,
    body: Vec<u8>,
) -> Response {
    let mut builder = Response::builder().status(status);
    for (name, value) in headers {
        if let Some(name) = name
            && should_return_header(name.as_str())
        {
            builder = builder.header(name.as_str(), value.as_bytes());
        }
    }
    builder.body(axum::body::Body::from(body)).unwrap()
}

pub(super) fn to_axum_headers(headers: &reqwest::header::HeaderMap) -> HeaderMap {
    let mut result = HeaderMap::new();
    for (name, value) in headers {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_str().as_bytes()),
            HeaderValue::from_bytes(value.as_bytes()),
        ) {
            result.insert(name, value);
        }
    }
    result
}

pub(super) fn convert_chat_sse_to_responses(text: &str) -> Vec<u8> {
    let mut out = String::new();
    let response_id = format!("resp_{}", uuid::Uuid::new_v4());
    out.push_str(&format!(
        "event: response.created\ndata: {}\n\n",
        json!({"type":"response.created","response":{"id":response_id,"object":"response","status":"in_progress","output":[]}})
    ));
    for block in text.split("\n\n") {
        for line in block.lines() {
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data == "[DONE]" {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(data) else {
                continue;
            };
            if let Some(delta) = value
                .pointer("/choices/0/delta/content")
                .and_then(|v| v.as_str())
                .filter(|v| !v.is_empty())
                .or_else(|| {
                    value
                        .pointer("/choices/0/delta/reasoning_content")
                        .and_then(|v| v.as_str())
                        .filter(|v| !v.is_empty())
                })
            {
                out.push_str(&format!(
                    "event: response.output_text.delta\ndata: {}\n\n",
                    json!({"type":"response.output_text.delta","delta":delta})
                ));
            }
            if value.get("usage").is_some() {
                let usage = crate::usage::chat_to_responses_json(&value)["usage"].clone();
                out.push_str(&format!(
                    "event: response.completed\ndata: {}\n\n",
                    json!({"type":"response.completed","response":{"id":response_id,"status":"completed","usage":usage}})
                ));
            }
        }
    }
    out.push_str("data: [DONE]\n\n");
    out.into_bytes()
}

fn should_return_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "content-type" | "cache-control" | "x-request-id"
    )
}
