//! LAN discovery for Alfone over mDNS/DNS-SD.
//!
//! `alfone serve` registers `_alfone._udp.local.` with a TXT record holding the
//! device id, name, short code and version. `alfone connect --name` and
//! `alfone discover` browse that service type, so users do not have to find
//! their own IP addresses. Nothing here replaces the QUIC handshake: discovery
//! only produces a candidate `SocketAddr`, and the TLS fingerprint is still
//! verified afterwards.
//!
//! Scope: local network only. mDNS does not cross routers, which matches the
//! v1 target (LAN) and the security model (no exposed ports beyond the LAN).

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use mdns_sd::{RecvTimeoutError, ResolvedService, ServiceDaemon, ServiceEvent, ServiceInfo};

/// DNS-SD service type announced by `alfone serve`.
pub const SERVICE_TYPE: &str = "_alfone._udp.local.";

/// A server found on the local network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    /// Stable random id from the server's config (hex).
    pub id: String,
    /// Human device name.
    pub name: String,
    /// Short code of the server certificate, for visual comparison.
    pub fingerprint: String,
    /// `CARGO_PKG_VERSION` of the server.
    pub version: String,
    /// Advertised port (the QUIC listener).
    pub port: u16,
    /// mDNS host name, mostly for diagnostics.
    pub host: String,
    /// Advertised addresses, IPv4 first.
    pub addresses: Vec<IpAddr>,
}

impl Peer {
    /// First address usable for a QUIC connection: a LAN IPv4 if present,
    /// then any other IPv4, then IPv6.
    pub fn socket_addr(&self) -> Option<SocketAddr> {
        self.addresses
            .iter()
            .find(|addr| addr.is_ipv4() && !addr.is_loopback())
            .or_else(|| self.addresses.iter().find(|addr| addr.is_ipv4()))
            .or_else(|| self.addresses.first())
            .map(|addr| SocketAddr::new(*addr, self.port))
    }
}

/// What a server announces on the LAN.
#[derive(Debug, Clone)]
pub struct Announcement {
    pub device_name: String,
    pub id: String,
    pub fingerprint: String,
    pub version: String,
    pub port: u16,
}

/// Keeps the mDNS registration alive; dropping it unregisters the service.
pub struct Advertiser {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Advertiser {
    pub fn start(info: Announcement) -> Result<Self> {
        let daemon = ServiceDaemon::new().context("creating the mDNS daemon")?;

        let instance = instance_name(&info.device_name, &info.id);
        let host = format!("alfone-{}.local.", &info.id.chars().take(8).collect::<String>());
        let properties = [
            ("id", info.id.as_str()),
            ("name", info.device_name.as_str()),
            ("fp", info.fingerprint.as_str()),
            ("ver", info.version.as_str()),
        ];
        // Empty address + `enable_addr_auto` lets the library pick the
        // addresses of the active interfaces (and follow changes).
        let service = ServiceInfo::new(
            SERVICE_TYPE,
            &instance,
            &host,
            "",
            info.port,
            &properties[..],
        )
        .with_context(|| format!("invalid mDNS announcement for '{}'", info.device_name))?
        .enable_addr_auto();
        let fullname = service.get_fullname().to_string();
        daemon
            .register(service)
            .context("registering the Alfone mDNS service")?;
        Ok(Self { daemon, fullname })
    }

