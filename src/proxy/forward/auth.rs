use crate::app::AppState;
use crate::peer::identity::{decode_public_key, fingerprint_from_public_key};
use crate::peer::protocol::{HOP_HEADER, append_hop};
use axum::{
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::json;

pub(crate) const PEER_PUBLIC_KEY_HEADER: &str = "x-codex-switch-peer-key";
pub(crate) const PEER_FINGERPRINT_HEADER: &str = "x-codex-switch-peer-fp";

#[derive(Debug, Clone)]
pub(super) enum LocalAccess {
    Primary,
    Temporary { id: String },
    Peer,
}

pub(super) async fn validate_local_access(
    state: &AppState,
    headers: &HeaderMap,
    anthropic_error: bool,
) -> Result<LocalAccess, Response> {
    if let Some(access) = validate_peer_access(state, headers, anthropic_error).await? {
        return Ok(access);
    }
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
        return Ok(LocalAccess::Primary);
    }
    let provided = bearer.or(x_api_key);
    if let Some(provided) = provided
        && let Some(key) = state
            .store
            .find_temporary_access_key(provided)
            .await
            .map_err(|err| {
                let message = format!("failed to read temporary access key: {err}");
                auth_response(
                    anthropic_error,
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &message,
                    "api_error",
                )
            })?
    {
        let now = chrono::Utc::now().timestamp();
        if !key.enabled {
            return Err(auth_response(
                anthropic_error,
                StatusCode::UNAUTHORIZED,
                "temporary access key is disabled",
                "authentication_error",
            ));
        }
        if key.expires_at.is_some_and(|expires_at| expires_at <= now) {
            return Err(auth_response(
                anthropic_error,
                StatusCode::UNAUTHORIZED,
                "temporary access key has expired",
                "authentication_error",
            ));
        }
        if key
            .request_limit
            .is_some_and(|limit| key.requests_used >= limit)
        {
            return Err(auth_response(
                anthropic_error,
                StatusCode::TOO_MANY_REQUESTS,
                "temporary access key request limit reached",
                "rate_limit_error",
            ));
        }
        if key.token_limit.is_some_and(|limit| key.tokens_used >= limit) {
            return Err(auth_response(
                anthropic_error,
                StatusCode::TOO_MANY_REQUESTS,
                "temporary access key token limit reached",
                "rate_limit_error",
            ));
        }
        return Ok(LocalAccess::Temporary { id: key.id });
    }
    if expected.is_empty() {
        return Ok(LocalAccess::Primary);
    }
    Err(auth_response(
        anthropic_error,
        StatusCode::UNAUTHORIZED,
        "invalid local access key",
        "authentication_error",
    ))
}

async fn validate_peer_access(
    state: &AppState,
    headers: &HeaderMap,
    anthropic_error: bool,
) -> Result<Option<LocalAccess>, Response> {
    let Some(public_key) = header_value(headers, PEER_PUBLIC_KEY_HEADER) else {
        return Ok(None);
    };
    let peer = match state.store.get_node_peer_by_public_key(public_key).await {
        Ok(Some(peer)) => peer,
        Ok(None) => {
            return Err(auth_response(
                anthropic_error,
                StatusCode::UNAUTHORIZED,
                "unpaired peer certificate",
                "authentication_error",
            ));
        }
        Err(err) => {
            return Err(auth_response(
                anthropic_error,
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("failed to read paired peer: {err}"),
                "api_error",
            ));
        }
    };
    if let Ok(bytes) = decode_public_key(&peer.public_key) {
        let fingerprint = fingerprint_from_public_key(&bytes);
        if header_value(headers, PEER_FINGERPRINT_HEADER)
            .is_some_and(|value| value != fingerprint)
        {
            return Err(auth_response(
                anthropic_error,
                StatusCode::UNAUTHORIZED,
                "peer fingerprint mismatch",
                "authentication_error",
            ));
        }
    }
    let local_node_id = state.peers.identity().node_id;
    let hops = match append_hop(header_value(headers, HOP_HEADER), &local_node_id) {
        Ok(hops) => hops,
        Err(err) => {
            return Err(auth_response(
                anthropic_error,
                StatusCode::BAD_GATEWAY,
                &err.to_string(),
                "proxy_error",
            ));
        }
    };
    let max_hops = match state.store.peer_max_hops().await {
        Ok(value) => value,
        Err(err) => {
            return Err(auth_response(
                anthropic_error,
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("failed to read peer hop limit: {err}"),
                "api_error",
            ));
        }
    };
    if hops.len() as i64 > max_hops {
        return Err(auth_response(
            anthropic_error,
            StatusCode::BAD_GATEWAY,
            "peer hop limit exceeded",
            "proxy_error",
        ));
    }
    Ok(Some(LocalAccess::Peer))
}

fn header_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

fn auth_response(
    anthropic_error: bool,
    status: StatusCode,
    message: &str,
    error_type: &str,
) -> Response {
    let value = if anthropic_error {
        json!({"type":"error","error":{"message":message,"type":error_type}})
    } else {
        json!({"error":{"message":message,"type":error_type}})
    };
    (status, axum::Json(value)).into_response()
}
