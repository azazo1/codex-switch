use aes_gcm::{
    Aes256Gcm, Key, Nonce,
    aead::{Aead, KeyInit},
};
use anyhow::{Context, anyhow};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngCore;

use crate::storage::Store;

#[derive(Clone)]
pub struct SecretStore {
    store: Store,
    key: [u8; 32],
    legacy_keys: Vec<[u8; 32]>,
    fallback_used: bool,
}

impl SecretStore {
    pub async fn new(store: Store) -> anyhow::Result<Self> {
        let loaded = load_or_create_master_key(&store).await?;
        Ok(Self {
            store,
            key: loaded.key,
            legacy_keys: loaded.legacy_keys,
            fallback_used: loaded.fallback_used,
        })
    }

    #[cfg(test)]
    pub fn new_for_tests(store: Store) -> Self {
        Self {
            store,
            key: [7_u8; 32],
            legacy_keys: Vec::new(),
            fallback_used: true,
        }
    }

    pub fn fallback_used(&self) -> bool {
        self.fallback_used
    }

    pub async fn put(&self, upstream_id: &str, name: &str, value: &str) -> anyhow::Result<()> {
        let encrypted = self.encrypt(value)?;
        self.store.save_secret(upstream_id, name, &encrypted).await
    }

    pub async fn get(&self, upstream_id: &str, name: &str) -> anyhow::Result<Option<String>> {
        let Some(value) = self.store.get_secret(upstream_id, name).await? else {
            return Ok(None);
        };
        let secret = self.decrypt(&value)?;
        if secret.used_legacy_key {
            let encrypted = self.encrypt(&secret.value)?;
            self.store.save_secret(upstream_id, name, &encrypted).await?;
            tracing::info!(upstream_id, name, "migrated secret to active master key");
        }
        Ok(Some(secret.value))
    }

    fn encrypt(&self, value: &str) -> anyhow::Result<String> {
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&self.key));
        let mut nonce_bytes = [0_u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = cipher
            .encrypt(nonce, value.as_bytes())
            .map_err(|_| anyhow!("failed to encrypt secret"))?;
        Ok(format!(
            "v1:{}:{}",
            URL_SAFE_NO_PAD.encode(nonce_bytes),
            URL_SAFE_NO_PAD.encode(ciphertext)
        ))
    }

    fn decrypt(&self, value: &str) -> anyhow::Result<PlainSecret> {
        let Some(rest) = value.strip_prefix("v1:") else {
            return Ok(PlainSecret {
                value: value.to_string(),
                used_legacy_key: false,
            });
        };
        let mut parts = rest.splitn(2, ':');
        let nonce = parts
            .next()
            .ok_or_else(|| anyhow!("secret is missing nonce"))?;
        let ciphertext = parts
            .next()
            .ok_or_else(|| anyhow!("secret is missing ciphertext"))?;
        let nonce = URL_SAFE_NO_PAD
            .decode(nonce)
            .context("failed to decode secret nonce")?;
        let ciphertext = URL_SAFE_NO_PAD
            .decode(ciphertext)
            .context("failed to decode secret ciphertext")?;
        if let Ok(value) = decrypt_with_key(&self.key, &nonce, &ciphertext) {
            return Ok(PlainSecret {
                value,
                used_legacy_key: false,
            });
        }
        for key in &self.legacy_keys {
            if let Ok(value) = decrypt_with_key(key, &nonce, &ciphertext) {
                return Ok(PlainSecret {
                    value,
                    used_legacy_key: true,
                });
            }
        }
        Err(anyhow!("failed to decrypt secret"))
    }
}

struct PlainSecret {
    value: String,
    used_legacy_key: bool,
}

struct LoadedMasterKey {
    key: [u8; 32],
    legacy_keys: Vec<[u8; 32]>,
    fallback_used: bool,
}