    /// Unregister and stop the mDNS daemon. Idempotent enough for shutdown.
    pub fn stop(&self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

impl Drop for Advertiser {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Browse for servers for `timeout`, de-duplicated by service instance.
///
/// A short timeout (1–3 s) is enough on a LAN. The function blocks the calling
/// thread; call it from a blocking context or a dedicated task.
pub fn browse(timeout: Duration) -> Result<Vec<Peer>> {
    let daemon = ServiceDaemon::new().context("creating the mDNS daemon")?;
    let receiver = daemon
        .browse(SERVICE_TYPE)
        .context("browsing for Alfone servers")?;

    let deadline = Instant::now() + timeout;
    let mut peers: BTreeMap<String, Peer> = BTreeMap::new();
    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        match receiver.recv_timeout(deadline - now) {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                if let Some(peer) = peer_from_resolved(&info) {
                    peers.insert(info.fullname.clone(), peer);
                }
            }
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
        }
    }

    let _ = daemon.stop_browse(SERVICE_TYPE);
    let _ = daemon.shutdown();
    Ok(peers.into_values().collect())
}

fn peer_from_resolved(info: &ResolvedService) -> Option<Peer> {
    let port = info.port;
    if port == 0 {
        return None;
    }
    let get = |key: &str| info.txt_properties.get_property_val_str(key).unwrap_or_default();
    let name = get("name");
    let name = if name.is_empty() {
        info.fullname.split('.').next().unwrap_or("alfone")
    } else {
        name
    };

    let mut addresses: Vec<IpAddr> = info.addresses.iter().map(|ip| ip.to_ip_addr()).collect();
    addresses.sort_by_key(|addr| !addr.is_ipv4());
    addresses.dedup();

    Some(Peer {
        id: get("id").to_string(),
        name: name.to_string(),
        fingerprint: get("fp").to_string(),
        version: get("ver").to_string(),
        port,
        host: info.host.trim_end_matches('.').to_string(),
        addresses,
    })
}

/// DNS-SD instance names cannot contain dots or control characters, and very
/// long names make browsing output unreadable.
fn instance_name(device_name: &str, id: &str) -> String {
    let mut clean: String = device_name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .take(40)
        .collect();
    clean = clean.trim_matches('-').to_string();
    if clean.is_empty() {
        clean = "alfone".into();
    }
    let suffix: String = id.chars().take(6).collect();
    if suffix.is_empty() {
        clean
    } else {
        format!("{clean}-{suffix}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_names_are_safe() {
        assert_eq!(instance_name("derik-pc", "abcdef1234"), "derik-pc-abcdef");
        assert_eq!(instance_name("Mi PC. (casa)", "123456"), "Mi-PC---casa-123456");
        assert_eq!(instance_name("", "ff00"), "alfone-ff00");
    }

    #[test]
    fn peer_prefers_ipv4() {
        let peer = Peer {
            id: "id".into(),
            name: "name".into(),
            fingerprint: "code".into(),
            version: "0.1.0".into(),
            port: 24800,
            host: "alfone.local".into(),
            addresses: vec![
                "fe80::1".parse().unwrap(),
                "192.168.1.10".parse().unwrap(),
            ],
        };
        assert_eq!(
            peer.socket_addr().unwrap().to_string(),
            "192.168.1.10:24800"
        );
    }

    #[test]
    fn peer_prefers_lan_over_loopback() {
        let peer = Peer {
            id: "id".into(),
            name: "name".into(),
            fingerprint: "code".into(),
            version: "0.1.0".into(),
            port: 24800,
            host: "alfone.local".into(),
            addresses: vec![
                "127.0.0.1".parse().unwrap(),
                "192.168.1.10".parse().unwrap(),
            ],
        };
        assert_eq!(
            peer.socket_addr().unwrap().to_string(),
            "192.168.1.10:24800"
        );
    }

    /// Requires multicast on a local interface. Run with:
    /// `cargo test -p alfone-discovery -- --ignored`
    #[test]
    #[ignore = "needs mDNS multicast on a real network interface"]
    fn advertises_and_finds_itself() {
        let advertiser = Advertiser::start(Announcement {
            device_name: "alfone-test".into(),
            id: "0123456789abcdef".into(),
            fingerprint: "TEST-CODE".into(),
            version: "0.1.0".into(),
            port: 24800,
        })
        .expect("advertising");
        let peers = browse(Duration::from_secs(3)).expect("browsing");
        advertiser.stop();
        assert!(
            peers.iter().any(|peer| peer.id == "0123456789abcdef"),
            "own announcement not found: {peers:?}"
        );
    }
}
