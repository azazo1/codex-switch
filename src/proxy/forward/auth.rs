use crate::app::AppState;
use axum::{
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::json;

pub(super) async fn validate_local_access(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(), Response> {
    let expected = match state.store.get_setting("local_access_key").await {
        Ok(Some(value)) => value,
        Ok(None) => String::new(),
        Err(err) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(json!({"error":{"message":format!("failed to read local key: {err}"),"type":"proxy_error"}})),
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
    if bearer == Some(expected.as_str()) {
        return Ok(());
    }
    Err((
        StatusCode::UNAUTHORIZED,
        axum::Json(json!({"error":{"message":"invalid local access key","type":"authentication_error"}})),
    )
        .into_response())
}
