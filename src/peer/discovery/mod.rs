use crate::core::models::PeerDiscoverySource;
use std::net::IpAddr;

pub mod lnd;
pub mod mdns;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredPeer {
    pub node_id: String,
    pub fingerprint: String,
    pub display_name: String,
    pub addresses: Vec<String>,
    pub source: PeerDiscoverySource,
}

pub fn local_peer_addresses(port: u16) -> Vec<String> {
    local_ip_addrs()
        .into_iter()
        .map(|ip| match ip {
            IpAddr::V4(ip) => format!("https://{ip}:{port}"),
            IpAddr::V6(ip) => format!("https://[{ip}]:{port}"),
        })
        .collect()
}

pub fn local_ip_addrs() -> Vec<IpAddr> {
    if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|iface| !iface.is_loopback())
        .map(|iface| iface.ip())
        .filter(|ip| matches!(ip, IpAddr::V4(_)))
        .collect()
}

pub fn merge_discovered(existing: &mut Vec<DiscoveredPeer>, incoming: DiscoveredPeer) {
    if let Some(current) = existing
        .iter_mut()
        .find(|item| item.node_id == incoming.node_id && item.source == incoming.source)
    {
        *current = incoming;
        return;
    }
    existing.push(incoming);
    existing.sort_by(|left, right| left.display_name.cmp(&right.display_name));
}

pub fn remove_discovered(
    existing: &mut Vec<DiscoveredPeer>,
    node_id: &str,
    source: PeerDiscoverySource,
) {
    existing.retain(|item| !(item.node_id == node_id && item.source == source));
}
