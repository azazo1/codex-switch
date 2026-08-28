use super::{DiscoveredPeer, local_ip_addrs, merge_discovered};
use crate::core::models::PeerDiscoverySource;
use crate::peer::identity::NodeIdentity;
use crate::peer::protocol::MDNS_SERVICE_TYPE;
use anyhow::Context;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub struct MdnsDiscovery {
    daemon: ServiceDaemon,
    stop: Arc<AtomicBool>,
    browse_thread: Option<std::thread::JoinHandle<()>>,
}

impl MdnsDiscovery {
    pub fn start(
        identity: &NodeIdentity,
        port: u16,
        discovered: Arc<Mutex<Vec<DiscoveredPeer>>>,
        on_change: impl Fn() + Send + Sync + 'static,
    ) -> anyhow::Result<Self> {
        let daemon = ServiceDaemon::new().context("failed to start mdns daemon")?;
        let instance = sanitize_mdns_label(&identity.node_id);
        let host = sanitize_mdns_label(&identity.display_name);
        let mut properties = HashMap::new();
        properties.insert("id".to_string(), identity.node_id.clone());
        properties.insert("fp".to_string(), identity.fingerprint());
        properties.insert("name".to_string(), identity.display_name.clone());
        properties.insert("ver".to_string(), env!("CARGO_PKG_VERSION").to_string());
        let addrs = local_ip_addrs()
            .into_iter()
            .filter_map(|ip| match ip {
                IpAddr::V4(ip) => Some(ip.to_string()),
                IpAddr::V6(_) => None,
            })
            .collect::<Vec<_>>();
        tracing::info!(instance, host, port, addrs = ?addrs, "starting mdns discovery");
        let mut info = ServiceInfo::new(
            MDNS_SERVICE_TYPE,
            &instance,
            &format!("{host}.local."),
            addrs.join(",").as_str(),
            port,
            properties,
        )
        .context("failed to build mdns service info")?;
        if addrs.is_empty() {
            info = info.enable_addr_auto();
        }
        daemon
            .register(info)
            .context("failed to register mdns service")?;
        let receiver = daemon
            .browse(MDNS_SERVICE_TYPE)
            .context("failed to browse mdns services")?;
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let local_node_id = identity.node_id.clone();
        let browse_thread = std::thread::Builder::new()
            .name("codex-switch-mdns".to_string())
            .spawn(move || {
                tracing::info!("mdns browse thread started");
                while !stop_thread.load(Ordering::Relaxed) {
                    match receiver.recv_timeout(Duration::from_millis(500)) {
                        Ok(ServiceEvent::ServiceResolved(info)) => {
                            match discovered_from_mdns(&info, &local_node_id) {
                                Some(peer) => {
                                    tracing::info!(
                                        node_id = %peer.node_id,
                                        fingerprint = %peer.fingerprint,
                                        addresses = ?peer.addresses,
                                        "mdns resolved peer"
                                    );
                                    if let Ok(mut items) = discovered.lock() {
                                        merge_discovered(&mut items, peer);
                                    }
                                    on_change();
                                }
                                None => {
                                    tracing::debug!(
                                        fullname = %info.get_fullname(),
                                        "ignored mdns service"
                                    );
                                }
                            }
                        }
                        Ok(ServiceEvent::ServiceRemoved(_, fullname)) => {
                            tracing::info!(fullname, "mdns service removed");
                            if let Ok(mut items) = discovered.lock() {
                                items.retain(|item| {
                                    !(item.source == PeerDiscoverySource::Mdns
                                        && fullname.contains(&item.node_id))
                                });
                            }
                            on_change();
                        }
                        Ok(event) => {
                            tracing::debug!(?event, "mdns browse event");
                        }
                        Err(err) => {
                            if stop_thread.load(Ordering::Relaxed) {
                                break;
                            }
                            let message = err.to_string().to_ascii_lowercase();
                            if message.contains("timed out") || message.contains("timeout") {
                                continue;
                            }
                            tracing::warn!(error = %err, "mdns browse ended");
                            break;
                        }
                    }
                }
            })
            .context("failed to start mdns browse thread")?;
        Ok(Self {
            daemon,
            stop,
            browse_thread: Some(browse_thread),
        })
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Ok(receiver) = self.daemon.shutdown() {
            let _ = receiver.recv_timeout(Duration::from_secs(2));
        }
        if let Some(thread) = self.browse_thread.take() {
            let _ = thread.join();
        }
    }
}

fn discovered_from_mdns(
    info: &mdns_sd::ResolvedService,
    local_node_id: &str,
) -> Option<DiscoveredPeer> {
    let node_id = info.get_property_val_str("id")?.to_string();
    if node_id == local_node_id {
        return None;
    }
    let fingerprint = info.get_property_val_str("fp")?.to_string();
    let display_name = info
        .get_property_val_str("name")
        .unwrap_or(info.get_hostname())
        .to_string();
    let port = info.get_port();
    let addresses = info
        .get_addresses()
        .iter()
        .filter_map(|ip| super::format_peer_https_addr(ip.to_ip_addr(), port))
        .collect::<Vec<_>>();
    let addresses = super::prefer_reachable_peer_addresses(addresses);
    if addresses.is_empty() {
        return None;
    }
    Some(DiscoveredPeer {
        node_id,
        fingerprint,
        display_name,
        addresses,
        source: PeerDiscoverySource::Mdns,
    })
}

fn sanitize_mdns_label(value: &str) -> String {
    let label = value
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>();
    let label = label.trim_matches('-');
    if label.is_empty() {
        "codex-switch".to_string()
    } else {
        label.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer::identity::NodeIdentity;
    use std::time::{Duration, Instant};

    #[test]
    fn two_local_nodes_discover_each_other_over_mdns() {
        let a = NodeIdentity::generate("mdns-a".to_string()).unwrap();
        let b = NodeIdentity::generate("mdns-b".to_string()).unwrap();
        let found_a = Arc::new(Mutex::new(Vec::new()));
        let found_b = Arc::new(Mutex::new(Vec::new()));
        let mdns_a = MdnsDiscovery::start(&a, 15722, found_a.clone(), || {}).unwrap();
        let mdns_b = MdnsDiscovery::start(&b, 15723, found_b.clone(), || {}).unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut a_saw_b = false;
        let mut b_saw_a = false;
        while Instant::now() < deadline && !(a_saw_b && b_saw_a) {
            a_saw_b = found_a
                .lock()
                .unwrap()
                .iter()
                .any(|peer| peer.node_id == b.node_id);
            b_saw_a = found_b
                .lock()
                .unwrap()
                .iter()
                .any(|peer| peer.node_id == a.node_id);
            std::thread::sleep(Duration::from_millis(200));
        }
        mdns_a.stop();
        mdns_b.stop();
        assert!(a_saw_b, "node a did not discover node b over mdns");
        assert!(b_saw_a, "node b did not discover node a over mdns");
    }
}
