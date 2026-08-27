use serde::{Deserialize, Serialize};

use super::identity::{NodeIdentity, decode_public_key, fingerprint_from_public_key, verify_signature};

pub const HOP_HEADER: &str = "x-codex-switch-hop";

pub const PAIR_MESSAGE_PREFIX: &str = "codex-switch-pair-v1";
pub const DEFAULT_PEER_BIND_ADDR: &str = "0.0.0.0:15722";
pub const DEFAULT_PEER_MAX_HOPS: i64 = 4;
pub const MDNS_SERVICE_TYPE: &str = "_codex-switch._tcp.local.";
pub const LND_SERVICE_TYPE: &str = "_codex-switch._tcp";
pub const PAIRING_TTL_SECS: i64 = 300;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerIdentityPayload {
    pub node_id: String,
    pub display_name: String,
    pub public_key: String,
    pub fingerprint: String,
    pub addresses: Vec<String>,
    pub signature: String,
}

impl PeerIdentityPayload {
    pub fn from_identity(identity: &NodeIdentity, addresses: Vec<String>) -> Self {
        let public_key = identity.public_key_base64();
        let fingerprint = identity.fingerprint();
        let signature = identity.sign(&canonical_pair_message(
            &identity.node_id,
            &public_key,
            &identity.display_name,
        ));
        Self {
            node_id: identity.node_id.clone(),
            display_name: identity.display_name.clone(),
            public_key,
            fingerprint,
            addresses,
            signature,
        }
    }

    pub fn verify(&self) -> anyhow::Result<[u8; 32]> {
        if self.node_id.trim().is_empty() {
            anyhow::bail!("peer node id is empty");
        }
        let public_key = decode_public_key(&self.public_key)?;
        let expected = fingerprint_from_public_key(&public_key);
        if expected != self.fingerprint {
            anyhow::bail!("peer fingerprint does not match public key");
        }
        verify_signature(
            &public_key,
            &canonical_pair_message(&self.node_id, &self.public_key, &self.display_name),
            &self.signature,
        )?;
        Ok(public_key)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairRequest {
    pub identity: PeerIdentityPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PairStatus {
    Pending,
    Accepted,
    Rejected,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairResponse {
    pub status: PairStatus,
    pub request_id: Option<String>,
    pub identity: Option<PeerIdentityPayload>,
    pub message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PeerTlsIdentity {
    pub public_key: [u8; 32],
    pub fingerprint: String,
}

pub fn canonical_pair_message(node_id: &str, public_key: &str, display_name: &str) -> Vec<u8> {
    format!("{PAIR_MESSAGE_PREFIX}|{node_id}|{public_key}|{display_name}").into_bytes()
}

pub fn parse_hops(value: Option<&str>) -> Vec<String> {
    value
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

pub fn encode_hops(hops: &[String]) -> String {
    hops.join(",")
}

pub fn append_hop(existing: Option<&str>, node_id: &str) -> anyhow::Result<Vec<String>> {
    let mut hops = parse_hops(existing);
    if hops.iter().any(|item| item == node_id) {
        anyhow::bail!("peer hop loop detected");
    }
    hops.push(node_id.to_string());
    Ok(hops)
}

pub fn parse_peer_address(value: &str) -> anyhow::Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        anyhow::bail!("peer address is empty");
    }
    if let Ok(url) = url::Url::parse(trimmed)
        && matches!(url.scheme(), "http" | "https")
        && let Some(host) = url.host_str()
    {
        let port = url.port().unwrap_or(15722);
        if host.contains(':') && !host.starts_with('[') {
            return Ok(format!("https://[{host}]:{port}"));
        }
        return Ok(format!("https://{host}:{port}"));
    }
    if trimmed.starts_with('[') {
        return Ok(format!("https://{trimmed}"));
    }
    if trimmed.parse::<std::net::SocketAddr>().is_ok() {
        return Ok(format!("https://{trimmed}"));
    }
    if let Some((host, port)) = trimmed.rsplit_once(':')
        && port.parse::<u16>().is_ok()
    {
        return Ok(format!("https://{host}:{port}"));
    }
    anyhow::bail!("peer address must be host:port or https URL")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer::identity::NodeIdentity;

    #[test]
    fn identity_payload_roundtrip_verifies() {
        let identity = NodeIdentity::generate("box".to_string()).unwrap();
        let payload = PeerIdentityPayload::from_identity(&identity, vec!["192.168.1.8:15722".into()]);
        payload.verify().unwrap();
        assert_eq!(payload.fingerprint, identity.fingerprint());
    }

    #[test]
    fn append_hop_rejects_loops() {
        let hops = append_hop(None, "a").unwrap();
        assert_eq!(hops, vec!["a".to_string()]);
        let hops = append_hop(Some("a"), "b").unwrap();
        assert_eq!(hops, vec!["a".to_string(), "b".to_string()]);
        assert!(append_hop(Some("a,b"), "a").is_err());
    }

    #[test]
    fn parse_peer_address_accepts_host_port_and_url() {
        assert_eq!(
            parse_peer_address("192.168.1.8:15722").unwrap(),
            "https://192.168.1.8:15722"
        );
        assert_eq!(
            parse_peer_address("https://box.local:15722/v1").unwrap(),
            "https://box.local:15722"
        );
    }
}
