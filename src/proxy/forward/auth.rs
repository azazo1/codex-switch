use crate::app::AppState;
use axum::{
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::json;

pub(super) async fn validate_local_access(
    state: &AppState,
    headers: &HeaderMap,
    anthropic_error: bool,
) -> Result<(), Response> {
    let expected = match state.store.get_setting("local_access_key").await {
        Ok(Some(value)) => value,
        Ok(None) => String::new(),
        Err(err) => {
            let message = format!("failed to read local key: {err}");
            let value = if anthropic_error {
                json!({"type":"error","error":{"message":message,"type":"api_error"}})
            } else {
                json!({"error":{"message":message,"type":"proxy_error"}})
            };
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(value),
            )
                .into_response());
        }
    };
    if expected.is_empty() {
        return Ok(());
    }
    let bearer = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim);
    let x_api_key = headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
        .map(str::trim);
    if bearer == Some(expected.as_str()) || x_api_key == Some(expected.as_str()) {
        return Ok(());
    }
    let value = if anthropic_error {
        json!({"type":"error","error":{"message":"invalid local access key","type":"authentication_error"}})
    } else {
        json!({"error":{"message":"invalid local access key","type":"authentication_error"}})
    };
    Err((StatusCode::UNAUTHORIZED, axum::Json(value)).into_response())
}
