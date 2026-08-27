use super::discovery::{DiscoveredPeer, lnd::LndDiscovery, mdns::MdnsDiscovery};
use super::identity::NodeIdentity;
use super::protocol::{PeerIdentityPayload, parse_peer_address};
use crate::app::AppEvents;
use crate::core::models::NodePeer;
use crate::storage::Store;
use anyhow::Context;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct PeerRuntime {
    identity: NodeIdentity,
    discovered: Arc<Mutex<Vec<DiscoveredPeer>>>,
    mdns: Arc<Mutex<Option<MdnsDiscovery>>>,
    lnd: Arc<Mutex<Option<LndDiscovery>>>,
}

impl PeerRuntime {
    pub async fn new(store: &Store) -> anyhow::Result<Self> {
        let identity = store.load_or_create_node_identity().await?;
        Ok(Self {
            identity,
            discovered: Arc::new(Mutex::new(Vec::new())),
            mdns: Arc::new(Mutex::new(None)),
            lnd: Arc::new(Mutex::new(None)),
        })
    }

    pub fn identity(&self) -> NodeIdentity {
        self.identity.clone()
    }

    pub fn discovered_peers(&self) -> Vec<DiscoveredPeer> {
        self.discovered
            .lock()
            .map(|items| items.clone())
            .unwrap_or_default()
    }

    pub async fn start_discovery(&self, store: &Store, events: AppEvents) -> anyhow::Result<()> {
        self.stop_discovery();
        let bind_addr = store.peer_bind_addr().await?;
        let port = bind_addr
            .parse::<std::net::SocketAddr>()
            .map(|addr| addr.port())
            .unwrap_or(15722);
        if store.mdns_discovery_enabled().await? {
            let discovered = self.discovered.clone();
            let events = events.clone();
            let mdns = MdnsDiscovery::start(&self.identity, port, discovered, move || {
                events.bump_peers();
            })?;
            if let Ok(mut slot) = self.mdns.lock() {
                *slot = Some(mdns);
            }
        }
        if let (Some(server_url), Some(token)) =
            (store.lnd_server_url().await?, store.lnd_bearer_token().await?)
        {
            let domain = store.lnd_discovery_domain().await?;
            let discovered = self.discovered.clone();
            let events = events.clone();
            match LndDiscovery::start(
                &self.identity,
                port,
                &server_url,
                &token,
                domain.as_deref(),
                discovered,
                move || events.bump_peers(),
            )
            .await
            {
                Ok(lnd) => {
                    if let Ok(mut slot) = self.lnd.lock() {
                        *slot = Some(lnd);
                    }
                }
                Err(err) => tracing::warn!(error = %err, "failed to start lnd discovery"),
            }
        }
        Ok(())
    }

    pub fn stop_discovery(&self) {
        if let Ok(mut slot) = self.mdns.lock()
            && let Some(mdns) = slot.take()
        {
            mdns.stop();
        }
        if let Ok(mut slot) = self.lnd.lock()
            && let Some(lnd) = slot.take()
        {
            lnd.stop();
        }
        if let Ok(mut items) = self.discovered.lock() {
            items.clear();
        }
    }

    pub async fn pair_direct(
        &self,
        store: &Store,
        address: &str,
        expected_fingerprint: Option<&str>,
    ) -> anyhow::Result<NodePeer> {
        let base_url = parse_peer_address(address)?;
        let client = super::client::tofu_client(&self.identity)?;
        let bind_addr = store.peer_bind_addr().await?;
        let port = bind_addr
            .parse::<std::net::SocketAddr>()
            .map(|addr| addr.port())
            .unwrap_or(15722);
        let local = PeerIdentityPayload::from_identity(
            &self.identity,
            super::discovery::local_peer_addresses(port),
        );
        let remote = super::client::poll_pair_until_resolved(&client, &base_url, local).await?;
        if let Some(expected) = expected_fingerprint
            && remote.fingerprint != expected
        {
            anyhow::bail!("peer fingerprint does not match the expected value");
        }
        let mut payload = remote;
        if payload.addresses.is_empty() {
            payload.addresses = vec![base_url];
        }
        let (peer, _, _) = store
            .upsert_paired_peer(&payload, crate::core::models::PeerDiscoverySource::Direct, None)
            .await?;
        Ok(peer)
    }

    pub async fn pair_discovered(
        &self,
        store: &Store,
        discovered: &DiscoveredPeer,
    ) -> anyhow::Result<NodePeer> {
        let address = discovered
            .addresses
            .first()
            .cloned()
            .context("discovered peer has no address")?;
        let source = discovered.source;
        let mut peer = self
            .pair_direct(store, &address, Some(&discovered.fingerprint))
            .await?;
        peer.discovery_source = source;
        peer.addresses = discovered.addresses.clone();
        store.save_node_peer(&peer).await?;
        store
            .touch_node_peer(&peer.node_id, &peer.addresses, source)
            .await?;
        Ok(peer)
    }

    pub async fn accept_pairing_request(
        &self,
        store: &Store,
        request_id: &str,
    ) -> anyhow::Result<NodePeer> {
        let request = store
            .get_peer_pairing_request(request_id)
            .await?
            .context("pairing request not found")?;
        let (peer, _, _) = store.accept_inbound_pairing(&request).await?;
        Ok(peer)
    }

    pub async fn reject_pairing_request(&self, store: &Store, request_id: &str) -> anyhow::Result<()> {
        store.delete_peer_pairing_request(request_id).await
    }
}
