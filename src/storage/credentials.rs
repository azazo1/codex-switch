use crate::storage::Store;

#[derive(Clone)]
pub struct CredentialStore {
    store: Store,
}

impl CredentialStore {
    pub async fn new(store: Store) -> anyhow::Result<Self> {
        Ok(Self { store })
    }

    #[cfg(test)]
    pub fn new_for_tests(store: Store) -> Self {
        Self { store }
    }

    pub async fn put(&self, upstream_id: &str, name: &str, value: &str) -> anyhow::Result<()> {
        self.store.save_credential(upstream_id, name, value).await
    }

    pub async fn get(&self, upstream_id: &str, name: &str) -> anyhow::Result<Option<String>> {
        self.store.get_credential(upstream_id, name).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::models::{BalanceProvider, Upstream, WireApi};

    #[tokio::test]
    async fn stores_credential_as_plaintext() {
        let path = std::env::temp_dir().join(format!(
            "codex-switch-credential-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let store = Store::open(path).await.unwrap();
        let credentials = CredentialStore::new_for_tests(store.clone());
        let upstream = Upstream::new_relay(
            "mock".to_string(),
            "http://127.0.0.1".to_string(),
            WireApi::Responses,
            true,
            BalanceProvider::Unsupported,
        );
        store.save_upstream(&upstream).await.unwrap();

        credentials
            .put(&upstream.id, "api_key", "sk-plain")
            .await
            .unwrap();

        assert_eq!(
            store
                .get_credential(&upstream.id, "api_key")
                .await
                .unwrap()
                .as_deref(),
            Some("sk-plain")
        );
        assert_eq!(
            credentials
                .get(&upstream.id, "api_key")
                .await
                .unwrap()
                .as_deref(),
            Some("sk-plain")
        );
    }
}
