//! mDNS / DNS-SD auto-discovery.
//!
//! Hosts advertise `_tether._tcp.local.`; clients browse for it. The TXT record
//! carries the host's fingerprint, so a client can tell an already-paired host
//! from a stranger *before* connecting, and the UI can show "your desktop" next
//! to "an unknown machine".
//!
//! Discovery is a convenience, never a trust decision: anything on the LAN can
//! advertise this service and claim any fingerprint it likes. The TLS handshake
//! is what actually checks it.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Duration;

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use tether_proto::SERVICE_TYPE;

use crate::transport::{NetError, Result};

const TXT_FINGERPRINT: &str = "fp";
const TXT_NAME: &str = "name";
const TXT_PLATFORM: &str = "os";
const TXT_VERSION: &str = "v";

/// A host found on the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredHost {
    pub name: String,
    pub addresses: Vec<IpAddr>,
    pub port: u16,
    /// Claimed certificate fingerprint. Verified by TLS, not by us.
    pub fingerprint: Option<String>,
    pub platform: Option<String>,
    pub protocol_version: Option<u16>,
}

impl DiscoveredHost {
    /// First address as a dialable `host:port`.
    pub fn socket_addr(&self) -> Option<String> {
        self.addresses.first().map(|ip| match ip {
            IpAddr::V4(v4) => format!("{v4}:{}", self.port),
            IpAddr::V6(v6) => format!("[{v6}]:{}", self.port),
        })
    }
}

/// Advertises this host. Stops advertising when dropped.
pub struct Advertiser {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Advertiser {
    pub fn start(
        instance_name: &str,
        port: u16,
        fingerprint: &str,
        platform: &str,
    ) -> Result<Advertiser> {
        let daemon = ServiceDaemon::new()
            .map_err(|e| NetError::Discovery(format!("could not start mDNS: {e}")))?;

        let mut properties = HashMap::new();
        properties.insert(TXT_FINGERPRINT.to_string(), fingerprint.to_string());
        properties.insert(TXT_NAME.to_string(), instance_name.to_string());
        properties.insert(TXT_PLATFORM.to_string(), platform.to_string());
        properties.insert(
            TXT_VERSION.to_string(),
            tether_proto::PROTOCOL_VERSION.to_string(),
        );

        // Instance names may not contain dots — they would be read as extra
        // DNS labels. Hostnames like "narf.local" are common, so sanitise.
        let instance = sanitise_instance_name(instance_name);
        let host_name = format!("{instance}.local.");

        let service = ServiceInfo::new(
            SERVICE_TYPE,
            &instance,
            &host_name,
            "",
            port,
            Some(properties),
        )
        .map_err(|e| NetError::Discovery(format!("invalid service info: {e}")))?
        // Fills in this machine's addresses and keeps them current as
        // interfaces come and go — important on a laptop moving between
        // wifi and a dock.
        .enable_addr_auto();

        let fullname = service.get_fullname().to_string();
        daemon
            .register(service)
            .map_err(|e| NetError::Discovery(format!("could not advertise: {e}")))?;

        tracing::info!(%fullname, port, "advertising over mDNS");
        Ok(Advertiser { daemon, fullname })
    }

    pub fn fullname(&self) -> &str {
        &self.fullname
    }
}

impl Drop for Advertiser {
    fn drop(&mut self) {
        // Best effort: send a goodbye packet so browsers drop us promptly
        // instead of waiting for the TTL.
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

/// Browse for hosts for `timeout`, returning everything that resolved.
///
/// One-shot rather than a stream because both callers — the setup wizard and
/// `tether discover` — want a list to show the user, not a subscription.
pub async fn browse(timeout: Duration) -> Result<Vec<DiscoveredHost>> {
    let daemon = ServiceDaemon::new()
        .map_err(|e| NetError::Discovery(format!("could not start mDNS: {e}")))?;
    let receiver = daemon
        .browse(SERVICE_TYPE)
        .map_err(|e| NetError::Discovery(format!("could not browse: {e}")))?;

    let deadline = tokio::time::Instant::now() + timeout;
    let mut found: Vec<DiscoveredHost> = Vec::new();

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }

        // `receiver` is a blocking channel; poll it with a short timeout on a
        // blocking thread so the async caller is never stalled.
        let receiver = receiver.clone();
        let step = remaining.min(Duration::from_millis(250));
        let event = tokio::task::spawn_blocking(move || receiver.recv_timeout(step))
            .await
            .map_err(|e| NetError::Discovery(format!("discovery task failed: {e}")))?;

        match event {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                let host = DiscoveredHost {
                    name: info
                        .get_property_val_str(TXT_NAME)
                        .unwrap_or_else(|| info.get_fullname())
                        .to_string(),
                    addresses: info.get_addresses().iter().copied().collect(),
                    port: info.get_port(),
                    fingerprint: info
                        .get_property_val_str(TXT_FINGERPRINT)
                        .map(str::to_string),
                    platform: info.get_property_val_str(TXT_PLATFORM).map(str::to_string),
                    protocol_version: info
                        .get_property_val_str(TXT_VERSION)
                        .and_then(|v| v.parse().ok()),
                };
                if !found.iter().any(|h| h.fingerprint == host.fingerprint) {
                    found.push(host);
                }
            }
            Ok(_) => {}
            // Timed out on this step; loop and check the deadline.
            Err(_) => continue,
        }
    }

    let _ = daemon.shutdown();
    Ok(found)
}

/// Strip characters that are not valid in a DNS-SD instance label.
fn sanitise_instance_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c == '.' || c.is_whitespace() {
                '-'
            } else {
                c
            }
        })
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();

    let trimmed = cleaned.trim_matches('-');
    if trimmed.is_empty() {
        "tether".to_string()
    } else {
        // DNS labels cap at 63 bytes.
        trimmed.chars().take(63).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_names_lose_dots_and_spaces() {
        assert_eq!(sanitise_instance_name("narf.local"), "narf-local");
        assert_eq!(sanitise_instance_name("Finn's MacBook"), "Finns-MacBook");
    }

    #[test]
    fn an_unusable_name_falls_back() {
        assert_eq!(sanitise_instance_name("..."), "tether");
        assert_eq!(sanitise_instance_name(""), "tether");
    }

    #[test]
    fn instance_names_are_capped_at_a_dns_label() {
        assert_eq!(sanitise_instance_name(&"a".repeat(200)).len(), 63);
    }

    #[test]
    fn a_discovered_host_formats_v4_and_v6_addresses() {
        let mut host = DiscoveredHost {
            name: "n".into(),
            addresses: vec!["192.168.1.5".parse().unwrap()],
            port: 24800,
            fingerprint: None,
            platform: None,
            protocol_version: None,
        };
        assert_eq!(host.socket_addr().unwrap(), "192.168.1.5:24800");

        host.addresses = vec!["fe80::1".parse().unwrap()];
        assert_eq!(host.socket_addr().unwrap(), "[fe80::1]:24800");
    }
}
