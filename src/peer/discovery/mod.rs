use crate::core::models::PeerDiscoverySource;
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::time::Duration;

pub mod lnd;
pub mod mdns;

const PEER_PROBE_TIMEOUT: Duration = Duration::from_millis(300);

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
        .filter_map(|ip| format_peer_https_addr(ip, port))
        .collect()
}

pub fn local_ip_addrs() -> Vec<IpAddr> {
    if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|iface| !iface.is_loopback())
        .map(|iface| iface.ip())
        .filter(|ip| is_usable_peer_ip(*ip) && ip.is_ipv4())
        .collect()
}

pub fn is_usable_peer_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            !ip.is_unspecified()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_broadcast()
                && !ip.is_multicast()
        }
        IpAddr::V6(ip) => {
            !ip.is_unspecified()
                && !ip.is_loopback()
                && !ip.is_multicast()
                && !ip.is_unicast_link_local()
        }
    }
}

pub fn format_peer_https_addr(ip: IpAddr, port: u16) -> Option<String> {
    if !is_usable_peer_ip(ip) {
        return None;
    }
    Some(match ip {
        IpAddr::V4(ip) => format!("https://{ip}:{port}"),
        IpAddr::V6(ip) => format!("https://[{ip}]:{port}"),
    })
}

pub fn prefer_reachable_peer_addresses_cached(
    addresses: Vec<String>,
    previous: Option<&[String]>,
) -> Vec<String> {
    let ranked = rank_peer_addresses(addresses, &local_nets());
    if ranked.len() <= 1 {
        return ranked;
    }
    if let Some(previous) = previous
        && previous == ranked.as_slice()
    {
        return ranked;
    }
    if let Some(previous) = previous
        && previous.first().is_some_and(|first| ranked.contains(first))
        && previous.iter().all(|address| ranked.contains(address))
        && ranked.iter().all(|address| previous.contains(address))
    {
        return previous.to_vec();
    }
    if let Some(index) = ranked.iter().position(|address| probe_peer_address(address)) {
        let mut ordered = ranked;
        let chosen = ordered.remove(index);
        ordered.insert(0, chosen);
        return ordered;
    }
    ranked
}

fn rank_peer_addresses(addresses: Vec<String>, nets: &[LocalNet]) -> Vec<String> {
    let mut scored = Vec::new();
    for address in addresses {
        let Ok(canonical) = crate::peer::protocol::parse_peer_address(&address) else {
            continue;
        };
        let Some(socket) = peer_socket_addr(&canonical) else {
            continue;
        };
        if !is_usable_peer_ip(socket.ip()) {
            continue;
        }
        if scored.iter().any(|(_, existing)| existing == &canonical) {
            continue;
        }
        scored.push((address_score(socket.ip(), nets), canonical));
    }
    scored.sort_by(|left, right| right.0.cmp(&left.0).then(left.1.cmp(&right.1)));
    scored.into_iter().map(|(_, address)| address).collect()
}

fn address_score(ip: IpAddr, nets: &[LocalNet]) -> u8 {
    if nets.iter().any(|net| net.contains(ip)) {
        2
    } else {
        1
    }
}

fn probe_peer_address(address: &str) -> bool {
    let Some(socket) = peer_socket_addr(address) else {
        return false;
    };
    TcpStream::connect_timeout(&socket, PEER_PROBE_TIMEOUT).is_ok()
}

fn peer_socket_addr(address: &str) -> Option<SocketAddr> {
    let parsed = url::Url::parse(address).ok()?;
    let port = parsed.port().unwrap_or(15722);
    match parsed.host()? {
        url::Host::Ipv4(ip) => Some(SocketAddr::new(IpAddr::V4(ip), port)),
        url::Host::Ipv6(ip) => Some(SocketAddr::new(IpAddr::V6(ip), port)),
        url::Host::Domain(_) => None,
    }
}

#[derive(Debug, Clone, Copy)]
struct LocalNet {
    network: IpAddr,
    mask: IpAddr,
}

impl LocalNet {
    fn contains(self, ip: IpAddr) -> bool {
        match (ip, self.network, self.mask) {
            (IpAddr::V4(ip), IpAddr::V4(network), IpAddr::V4(mask)) => {
                (u32::from(ip) & u32::from(mask)) == (u32::from(network) & u32::from(mask))
            }
            (IpAddr::V6(ip), IpAddr::V6(network), IpAddr::V6(mask)) => {
                (u128::from(ip) & u128::from(mask)) == (u128::from(network) & u128::from(mask))
            }
            _ => false,
        }
    }
}

fn local_nets() -> Vec<LocalNet> {
    if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|iface| !iface.is_loopback())
        .filter_map(|iface| match iface.addr {
            if_addrs::IfAddr::V4(addr) if is_usable_peer_ip(IpAddr::V4(addr.ip)) => Some(LocalNet {
                network: IpAddr::V4(addr.ip),
                mask: IpAddr::V4(addr.netmask),
            }),
            if_addrs::IfAddr::V6(addr) if is_usable_peer_ip(IpAddr::V6(addr.ip)) => Some(LocalNet {
                network: IpAddr::V6(addr.ip),
                mask: IpAddr::V6(addr.netmask),
            }),
            _ => None,
        })
        .collect()
}

pub fn merge_discovered(existing: &mut Vec<DiscoveredPeer>, incoming: DiscoveredPeer) -> bool {
    if let Some(current) = existing
        .iter_mut()
        .find(|item| item.node_id == incoming.node_id && item.source == incoming.source)
    {
        if current == &incoming {
            return false;
        }
        *current = incoming;
        return true;
    }
    existing.push(incoming);
    existing.sort_by(|left, right| left.display_name.cmp(&right.display_name));
    true
}

pub fn remove_discovered(
    existing: &mut Vec<DiscoveredPeer>,
    node_id: &str,
    source: PeerDiscoverySource,
) {
    existing.retain(|item| !(item.node_id == node_id && item.source == source));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn ranks_same_subnet_addresses_first() {
        let nets = [LocalNet {
            network: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 8)),
            mask: IpAddr::V4(Ipv4Addr::new(255, 255, 255, 0)),
        }];
        let ranked = rank_peer_addresses(
            vec![
                "https://8.8.8.8:15722".to_string(),
                "https://10.0.0.3:15723".to_string(),
                "https://10.0.0.3:15722".to_string(),
            ],
            &nets,
        );
        assert_eq!(
            ranked,
            vec![
                "https://10.0.0.3:15722".to_string(),
                "https://10.0.0.3:15723".to_string(),
                "https://8.8.8.8:15722".to_string(),
            ]
        );
    }
}
