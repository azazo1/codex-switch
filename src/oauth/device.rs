use crate::oauth::token::{OAuthTokenResponse, exchange_code};
use anyhow::{Context, anyhow};
use serde::Deserialize;

const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const DEVICE_AUTH_USERCODE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
const DEVICE_AUTH_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
const DEVICE_VERIFICATION_URL: &str = "https://auth.openai.com/codex/device";
const CODEX_USER_AGENT: &str = "codex-switch-codex-oauth";

#[derive(Debug, Clone)]
pub struct DeviceFlow {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub interval: u64,
    pub expires_in: u64,
}

#[derive(Debug, Clone)]
pub enum DevicePollOutcome {
    Pending,
    Authorized(OAuthTokenResponse),
    Expired,
    RetryableError(String),
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_auth_id: String,
    user_code: String,
    interval: Option<serde_json::Value>,
    expires_in: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct DevicePollSuccess {
    authorization_code: String,
    code_verifier: String,
}

pub async fn start_device_flow(http: &reqwest::Client) -> anyhow::Result<DeviceFlow> {
    start_device_flow_at(http, DEVICE_AUTH_USERCODE_URL).await
}

async fn start_device_flow_at(
    http: &reqwest::Client,
    user_code_url: &str,
) -> anyhow::Result<DeviceFlow> {
    tracing::info!("starting codex oauth device flow");
    let response = http
        .post(user_code_url)
        .header("Content-Type", "application/json")
        .header("User-Agent", CODEX_USER_AGENT)
        .json(&serde_json::json!({ "client_id": CODEX_CLIENT_ID }))
        .send()
        .await
        .context("failed to start device flow")?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!("device flow failed: {status} {body}"));
    }
    let body: DeviceCodeResponse = response.json().await.context("invalid device response")?;
    Ok(DeviceFlow {
        device_code: body.device_auth_id,
        user_code: body.user_code,
        verification_uri: DEVICE_VERIFICATION_URL.to_string(),
        interval: parse_interval(body.interval.as_ref()),
        expires_in: body.expires_in.unwrap_or(900),
    })
}

pub async fn poll_device_flow(
    http: &reqwest::Client,
    flow: &DeviceFlow,
) -> anyhow::Result<DevicePollOutcome> {
    poll_device_flow_at(http, flow, DEVICE_AUTH_TOKEN_URL).await
}

async fn poll_device_flow_at(
    http: &reqwest::Client,
    flow: &DeviceFlow,
    token_url: &str,
) -> anyhow::Result<DevicePollOutcome> {
    let response = match http
        .post(token_url)
        .header("Content-Type", "application/json")
        .header("User-Agent", CODEX_USER_AGENT)
        .json(&serde_json::json!({
            "device_auth_id": flow.device_code,
            "user_code": flow.user_code,
        }))
        .send()
        .await
    {
        Ok(response) => response,
        Err(err) => {
            return Ok(DevicePollOutcome::RetryableError(format!(
                "failed to poll device flow: {err}"
            )));
        }
    };
    let status = response.status();
    if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::NOT_FOUND {
        return Ok(DevicePollOutcome::Pending);
    }
    if status == reqwest::StatusCode::GONE {
        return Ok(DevicePollOutcome::Expired);
    }
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
        return Ok(DevicePollOutcome::RetryableError(format!(
            "device poll temporarily failed: {status}"
        )));
    }
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!("device poll failed: {status} {body}"));
    }
    let success: DevicePollSuccess = response.json().await.context("invalid poll response")?;
    let tokens = exchange_code(
        http,
        &success.authorization_code,
        &success.code_verifier,
    )
    .await?;
    Ok(DevicePollOutcome::Authorized(tokens))
}

fn parse_interval(value: Option<&serde_json::Value>) -> u64 {
    match value {
        Some(serde_json::Value::Number(n)) => n.as_u64().unwrap_or(5),
        Some(serde_json::Value::String(s)) => s.parse().unwrap_or(5),
        _ => 5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, http::StatusCode, routing::post};

    #[tokio::test]
    async fn maps_device_start_and_poll_states() {
        let app = Router::new()
            .route(
                "/start",
                post(|| async {
                    Json(serde_json::json!({
                        "device_auth_id": "device-one",
                        "user_code": "CODE-ONE",
                        "interval": "7",
                        "expires_in": 120
                    }))
                }),
            )
            .route("/pending", post(|| async { StatusCode::FORBIDDEN }))
            .route("/expired", post(|| async { StatusCode::GONE }))
            .route("/retry", post(|| async { StatusCode::SERVICE_UNAVAILABLE }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let http = reqwest::Client::new();
        let flow = start_device_flow_at(&http, &format!("http://{address}/start"))
            .await
            .unwrap();
        assert_eq!(flow.device_code, "device-one");
        assert_eq!(flow.user_code, "CODE-ONE");
        assert_eq!(flow.interval, 7);
        assert_eq!(flow.expires_in, 120);
        assert!(matches!(
            poll_device_flow_at(&http, &flow, &format!("http://{address}/pending"))
                .await
                .unwrap(),
            DevicePollOutcome::Pending
        ));
        assert!(matches!(
            poll_device_flow_at(&http, &flow, &format!("http://{address}/expired"))
                .await
                .unwrap(),
            DevicePollOutcome::Expired
        ));
        assert!(matches!(
            poll_device_flow_at(&http, &flow, &format!("http://{address}/retry"))
                .await
                .unwrap(),
            DevicePollOutcome::RetryableError(_)
        ));
        server.abort();
    }
}
