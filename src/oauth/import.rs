use crate::oauth::{
    OAuthAccountInput, OAuthAccountService, OAuthAccountStoreOutcome, OAuthAccountStoreResult,
};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub enum OAuthFileImportOutcome {
    Created {
        upstream_id: String,
        name: String,
        refreshable: bool,
    },
    Updated {
        upstream_id: String,
        name: String,
        refreshable: bool,
    },
    Failed {
        message: String,
    },
}

#[derive(Debug, Clone)]
pub struct OAuthFileImportItem {
    pub source: PathBuf,
    pub outcome: OAuthFileImportOutcome,
}

#[derive(Debug, Clone)]
pub struct OAuthImportProgress {
    pub processed: usize,
    pub total: usize,
    pub item: OAuthFileImportItem,
}

#[derive(Debug, Clone, Default)]
pub struct OAuthImportBatchResult {
    pub items: Vec<OAuthFileImportItem>,
    pub created: usize,
    pub updated: usize,
    pub failed: usize,
}

pub async fn import_auth_files<F>(
    service: &OAuthAccountService,
    paths: Vec<PathBuf>,
    mut on_progress: F,
) -> OAuthImportBatchResult
where
    F: FnMut(OAuthImportProgress),
{
    let started_at = std::time::Instant::now();
    let total = paths.len();
    tracing::info!(file_count = total, "starting codex oauth file import");
    let mut batch = OAuthImportBatchResult::default();
    for (index, path) in paths.into_iter().enumerate() {
        let item = import_auth_file(service, path).await;
        match item.outcome {
            OAuthFileImportOutcome::Created { .. } => batch.created += 1,
            OAuthFileImportOutcome::Updated { .. } => batch.updated += 1,
            OAuthFileImportOutcome::Failed { .. } => batch.failed += 1,
        }
        batch.items.push(item.clone());
        on_progress(OAuthImportProgress {
            processed: index + 1,
            total,
            item,
        });
    }
    tracing::info!(
        file_count = total,
        created = batch.created,
        updated = batch.updated,
        failed = batch.failed,
        elapsed_ms = started_at.elapsed().as_millis(),
        "completed codex oauth file import"
    );
    batch
}

async fn import_auth_file(
    service: &OAuthAccountService,
    source: PathBuf,
) -> OAuthFileImportItem {
    let result = async {
        let bytes = tokio::fs::read(&source).await?;
        let input = parse_auth_json(&bytes)?;
        service.store_tokens(input).await
    }
    .await;
    let outcome = match result {
        Ok(saved) => saved_outcome(saved),
        Err(err) => {
            tracing::warn!(error = %err, "failed to import codex oauth credential file");
            OAuthFileImportOutcome::Failed {
                message: err.to_string(),
            }
        }
    };
    OAuthFileImportItem { source, outcome }
}

fn saved_outcome(saved: OAuthAccountStoreResult) -> OAuthFileImportOutcome {
    match saved.outcome {
        OAuthAccountStoreOutcome::Created => OAuthFileImportOutcome::Created {
            upstream_id: saved.upstream.id,
            name: saved.upstream.name,
            refreshable: saved.refreshable,
        },
        OAuthAccountStoreOutcome::Updated => OAuthFileImportOutcome::Updated {
            upstream_id: saved.upstream.id,
            name: saved.upstream.name,
            refreshable: saved.refreshable,
        },
    }
}

fn parse_auth_json(bytes: &[u8]) -> anyhow::Result<OAuthAccountInput> {
    let auth: CodexAuthFile = serde_json::from_slice(bytes)?;
    let Some(tokens) = auth.tokens else {
        if auth
            .openai_api_key
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        {
            anyhow::bail!("auth.json contains an API key instead of ChatGPT OAuth tokens");
        }
        anyhow::bail!("auth.json does not contain a tokens object");
    };
    OAuthAccountInput::imported(
        tokens.access_token.unwrap_or_default(),
        tokens.refresh_token,
        tokens.id_token,
        tokens.account_id,
    )
}

#[derive(Debug, Deserialize)]
struct CodexAuthFile {
    #[serde(rename = "OPENAI_API_KEY")]
    openai_api_key: Option<String>,
    tokens: Option<CodexAuthTokens>,
}

