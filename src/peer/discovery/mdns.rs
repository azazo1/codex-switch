use super::{DiscoveredPeer, local_ip_addrs, merge_discovered};
use crate::core::models::PeerDiscoverySource;
use crate::peer::identity::NodeIdentity;
use crate::peer::protocol::MDNS_SERVICE_TYPE;
use anyhow::Context;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;

pub struct MdnsDiscovery {
    daemon: ServiceDaemon,
    browse_shutdown: watch::Sender<bool>,
}

impl MdnsDiscovery {
    pub fn start(
        identity: &NodeIdentity,
        port: u16,
        discovered: Arc<Mutex<Vec<DiscoveredPeer>>>,
        on_change: impl Fn() + Send + Sync + 'static,
    ) -> anyhow::Result<Self> {
        let daemon = ServiceDaemon::new().context("failed to start mdns daemon")?;
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
        let info = ServiceInfo::new(
            MDNS_SERVICE_TYPE,
            &host,
            &format!("{host}.local."),
            addrs.join(",").as_str(),
            port,
            properties,
        )
        .context("failed to build mdns service info")?;
        daemon
            .register(info)
            .context("failed to register mdns service")?;
        let receiver = daemon
            .browse(MDNS_SERVICE_TYPE)
            .context("failed to browse mdns services")?;
        let (tx, rx) = watch::channel(false);
        let local_node_id = identity.node_id.clone();
        tokio::task::spawn_blocking(move || {
            loop {
                if *rx.borrow() {
                    break;
                }
                match receiver.recv_timeout(Duration::from_millis(500)) {
                    Ok(ServiceEvent::ServiceResolved(info)) => {
                        if let Some(peer) = discovered_from_mdns(&info, &local_node_id) {
                            if let Ok(mut items) = discovered.lock() {
                                merge_discovered(&mut items, peer);
                            }
                            on_change();
                        }
                    }
                    Ok(ServiceEvent::ServiceRemoved(_, fullname)) => {
                        if let Ok(mut items) = discovered.lock() {
                            items.retain(|item| {
                                !(item.source == PeerDiscoverySource::Mdns
                                    && fullname.contains(&item.node_id))
                            });
                        }
                        on_change();
                    }
                    Ok(_) => {}
                    Err(err) => {
                        if err.to_string().to_ascii_lowercase().contains("timeout") {
                            continue;
                        }
                        tracing::debug!(error = %err, "mdns browse ended");
                        break;
                    }
                }
            }
        });
        Ok(Self {
            daemon,
            browse_shutdown: tx,
        })
    }

    pub fn stop(self) {
        let _ = self.browse_shutdown.send(true);
        if let Ok(receiver) = self.daemon.shutdown() {
            let _ = receiver.recv_timeout(Duration::from_secs(2));
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
        .map(|ip| match ip.to_ip_addr() {
            IpAddr::V4(ip) => format!("https://{ip}:{port}"),
            IpAddr::V6(ip) => format!("https://[{ip}]:{port}"),
        })
        .collect::<Vec<_>>();
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


