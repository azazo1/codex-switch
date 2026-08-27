use crate::core::models::{NodePeer, PeerDiscoverySource, PeerPairingRequest, Upstream};
use crate::peer::identity::{
    NODE_DISPLAY_NAME_SETTING, NODE_ID_SETTING, NODE_PRIVATE_KEY_SETTING, NodeIdentity,
    default_display_name,
};
use crate::peer::protocol::{DEFAULT_PEER_BIND_ADDR, DEFAULT_PEER_MAX_HOPS, PAIRING_TTL_SECS};
use crate::storage::Store;
use anyhow::Context;
use chrono::{DateTime, Duration, Utc};
use sqlx::Row;

impl Store {
    pub async fn load_or_create_node_identity(&self) -> anyhow::Result<NodeIdentity> {
        let node_id = self.get_setting(NODE_ID_SETTING).await?;
        let private_key = self.get_setting(NODE_PRIVATE_KEY_SETTING).await?;
        let display_name = self
            .get_setting(NODE_DISPLAY_NAME_SETTING)
            .await?
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(default_display_name);
        if let (Some(node_id), Some(private_key)) = (node_id, private_key) {
            return NodeIdentity::from_private_key(node_id, display_name, &private_key);
        }
        let identity = NodeIdentity::generate(display_name)?;
        self.set_setting(NODE_ID_SETTING, &identity.node_id).await?;
        self.set_setting(NODE_DISPLAY_NAME_SETTING, &identity.display_name)
            .await?;
        self.set_setting(NODE_PRIVATE_KEY_SETTING, &identity.private_key_pem()?)
            .await?;
        tracing::info!(
            node_id = %identity.node_id,
            fingerprint = %identity.fingerprint(),
            "generated local node identity"
        );
        Ok(identity)
    }

    pub async fn save_node_display_name(&self, display_name: &str) -> anyhow::Result<()> {
        self.set_setting(NODE_DISPLAY_NAME_SETTING, display_name)
            .await
    }

    pub async fn peer_listen_enabled(&self) -> anyhow::Result<bool> {
        Ok(self
            .get_setting("peer_listen_enabled")
            .await?
            .as_deref()
            == Some("true"))
    }

    pub async fn peer_bind_addr(&self) -> anyhow::Result<String> {
        Ok(self
            .get_setting("peer_bind_addr")
            .await?
            .unwrap_or_else(|| DEFAULT_PEER_BIND_ADDR.to_string()))
    }

    pub async fn peer_max_hops(&self) -> anyhow::Result<i64> {
        Ok(self
            .get_setting("peer_max_hops")
            .await?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(DEFAULT_PEER_MAX_HOPS)
            .max(1))
    }

    pub async fn mdns_discovery_enabled(&self) -> anyhow::Result<bool> {
        Ok(self
            .get_setting("mdns_discovery_enabled")
            .await?
            .as_deref()
            == Some("true"))
    }

