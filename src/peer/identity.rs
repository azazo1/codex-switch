use anyhow::Context;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::pkcs8::{DecodePrivateKey, EncodePrivateKey};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};

pub const NODE_ID_SETTING: &str = "node_id";
pub const NODE_DISPLAY_NAME_SETTING: &str = "node_display_name";
pub const NODE_PRIVATE_KEY_SETTING: &str = "node_ed25519_private";

#[derive(Clone)]
pub struct NodeIdentity {
    pub node_id: String,
    pub display_name: String,
    signing_key: SigningKey,
}

impl NodeIdentity {
    pub fn generate(display_name: String) -> anyhow::Result<Self> {
        let mut rng = rand::rng();
        let signing_key = SigningKey::generate(&mut rng);
        Ok(Self {
            node_id: uuid::Uuid::new_v4().to_string(),
            display_name,
            signing_key,
        })
    }

    pub fn from_private_key(
        node_id: String,
        display_name: String,
        pkcs8_pem: &str,
    ) -> anyhow::Result<Self> {
        let signing_key = SigningKey::from_pkcs8_pem(pkcs8_pem)
            .context("failed to parse node ed25519 private key")?;
        Ok(Self {
            node_id,
            display_name,
            signing_key,
        })
    }

    pub fn private_key_pem(&self) -> anyhow::Result<String> {
        Ok(self
            .signing_key
            .to_pkcs8_pem(ed25519_dalek::pkcs8::spki::der::pem::LineEnding::LF)
            .context("failed to encode node ed25519 private key")?
            .to_string())
    }

    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }

    pub fn public_key_bytes(&self) -> [u8; 32] {
        self.verifying_key().to_bytes()
    }

    pub fn public_key_base64(&self) -> String {
        STANDARD.encode(self.public_key_bytes())
    }

    pub fn fingerprint(&self) -> String {
        fingerprint_from_public_key(&self.public_key_bytes())
    }

    pub fn sign(&self, message: &[u8]) -> String {
        STANDARD.encode(self.signing_key.sign(message).to_bytes())
    }
}

pub fn base64_public_key(public_key: &[u8; 32]) -> String {
    STANDARD.encode(public_key)
}

pub fn fingerprint_from_public_key(public_key: &[u8]) -> String {
    let digest = Sha256::digest(public_key);
    format!(
        "CS-{:02X}{:02X}-{:02X}{:02X}",
        digest[0], digest[1], digest[2], digest[3]
    )
}

pub fn decode_public_key(value: &str) -> anyhow::Result<[u8; 32]> {
    let bytes = STANDARD
        .decode(value.trim())
        .context("invalid node public key encoding")?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("node public key must be 32 bytes"))?;
    Ok(bytes)
}

pub fn verify_signature(public_key: &[u8; 32], message: &[u8], signature: &str) -> anyhow::Result<()> {
    let verifying_key =
        VerifyingKey::from_bytes(public_key).context("invalid node public key")?;
    let signature_bytes = STANDARD
        .decode(signature.trim())
        .context("invalid node signature encoding")?;
    let signature = Signature::from_slice(&signature_bytes).context("invalid node signature")?;
    verifying_key
        .verify(message, &signature)
        .context("node signature mismatch")?;
    Ok(())
}

pub fn default_display_name() -> String {
    hostname::get()
        .ok()
        .and_then(|value| value.into_string().ok())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "codex-switch".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_stable_for_the_same_public_key() {
        let identity = NodeIdentity::generate("box".to_string()).unwrap();
        let first = identity.fingerprint();
        let second = fingerprint_from_public_key(&identity.public_key_bytes());
        assert_eq!(first, second);
        assert!(first.starts_with("CS-"));
        assert_eq!(first.len(), 12);
    }

    #[test]
    fn generated_identity_can_sign_and_verify() {
        let identity = NodeIdentity::generate("box".to_string()).unwrap();
        let message = b"codex-switch-pair-v1";
        let signature = identity.sign(message);
        verify_signature(&identity.public_key_bytes(), message, &signature).unwrap();
    }
}
