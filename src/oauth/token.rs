use crate::app::AppState;
use crate::core::models::Upstream;
use anyhow::{Context, anyhow};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;

const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";
const CODEX_USER_AGENT: &str = "codex-switch-codex-oauth";

#[derive(Debug, Clone, Deserialize)]
pub struct OAuthTokenResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub id_token: Option<String>,
    pub expires_in: Option<i64>,
}

#[derive(Debug, Clone, Default)]
pub struct TokenIdentity {
    pub chatgpt_account_id: Option<String>,
    pub email: Option<String>,
    pub plan_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct IdTokenClaims {
    email: Option<String>,
    #[serde(rename = "https://api.openai.com/auth")]
    openai_auth: Option<OpenAiAuthClaim>,
}

#[derive(Debug, Deserialize)]
struct OpenAiAuthClaim {
    chatgpt_account_id: Option<String>,
    chatgpt_plan_type: Option<String>,
}

pub async fn exchange_code(
    http: &reqwest::Client,
    code: &str,
    code_verifier: &str,
) -> anyhow::Result<OAuthTokenResponse> {
    let response = http
        .post(OAUTH_TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("User-Agent", CODEX_USER_AGENT)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", DEVICE_REDIRECT_URI),
            ("client_id", CODEX_CLIENT_ID),
            ("code_verifier", code_verifier),
        ])
        .send()
        .await
        .context("failed to exchange oauth code")?;
    parse_token_response(response).await
}

pub async fn refresh_token(
    http: &reqwest::Client,
    refresh_token: &str,
) -> anyhow::Result<OAuthTokenResponse> {
    let response = http
        .post(OAUTH_TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("User-Agent", CODEX_USER_AGENT)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", CODEX_CLIENT_ID),
            ("scope", "openid profile email"),
        ])
        .send()
        .await
        .context("failed to refresh oauth token")?;
    parse_token_response(response).await
}

pub async fn valid_access_token(state: &AppState, upstream: &Upstream) -> anyhow::Result<String> {
    let now = chrono::Utc::now().timestamp();
    if upstream.token_expires_at.unwrap_or(0) - now > 60
        && let Some(token) = state.secrets.get(&upstream.id, "access_token").await?
    {
        return Ok(token);
    }
    let refresh = state
        .secrets
        .get(&upstream.id, "refresh_token")
        .await?
        .ok_or_else(|| anyhow!("missing refresh token"))?;
    let tokens = refresh_token(&state.http, &refresh).await?;
    let expires_at = Some(now + tokens.expires_in.unwrap_or(3600));
    state
        .secrets
        .put(&upstream.id, "access_token", &tokens.access_token)
        .await?;
    if let Some(refresh_token) = tokens.refresh_token.as_deref() {
        state
            .secrets
            .put(&upstream.id, "refresh_token", refresh_token)
            .await?;
    }
    state
        .store
        .update_token_expiry(&upstream.id, expires_at)
        .await?;
    Ok(tokens.access_token)
}

pub fn parse_identity(id_token: Option<&str>) -> TokenIdentity {
    let Some(id_token) = id_token else {
        return TokenIdentity::default();
    };
    let Some(payload) = id_token.split('.').nth(1) else {
        return TokenIdentity::default();
    };
    let Ok(bytes) = URL_SAFE_NO_PAD.decode(payload) else {
        return TokenIdentity::default();
    };
    let Ok(claims) = serde_json::from_slice::<IdTokenClaims>(&bytes) else {
        return TokenIdentity::default();
    };
    TokenIdentity {
        chatgpt_account_id: claims
            .openai_auth
            .as_ref()
            .and_then(|auth| auth.chatgpt_account_id.clone()),
        email: claims.email,
        plan_type: claims
            .openai_auth
            .and_then(|auth| auth.chatgpt_plan_type.clone()),
    }
}

async fn parse_token_response(response: reqwest::Response) -> anyhow::Result<OAuthTokenResponse> {
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!("oauth token request failed: {status} {body}"));
    }
    response.json().await.context("invalid oauth token response")
}