async fn load_or_create_master_key(store: &Store) -> anyhow::Result<LoadedMasterKey> {
    let fallback_key = load_fallback_key(store).await?;
    if let Ok(entry) = keyring::Entry::new("codex-switch", "master-key") {
        match entry.get_password() {
            Ok(value) => {
                if let Ok(key) = decode_key(&value) {
                    let legacy_keys = fallback_key
                        .into_iter()
                        .filter(|fallback| fallback != &key)
                        .collect();
                    return Ok(LoadedMasterKey {
                        key,
                        legacy_keys,
                        fallback_used: false,
                    });
                }
                if let Some(key) = fallback_key {
                    sync_keyring(&entry, &key);
                    return Ok(LoadedMasterKey {
                        key,
                        legacy_keys: Vec::new(),
                        fallback_used: false,
                    });
                }
            }
            Err(keyring::Error::NoEntry) => {
                if let Some(key) = fallback_key {
                    if sync_keyring(&entry, &key) {
                        return Ok(LoadedMasterKey {
                            key,
                            legacy_keys: Vec::new(),
                            fallback_used: false,
                        });
                    }
                    return Ok(LoadedMasterKey {
                        key,
                        legacy_keys: Vec::new(),
                        fallback_used: true,
                    });
                }
                let key = generate_key();
                let encoded = URL_SAFE_NO_PAD.encode(key);
                if entry.set_password(&encoded).is_ok() {
                    return Ok(LoadedMasterKey {
                        key,
                        legacy_keys: Vec::new(),
                        fallback_used: false,
                    });
                }
            }
            Err(_) => {
                if let Some(key) = fallback_key {
                    return Ok(LoadedMasterKey {
                        key,
                        legacy_keys: Vec::new(),
                        fallback_used: true,
                    });
                }
            }
        }
    }

    if let Some(key) = fallback_key {
        return Ok(LoadedMasterKey {
            key,
            legacy_keys: Vec::new(),
            fallback_used: true,
        });
    }

    let key = generate_key();
    store
        .set_setting("fallback_master_key", &URL_SAFE_NO_PAD.encode(key))
        .await?;
    Ok(LoadedMasterKey {
        key,
        legacy_keys: Vec::new(),
        fallback_used: true,
    })
}

async fn load_fallback_key(store: &Store) -> anyhow::Result<Option<[u8; 32]>> {
    let Some(value) = store.get_setting("fallback_master_key").await? else {
        return Ok(None);
    };
    match decode_key(&value) {
        Ok(key) => Ok(Some(key)),
        Err(err) => {
            tracing::warn!(error = %err, "ignoring invalid fallback master key");
            Ok(None)
        }
    }
}

fn sync_keyring(entry: &keyring::Entry, key: &[u8; 32]) -> bool {
    entry.set_password(&URL_SAFE_NO_PAD.encode(key)).is_ok()
}

fn decrypt_with_key(key: &[u8; 32], nonce: &[u8], ciphertext: &[u8]) -> anyhow::Result<String> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let plaintext = cipher
        .decrypt(Nonce::from_slice(nonce), ciphertext)
        .map_err(|_| anyhow!("failed to decrypt secret"))?;
    String::from_utf8(plaintext).context("secret is not valid utf8")
}

fn generate_key() -> [u8; 32] {
    let mut key = [0_u8; 32];
    rand::thread_rng().fill_bytes(&mut key);
    key
}

fn decode_key(value: &str) -> anyhow::Result<[u8; 32]> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .context("failed to decode master key")?;
    let key: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow!("master key length is invalid"))?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn decrypts_secret_with_legacy_key() {
        let path = std::env::temp_dir()
            .join(format!("codex-switch-secret-{}.sqlite", uuid::Uuid::new_v4()));
        let store = Store::open(path).await.unwrap();
        let old_store = SecretStore {
            store: store.clone(),
            key: [1_u8; 32],
            legacy_keys: Vec::new(),
            fallback_used: true,
        };
        let current_store = SecretStore {
            store,
            key: [2_u8; 32],
            legacy_keys: vec![[1_u8; 32]],
            fallback_used: false,
        };

        let encrypted = old_store.encrypt("sk-legacy").unwrap();
        let secret = current_store.decrypt(&encrypted).unwrap();

        assert_eq!(secret.value, "sk-legacy");
        assert!(secret.used_legacy_key);
    }
}
