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
    fallback_used: bool,
}

impl SecretStore {
    pub async fn new(store: Store) -> anyhow::Result<Self> {
        let (key, fallback_used) = load_or_create_master_key(&store).await?;
        Ok(Self {
            store,
            key,
            fallback_used,
        })
    }

    #[cfg(test)]
    pub fn new_for_tests(store: Store) -> Self {
        Self {
            store,
            key: [7_u8; 32],
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
        Ok(Some(self.decrypt(&value)?))
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

    fn decrypt(&self, value: &str) -> anyhow::Result<String> {
        let Some(rest) = value.strip_prefix("v1:") else {
            return Ok(value.to_string());
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
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&self.key));
        let plaintext = cipher
            .decrypt(Nonce::from_slice(&nonce), ciphertext.as_ref())
            .map_err(|_| anyhow!("failed to decrypt secret"))?;
        String::from_utf8(plaintext).context("secret is not valid utf8")
    }
}

async fn load_or_create_master_key(store: &Store) -> anyhow::Result<([u8; 32], bool)> {
    if let Ok(entry) = keyring::Entry::new("codex-switch", "master-key") {
        match entry.get_password() {
            Ok(value) => {
                if let Ok(key) = decode_key(&value) {
                    return Ok((key, false));
                }
            }
            Err(keyring::Error::NoEntry) => {
                let key = generate_key();
                let encoded = URL_SAFE_NO_PAD.encode(key);
                if entry.set_password(&encoded).is_ok() {
                    return Ok((key, false));
                }
            }
            Err(_) => {}
        }
    }

    if let Some(value) = store.get_setting("fallback_master_key").await? {
        return Ok((decode_key(&value)?, true));
    }

    let key = generate_key();
    store
        .set_setting("fallback_master_key", &URL_SAFE_NO_PAD.encode(key))
        .await?;
    Ok((key, true))
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
