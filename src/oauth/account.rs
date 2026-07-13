use crate::core::models::Upstream;
use crate::oauth::token::{OAuthTokenResponse, parse_identity, token_expiry};
use crate::storage::Store;
use anyhow::anyhow;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone)]
pub struct OAuthAccountInput {
    pub access_token: String,
    pub refresh_token: String,
    pub id_token: Option<String>,
    pub account_id: Option<String>,
    pub token_expires_at: Option<i64>,
}

impl OAuthAccountInput {
    pub fn from_token_response(tokens: OAuthTokenResponse) -> anyhow::Result<Self> {
        let refresh_token = required_value(tokens.refresh_token, "oauth response is missing refresh token")?;
        let access_token = required_value(Some(tokens.access_token), "oauth response is missing access token")?;
        let token_expires_at = Some(
            chrono::Utc::now().timestamp() + tokens.expires_in.unwrap_or(3600),
        );
        Ok(Self {
            access_token,
            refresh_token,
            id_token: non_empty(tokens.id_token),
            account_id: None,
            token_expires_at,
        })
    }

    pub fn imported(
        access_token: String,
        refresh_token: String,
        id_token: Option<String>,
        account_id: Option<String>,
    ) -> anyhow::Result<Self> {
        let access_token = required_value(Some(access_token), "auth.json is missing access token")?;
        let refresh_token = required_value(Some(refresh_token), "auth.json is missing refresh token")?;
        let id_token = non_empty(id_token);
        let account_id = non_empty(account_id);
        let token_expires_at = Some(
            token_expiry(&access_token).unwrap_or_else(|| chrono::Utc::now().timestamp()),
        );
        Ok(Self {
            access_token,
            refresh_token,
            id_token,
            account_id,
            token_expires_at,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthAccountStoreOutcome {
    Created,
    Updated,
}

#[derive(Debug, Clone)]
pub struct OAuthAccountStoreResult {
    pub outcome: OAuthAccountStoreOutcome,
    pub upstream: Upstream,
}

#[derive(Clone)]
pub struct OAuthAccountService {
    store: Store,
    write_lock: Arc<Mutex<()>>,
}

impl OAuthAccountService {
    pub fn new(store: Store) -> Self {
        Self {
            store,
            write_lock: Arc::new(Mutex::new(())),
        }
    }

    pub async fn store_tokens(
        &self,
        input: OAuthAccountInput,
    ) -> anyhow::Result<OAuthAccountStoreResult> {
        let identity = parse_identity(input.id_token.as_deref());
        if let (Some(account_id), Some(claimed_account_id)) = (
            input.account_id.as_deref(),
            identity.chatgpt_account_id.as_deref(),
        ) && account_id != claimed_account_id
        {
            anyhow::bail!("oauth account ID does not match id token claim");
        }
        let account_id = input
            .account_id
            .or(identity.chatgpt_account_id)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| anyhow!("oauth credentials do not contain chatgpt_account_id"))?;
        let name = identity
            .email
            .clone()
            .unwrap_or_else(|| format!("ChatGPT {account_id}"));
        let upstream = Upstream::new_codex_oauth(
            name,
            account_id,
            identity.email,
            identity.plan_type,
            input.token_expires_at,
        );

        let started_at = std::time::Instant::now();
        let _guard = self.write_lock.lock().await;
        let saved = self
            .store
            .save_oauth_account(
                &upstream,
                &input.access_token,
                &input.refresh_token,
                input.id_token.as_deref(),
            )
            .await?;
        tracing::info!(
            account_created = saved.created,
            elapsed_ms = started_at.elapsed().as_millis(),
            "stored codex oauth account"
        );
        Ok(OAuthAccountStoreResult {
            outcome: if saved.created {
                OAuthAccountStoreOutcome::Created
            } else {
                OAuthAccountStoreOutcome::Updated
            },
            upstream: saved.upstream,
        })
    }
}

fn required_value(value: Option<String>, message: &'static str) -> anyhow::Result<String> {
    non_empty(value).ok_or_else(|| anyhow!(message))
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    #[tokio::test]
    async fn stores_distinct_accounts_and_updates_existing_account_in_place() {
        let path = std::env::temp_dir().join(format!(
            "codex-switch-oauth-account-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let store = Store::open(&path).await.unwrap();
        let service = OAuthAccountService::new(store.clone());
        let first = service
            .store_tokens(input(
                "account-one",
                "one@example.com",
                "plus",
                "access-one",
                "refresh-one",
            ))
            .await
            .unwrap();
        assert_eq!(first.outcome, OAuthAccountStoreOutcome::Created);
        let mut customized = first.upstream.clone();
        customized.name = "主要账号".to_string();
        customized.enabled = false;
        customized.priority = 77;
        customized.weight = 9;
        customized.proxy_url = Some("http://127.0.0.1:7890".to_string());
        store.save_upstream(&customized).await.unwrap();

        let updated = service
            .store_tokens(input(
                "account-one",
                "new@example.com",
                "pro",
                "access-new",
                "refresh-new",
            ))
            .await
            .unwrap();
        assert_eq!(updated.outcome, OAuthAccountStoreOutcome::Updated);
        assert_eq!(updated.upstream.id, first.upstream.id);
        assert_eq!(updated.upstream.name, "主要账号");
        assert!(!updated.upstream.enabled);
        assert_eq!(updated.upstream.priority, 77);
        assert_eq!(updated.upstream.weight, 9);
        assert_eq!(
            updated.upstream.proxy_url.as_deref(),
            Some("http://127.0.0.1:7890")
        );
        assert_eq!(updated.upstream.email.as_deref(), Some("new@example.com"));
        assert_eq!(updated.upstream.plan_type.as_deref(), Some("pro"));
        assert_eq!(
            store
                .get_credential(&updated.upstream.id, "access_token")
                .await
                .unwrap()
                .as_deref(),
            Some("access-new")
        );

        let second = service
            .store_tokens(input(
                "account-two",
                "two@example.com",
                "team",
                "access-two",
                "refresh-two",
            ))
            .await
            .unwrap();
        assert_eq!(second.outcome, OAuthAccountStoreOutcome::Created);
        assert_ne!(second.upstream.id, first.upstream.id);
        assert_eq!(store.list_upstreams().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn rejects_credentials_without_account_id() {
        let path = std::env::temp_dir().join(format!(
            "codex-switch-oauth-missing-account-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let store = Store::open(path).await.unwrap();
        let service = OAuthAccountService::new(store);
        let err = service
            .store_tokens(OAuthAccountInput {
                access_token: "access".to_string(),
                refresh_token: "refresh".to_string(),
                id_token: None,
                account_id: None,
                token_expires_at: Some(1_900_000_000),
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("chatgpt_account_id"));
    }

    #[tokio::test]
    async fn rejects_mismatched_account_ids() {
        let path = std::env::temp_dir().join(format!(
            "codex-switch-oauth-mismatch-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let store = Store::open(path).await.unwrap();
        let service = OAuthAccountService::new(store);
        let mut mismatched = input(
            "claimed-account",
            "one@example.com",
            "plus",
            "access",
            "refresh",
        );
        mismatched.account_id = Some("file-account".to_string());
        let err = service.store_tokens(mismatched).await.unwrap_err();
        assert!(err.to_string().contains("does not match"));
    }

    fn input(
        account_id: &str,
        email: &str,
        plan_type: &str,
        access_token: &str,
        refresh_token: &str,
    ) -> OAuthAccountInput {
        OAuthAccountInput {
            access_token: access_token.to_string(),
            refresh_token: refresh_token.to_string(),
            id_token: Some(jwt(&serde_json::json!({
                "email": email,
                "https://api.openai.com/auth": {
                    "chatgpt_account_id": account_id,
                    "chatgpt_plan_type": plan_type
                }
            }))),
            account_id: Some(account_id.to_string()),
            token_expires_at: Some(1_900_000_000),
        }
    }

    fn jwt(payload: &serde_json::Value) -> String {
        format!(
            "header.{}.signature",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(payload).unwrap())
        )
    }
}