    pub async fn lnd_server_url(&self) -> anyhow::Result<Option<String>> {
        Ok(self
            .get_setting("lnd_server_url")
            .await?
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()))
    }

    pub async fn lnd_bearer_token(&self) -> anyhow::Result<Option<String>> {
        Ok(self
            .get_setting("lnd_bearer_token")
            .await?
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()))
    }

    pub async fn lnd_discovery_domain(&self) -> anyhow::Result<Option<String>> {
        Ok(self
            .get_setting("lnd_discovery_domain")
            .await?
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()))
    }

    pub async fn list_node_peers(&self) -> anyhow::Result<Vec<NodePeer>> {
        let rows = sqlx::query("SELECT * FROM node_peers ORDER BY paired_at ASC")
            .fetch_all(self.pool())
            .await?;
        rows.into_iter().map(row_to_node_peer).collect()
    }

    pub async fn get_node_peer(&self, node_id: &str) -> anyhow::Result<Option<NodePeer>> {
        let row = sqlx::query("SELECT * FROM node_peers WHERE node_id = ?1")
            .bind(node_id)
            .fetch_optional(self.pool())
            .await?;
        row.map(row_to_node_peer).transpose()
    }

    pub async fn get_node_peer_by_public_key(
        &self,
        public_key: &str,
    ) -> anyhow::Result<Option<NodePeer>> {
        let row = sqlx::query("SELECT * FROM node_peers WHERE public_key = ?1")
            .bind(public_key)
            .fetch_optional(self.pool())
            .await?;
        row.map(row_to_node_peer).transpose()
    }

    pub async fn get_node_peer_by_upstream(
        &self,
        upstream_id: &str,
    ) -> anyhow::Result<Option<NodePeer>> {
        let row = sqlx::query("SELECT * FROM node_peers WHERE upstream_id = ?1")
            .bind(upstream_id)
            .fetch_optional(self.pool())
            .await?;
        row.map(row_to_node_peer).transpose()
    }

    pub async fn save_node_peer(&self, peer: &NodePeer) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO node_peers (
                node_id, fingerprint, public_key, display_name, addresses, discovery_source,
                upstream_id, paired_at, last_seen_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(node_id) DO UPDATE SET
                fingerprint = excluded.fingerprint,
                public_key = excluded.public_key,
                display_name = excluded.display_name,
                addresses = excluded.addresses,
                discovery_source = excluded.discovery_source,
                upstream_id = excluded.upstream_id,
                last_seen_at = excluded.last_seen_at",
        )
        .bind(&peer.node_id)
        .bind(&peer.fingerprint)
        .bind(&peer.public_key)
        .bind(&peer.display_name)
        .bind(serde_json::to_string(&peer.addresses)?)
        .bind(peer.discovery_source.as_str())
        .bind(&peer.upstream_id)
        .bind(peer.paired_at.to_rfc3339())
        .bind(peer.last_seen_at.map(|value| value.to_rfc3339()))
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn touch_node_peer(
        &self,
        node_id: &str,
        addresses: &[String],
        source: PeerDiscoverySource,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE node_peers
             SET addresses = ?2, discovery_source = ?3, last_seen_at = ?4
             WHERE node_id = ?1",
        )
        .bind(node_id)
        .bind(serde_json::to_string(addresses)?)
        .bind(source.as_str())
        .bind(Utc::now().to_rfc3339())
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn delete_node_peer(&self, node_id: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM node_peers WHERE node_id = ?1")
            .bind(node_id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    pub async fn list_peer_pairing_requests(&self) -> anyhow::Result<Vec<PeerPairingRequest>> {
        self.expire_peer_pairing_requests().await?;
        let rows =
            sqlx::query("SELECT * FROM peer_pairing_requests ORDER BY created_at ASC")
                .fetch_all(self.pool())
                .await?;
        rows.into_iter().map(row_to_pairing_request).collect()
    }

    pub async fn get_peer_pairing_request(
        &self,
        id: &str,
    ) -> anyhow::Result<Option<PeerPairingRequest>> {
        self.expire_peer_pairing_requests().await?;
        let row = sqlx::query("SELECT * FROM peer_pairing_requests WHERE id = ?1")
            .bind(id)
            .fetch_optional(self.pool())
            .await?;
        row.map(row_to_pairing_request).transpose()
    }

    pub async fn get_peer_pairing_request_by_node(
        &self,
        node_id: &str,
    ) -> anyhow::Result<Option<PeerPairingRequest>> {
        self.expire_peer_pairing_requests().await?;
        let row = sqlx::query("SELECT * FROM peer_pairing_requests WHERE node_id = ?1")
            .bind(node_id)
            .fetch_optional(self.pool())
            .await?;
        row.map(row_to_pairing_request).transpose()
    }

    pub async fn save_peer_pairing_request(
        &self,
        request: &PeerPairingRequest,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO peer_pairing_requests (
                id, node_id, fingerprint, public_key, display_name, addresses, created_at, expires_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(node_id) DO UPDATE SET
                id = excluded.id,
                fingerprint = excluded.fingerprint,
                public_key = excluded.public_key,
                display_name = excluded.display_name,
                addresses = excluded.addresses,
                created_at = excluded.created_at,
                expires_at = excluded.expires_at",
        )
        .bind(&request.id)
        .bind(&request.node_id)
        .bind(&request.fingerprint)
        .bind(&request.public_key)
        .bind(&request.display_name)
        .bind(serde_json::to_string(&request.addresses)?)
        .bind(request.created_at.to_rfc3339())
        .bind(request.expires_at.to_rfc3339())
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn delete_peer_pairing_request(&self, id: &str) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM peer_pairing_requests WHERE id = ?1")
            .bind(id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    pub async fn expire_peer_pairing_requests(&self) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM peer_pairing_requests WHERE expires_at <= ?1")
            .bind(Utc::now().to_rfc3339())
            .execute(self.pool())
            .await?;
        Ok(())
    }

    pub async fn upsert_paired_peer(
        &self,
        payload: &crate::peer::protocol::PeerIdentityPayload,
        source: PeerDiscoverySource,
        existing_upstream_id: Option<String>,
    ) -> anyhow::Result<(NodePeer, Upstream, bool)> {
        payload.verify()?;
        self.upsert_verified_peer(payload, source, existing_upstream_id)
            .await
    }

    pub async fn accept_inbound_pairing(
        &self,
        request: &PeerPairingRequest,
    ) -> anyhow::Result<(NodePeer, Upstream, bool)> {
        let payload = crate::peer::protocol::PeerIdentityPayload {
            node_id: request.node_id.clone(),
            display_name: request.display_name.clone(),
            public_key: request.public_key.clone(),
            fingerprint: request.fingerprint.clone(),
            addresses: request.addresses.clone(),
            signature: String::new(),
        };
        let public_key = crate::peer::identity::decode_public_key(&payload.public_key)?;
        let expected = crate::peer::identity::fingerprint_from_public_key(&public_key);
        if expected != payload.fingerprint {
            anyhow::bail!("pairing request fingerprint is invalid");
        }
        self.delete_peer_pairing_request(&request.id).await?;
        self.upsert_verified_peer(&payload, PeerDiscoverySource::Direct, None)
            .await
    }

    async fn upsert_verified_peer(
        &self,
        payload: &crate::peer::protocol::PeerIdentityPayload,
        source: PeerDiscoverySource,
        existing_upstream_id: Option<String>,
    ) -> anyhow::Result<(NodePeer, Upstream, bool)> {
        if let Some(existing) = self.get_node_peer(&payload.node_id).await? {
            let mut peer = existing;
            peer.fingerprint = payload.fingerprint.clone();
            peer.public_key = payload.public_key.clone();
            peer.display_name = payload.display_name.clone();
            if !payload.addresses.is_empty() {
                peer.addresses = payload.addresses.clone();
            }
            peer.discovery_source = source;
            peer.last_seen_at = Some(Utc::now());
            self.save_node_peer(&peer).await?;
            let Some(mut upstream) = self.get_upstream(&peer.upstream_id).await? else {
                anyhow::bail!("paired peer is missing upstream");
            };
            if let Some(address) = peer.addresses.first() {
                upstream.base_url = address.clone();
                upstream.name = payload.display_name.clone();
                self.save_upstream(&upstream).await?;
            }
            return Ok((peer, upstream, false));
        }
        let upstream_id = existing_upstream_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let base_url = payload
            .addresses
            .first()
            .cloned()
            .unwrap_or_default();
        let mut upstream = Upstream::new_peer_node(payload.display_name.clone(), base_url);
        upstream.id = upstream_id;
        self.save_upstream(&upstream).await?;
        let now = Utc::now();
        let peer = NodePeer {
            node_id: payload.node_id.clone(),
            fingerprint: payload.fingerprint.clone(),
            public_key: payload.public_key.clone(),
            display_name: payload.display_name.clone(),
            addresses: payload.addresses.clone(),
            discovery_source: source,
            upstream_id: upstream.id.clone(),
            paired_at: now,
            last_seen_at: Some(now),
        };
        self.save_node_peer(&peer).await?;
        Ok((peer, upstream, true))
    }

    pub fn new_pairing_request(
        payload: &crate::peer::protocol::PeerIdentityPayload,
    ) -> PeerPairingRequest {
        let now = Utc::now();
        PeerPairingRequest {
            id: uuid::Uuid::new_v4().to_string(),
            node_id: payload.node_id.clone(),
            fingerprint: payload.fingerprint.clone(),
            public_key: payload.public_key.clone(),
            display_name: payload.display_name.clone(),
            addresses: payload.addresses.clone(),
            created_at: now,
            expires_at: now + Duration::seconds(PAIRING_TTL_SECS),
        }
    }
}

fn row_to_node_peer(row: sqlx::sqlite::SqliteRow) -> anyhow::Result<NodePeer> {
    let paired_at: String = row.get("paired_at");
    let last_seen_at: Option<String> = row.get("last_seen_at");
    let addresses: String = row.get("addresses");
    Ok(NodePeer {
        node_id: row.get("node_id"),
        fingerprint: row.get("fingerprint"),
        public_key: row.get("public_key"),
        display_name: row.get("display_name"),
        addresses: serde_json::from_str(&addresses).context("invalid peer addresses json")?,
        discovery_source: PeerDiscoverySource::from_str(&row.get::<String, _>("discovery_source")),
        upstream_id: row.get("upstream_id"),
        paired_at: DateTime::parse_from_rfc3339(&paired_at)
            .context("invalid peer paired_at")?
            .with_timezone(&Utc),
        last_seen_at: last_seen_at
            .map(|value| {
                DateTime::parse_from_rfc3339(&value)
                    .context("invalid peer last_seen_at")
                    .map(|value| value.with_timezone(&Utc))
            })
            .transpose()?,
    })
}

fn row_to_pairing_request(row: sqlx::sqlite::SqliteRow) -> anyhow::Result<PeerPairingRequest> {
    let created_at: String = row.get("created_at");
    let expires_at: String = row.get("expires_at");
    let addresses: String = row.get("addresses");
    Ok(PeerPairingRequest {
        id: row.get("id"),
        node_id: row.get("node_id"),
        fingerprint: row.get("fingerprint"),
        public_key: row.get("public_key"),
        display_name: row.get("display_name"),
        addresses: serde_json::from_str(&addresses).context("invalid pairing addresses json")?,
        created_at: DateTime::parse_from_rfc3339(&created_at)
            .context("invalid pairing created_at")?
            .with_timezone(&Utc),
        expires_at: DateTime::parse_from_rfc3339(&expires_at)
            .context("invalid pairing expires_at")?
            .with_timezone(&Utc),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer::protocol::PeerIdentityPayload;
    use crate::storage::Store;

    #[tokio::test]
    async fn creates_and_reloads_node_identity() {
        let path = std::env::temp_dir().join(format!(
            "codex-switch-node-identity-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let store = Store::open(&path).await.unwrap();
        let first = store.load_or_create_node_identity().await.unwrap();
        let second = store.load_or_create_node_identity().await.unwrap();
        assert_eq!(first.node_id, second.node_id);
        assert_eq!(first.fingerprint(), second.fingerprint());
    }

    #[tokio::test]
    async fn pairing_creates_peer_upstream_once() {
        let path = std::env::temp_dir().join(format!(
            "codex-switch-node-peer-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let store = Store::open(&path).await.unwrap();
        let identity = crate::peer::identity::NodeIdentity::generate("peer-a".to_string()).unwrap();
        let payload = PeerIdentityPayload::from_identity(
            &identity,
            vec!["https://192.168.1.8:15722".to_string()],
        );
        let (_, upstream, created) = store
            .upsert_paired_peer(&payload, PeerDiscoverySource::Direct, None)
            .await
            .unwrap();
        assert!(created);
        let (_, saved, created_again) = store
            .upsert_paired_peer(&payload, PeerDiscoverySource::Mdns, None)
            .await
            .unwrap();
        assert!(!created_again);
        assert_eq!(upstream.id, saved.id);
        assert_eq!(saved.kind, crate::core::models::UpstreamKind::PeerNode);
    }
}
