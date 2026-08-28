use super::{DiscoveredPeer, merge_discovered, remove_discovered};
use crate::core::models::PeerDiscoverySource;
use crate::peer::identity::NodeIdentity;
use crate::peer::protocol::LND_SERVICE_TYPE;
use anyhow::Context;
use futures_util::StreamExt;
use lnd::{AnnounceHandle, AnnounceSpec, DiscoveryEvent, DiscoveryFilter, LndClient};
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

pub struct LndDiscovery {
    _announce: AnnounceHandle,
    watch_shutdown: watch::Sender<bool>,
}

impl LndDiscovery {
    pub async fn start(
        identity: &NodeIdentity,
        port: u16,
        server_url: &str,
        bearer_token: &str,
        discovery_domain: Option<&str>,
        discovered: Arc<Mutex<Vec<DiscoveredPeer>>>,
        on_change: impl Fn() + Send + Sync + 'static,
    ) -> anyhow::Result<Self> {
        let client = LndClient::builder(server_url)
            .bearer_token(bearer_token)
            .build()
            .context("failed to build lnd client")?;
        let mut spec = AnnounceSpec::new(
            identity.node_id.clone(),
            LND_SERVICE_TYPE,
            identity.display_name.clone(),
            port,
        )
        .insert_metadata("fp", identity.fingerprint())
        .insert_metadata("id", identity.node_id.clone())
        .insert_metadata("name", identity.display_name.clone())
        .insert_metadata("ver", env!("CARGO_PKG_VERSION"));
        if let Some(domain) = discovery_domain.filter(|value| !value.is_empty()) {
            spec = spec.with_discovery_domain(domain);
        }
        let announce = client
            .announce_loop(spec)
            .context("failed to start lnd announce")?;
        let mut filter = DiscoveryFilter::new().with_service(LND_SERVICE_TYPE);
        if let Some(domain) = discovery_domain.filter(|value| !value.is_empty()) {
            filter = filter.with_discovery_domain(domain);
        }
        if let Ok(nodes) = client.list(filter.clone()).await {
            if let Ok(mut items) = discovered.lock() {
                for node in nodes {
                    if let Some(peer) = discovered_from_lnd(&node, &identity.node_id, None) {
                        merge_discovered(&mut items, peer);
                    }
                }
            }
            on_change();
        }
        let (tx, mut rx) = watch::channel(false);
        let local_node_id = identity.node_id.clone();
        let mut watch = client.watch(filter);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = rx.changed() => {
                        if *rx.borrow() {
                            break;
                        }
                    }
                    event = watch.next() => {
                        let Some(event) = event else { break; };
                        let Ok(envelope) = event else { continue; };
                        match envelope.event {
                            DiscoveryEvent::Snapshot { nodes } => {
                                let changed = if let Ok(mut items) = discovered.lock() {
                                    items.retain(|item| item.source != PeerDiscoverySource::Lnd);
                                    for node in nodes {
                                        if let Some(peer) =
                                            discovered_from_lnd(&node, &local_node_id, None)
                                        {
                                            merge_discovered(&mut items, peer);
                                        }
                                    }
                                    true
                                } else {
                                    false
                                };
                                if changed {
                                    on_change();
                                }
                            }
                            DiscoveryEvent::Upsert { node } => {
                                let previous = discovered.lock().ok().and_then(|items| {
                                    items
                                        .iter()
                                        .find(|item| {
                                            item.source == PeerDiscoverySource::Lnd
                                                && item.node_id == node.node_id
                                        })
                                        .cloned()
                                });
                                let changed = discovered_from_lnd(
                                    &node,
                                    &local_node_id,
                                    previous.as_ref(),
                                )
                                .and_then(|peer| {
                                    discovered
                                        .lock()
                                        .ok()
                                        .map(|mut items| merge_discovered(&mut items, peer))
                                })
                                .unwrap_or(false);
                                if changed {
                                    on_change();
                                }
                            }
                            DiscoveryEvent::Remove { node } => {
                                if let Ok(mut items) = discovered.lock() {
                                    remove_discovered(
                                        &mut items,
                                        &node.node_id,
                                        PeerDiscoverySource::Lnd,
                                    );
                                }
                                on_change();
                            }
                            DiscoveryEvent::Reset | DiscoveryEvent::Keepalive => {}
                        }
                    }
                }
            }
        });
        Ok(Self {
            _announce: announce,
            watch_shutdown: tx,
        })
    }

    pub fn stop(self) {
        let _ = self.watch_shutdown.send(true);
    }
}

fn discovered_from_lnd(
    node: &lnd::DiscoveredNode,
    local_node_id: &str,
    previous: Option<&DiscoveredPeer>,
) -> Option<DiscoveredPeer> {
    if node.node_id == local_node_id {
        return None;
    }
    let fingerprint = node.metadata.get("fp")?.clone();
    let display_name = node
        .metadata
        .get("name")
        .cloned()
        .unwrap_or_else(|| node.display_name.clone());
    let addresses = node
        .lan_addrs
        .iter()
        .filter_map(|addr| super::format_peer_https_addr(addr.ip(), addr.port()))
        .collect::<Vec<_>>();
    let addresses = super::prefer_reachable_peer_addresses_cached(
        addresses,
        previous.map(|peer| peer.addresses.as_slice()),
    );
    if addresses.is_empty() {
        return None;
    }
    Some(DiscoveredPeer {
        node_id: node.node_id.clone(),
        fingerprint,
        display_name,
        addresses,
        source: PeerDiscoverySource::Lnd,
    })
}
