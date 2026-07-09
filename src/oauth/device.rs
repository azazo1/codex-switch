use crate::app::AppState;
use crate::core::models::Upstream;
use crate::oauth::token::{OAuthTokenResponse, exchange_code, parse_identity};
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
    tracing::info!("starting codex oauth device flow");
    let response = http
        .post(DEVICE_AUTH_USERCODE_URL)
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
    state: &AppState,
    flow: &DeviceFlow,
) -> anyhow::Result<Option<Upstream>> {
    let response = state
        .http
        .post(DEVICE_AUTH_TOKEN_URL)
        .header("Content-Type", "application/json")
        .header("User-Agent", CODEX_USER_AGENT)
        .json(&serde_json::json!({
            "device_auth_id": flow.device_code,
            "user_code": flow.user_code,
        }))
        .send()
        .await
        .context("failed to poll device flow")?;
    let status = response.status();
    if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if status == reqwest::StatusCode::GONE {
        return Err(anyhow!("device code expired"));
    }
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!("device poll failed: {status} {body}"));
    }
    let success: DevicePollSuccess = response.json().await.context("invalid poll response")?;
    let tokens = exchange_code(
        &state.http,
        &success.authorization_code,
        &success.code_verifier,
    )
    .await?;
    store_oauth_account(state, tokens).await.map(Some)
}

async fn store_oauth_account(
    state: &AppState,
    tokens: OAuthTokenResponse,
) -> anyhow::Result<Upstream> {
    let identity = parse_identity(tokens.id_token.as_deref());
    let account_id = identity
        .chatgpt_account_id
        .clone()
        .ok_or_else(|| anyhow!("oauth token does not contain chatgpt_account_id"))?;
    let name = identity
        .email
        .clone()
        .unwrap_or_else(|| format!("ChatGPT {account_id}"));
    let expires_at = Some(chrono::Utc::now().timestamp() + tokens.expires_in.unwrap_or(3600));
    let upstream = Upstream::new_codex_oauth(
        name,
        account_id,
        identity.email,
        identity.plan_type,
        expires_at,
    );
    state.store.save_upstream(&upstream).await?;
    state
        .credentials
        .put(&upstream.id, "access_token", &tokens.access_token)
        .await?;
    if let Some(refresh_token) = tokens.refresh_token {
        state
            .credentials
            .put(&upstream.id, "refresh_token", &refresh_token)
            .await?;
    }
    if let Some(id_token) = tokens.id_token {
        state
            .credentials
            .put(&upstream.id, "id_token", &id_token)
            .await?;
    }
    Ok(upstream)
}

fn parse_interval(value: Option<&serde_json::Value>) -> u64 {
    match value {
        Some(serde_json::Value::Number(n)) => n.as_u64().unwrap_or(5),
        Some(serde_json::Value::String(s)) => s.parse().unwrap_or(5),
        _ => 5,
    }
}