#[derive(Debug, Deserialize)]
struct CodexAuthTokens {
    id_token: Option<String>,
    access_token: Option<String>,
    refresh_token: Option<String>,
    account_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    #[test]
    fn parses_current_codex_auth_file() {
        let access_token = jwt(&serde_json::json!({ "exp": 1_900_000_000_i64 }));
        let input = parse_auth_json(
            serde_json::json!({
                "OPENAI_API_KEY": null,
                "tokens": {
                    "id_token": jwt(&serde_json::json!({
                        "email": "one@example.com",
                        "https://api.openai.com/auth": {
                            "chatgpt_account_id": "account-one",
                            "chatgpt_plan_type": "plus"
                        }
                    })),
                    "access_token": access_token,
                    "refresh_token": "refresh-one",
                    "account_id": "account-one"
                }
            })
            .to_string()
            .as_bytes(),
        )
        .unwrap();
        assert_eq!(input.account_id.as_deref(), Some("account-one"));
        assert_eq!(input.refresh_token.as_deref(), Some("refresh-one"));
        assert_eq!(input.token_expires_at, Some(1_900_000_000));
    }

    #[test]
    fn rejects_api_key_only_auth_file() {
        let err = parse_auth_json(br#"{"OPENAI_API_KEY":"sk-test"}"#).unwrap_err();
        assert!(err.to_string().contains("API key"));
    }

    #[test]
    fn accepts_missing_refresh_token() {
        let access_token = jwt(&serde_json::json!({ "exp": 1_900_000_000_i64 }));
        let input = parse_auth_json(
            serde_json::json!({
                "tokens": {
                    "access_token": access_token,
                    "account_id": "account-one"
                }
            })
            .to_string()
            .as_bytes(),
        )
        .unwrap();
        assert!(input.refresh_token.is_none());
        assert_eq!(input.token_expires_at, Some(1_900_000_000));
    }

    #[test]
    fn invalid_access_token_uses_expired_fallback() {
        let before = chrono::Utc::now().timestamp();
        let input = parse_auth_json(
            br#"{"tokens":{"access_token":"not-a-jwt","refresh_token":"refresh","account_id":"account-one"}}"#,
        )
        .unwrap();
        let after = chrono::Utc::now().timestamp();
        assert!(input.token_expires_at.is_some_and(|expiry| expiry >= before && expiry <= after));
    }

    #[test]
    fn rejects_invalid_json() {
        assert!(parse_auth_json(b"not-json").is_err());
    }

    fn jwt(payload: &serde_json::Value) -> String {
        format!(
            "header.{}.signature",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(payload).unwrap())
        )
    }

    #[tokio::test]
    async fn batch_import_continues_after_invalid_file() {
        let dir = std::env::temp_dir().join(format!(
            "codex-switch-oauth-import-{}",
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let valid_path = dir.join("valid.json");
        let invalid_path = dir.join("invalid.json");
        let id_token = jwt(&serde_json::json!({
            "email": "batch@example.com",
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "batch-account",
                "chatgpt_plan_type": "plus"
            }
        }));
        let valid = serde_json::json!({
            "tokens": {
                "id_token": id_token,
                "access_token": jwt(&serde_json::json!({ "exp": 1_900_000_000_i64 })),
                "refresh_token": "batch-refresh",
                "account_id": "batch-account"
            }
        });
        tokio::fs::write(&valid_path, valid.to_string()).await.unwrap();
        tokio::fs::write(&invalid_path, "not-json").await.unwrap();
        let store = crate::storage::Store::open(dir.join("test.sqlite"))
            .await
            .unwrap();
        let service = OAuthAccountService::new(store);
        let mut progress = Vec::new();
        let result = import_auth_files(
            &service,
            vec![valid_path, invalid_path],
            |item| progress.push((item.processed, item.total)),
        )
        .await;
        assert_eq!(result.created, 1);
        assert_eq!(result.updated, 0);
        assert_eq!(result.failed, 1);
        assert_eq!(progress, vec![(1, 2), (2, 2)]);
        tokio::fs::remove_dir_all(dir).await.unwrap();
    }
}
