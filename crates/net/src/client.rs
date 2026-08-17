//! Dialling the host from a client, with reconnection.

use std::sync::Arc;
use std::time::Duration;

use rustls::ClientConfig;
use rustls_pki_types::ServerName;
use tokio::net::TcpStream;
use tokio_rustls::{client::TlsStream, TlsConnector};

use crate::identity::Identity;
use crate::tls::{self, TrustStore, SNI};
use crate::transport::{fingerprint_of_der, Connection, NetError, Result};

pub struct Connected {
    /// SHA-256 of the host's certificate.
    pub fingerprint: String,
    pub connection: Connection<TlsStream<TcpStream>>,
}

/// Dial once.
pub async fn connect(address: &str, identity: &Identity, trust: TrustStore) -> Result<Connected> {
    let config: Arc<ClientConfig> = tls::client_config(identity, trust)?;
    let connector = TlsConnector::from(config);

    let stream = TcpStream::connect(address).await?;
    stream.set_nodelay(true)?;

    // Names are not validated — pinning replaces that — but TLS requires an
    // SNI value, so every connection uses the same placeholder.
    let server_name = ServerName::try_from(SNI)
        .map_err(|e| NetError::Identity(format!("invalid SNI: {e}")))?
        .to_owned();

    let tls_stream = connector.connect(server_name, stream).await?;

    let fingerprint = {
        let (_, connection) = tls_stream.get_ref();
        connection
            .peer_certificates()
            .and_then(|certs| certs.first())
            .map(|cert| fingerprint_of_der(cert))
            .ok_or_else(|| NetError::Identity("host presented no certificate".to_string()))?
    };

    Ok(Connected {
        fingerprint,
        connection: crate::transport::framed(tls_stream),
    })
}

/// Exponential backoff for reconnection.
///
/// Capped rather than unbounded so a machine that was asleep for a weekend
/// rejoins within a few seconds of waking, not after an hour-long wait that
/// happened to be scheduled.
#[derive(Debug, Clone)]
pub struct Backoff {
    current: Duration,
    initial: Duration,
    max: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new(Duration::from_millis(500), Duration::from_secs(15))
    }
}

impl Backoff {
    pub fn new(initial: Duration, max: Duration) -> Self {
        Self {
            current: initial,
            initial,
            max,
        }
    }

    /// The delay to wait before the next attempt, then double it.
    pub fn next_delay(&mut self) -> Duration {
        let delay = self.current;
        self.current = (self.current * 2).min(self.max);
        delay
    }

    /// Call after a successful connection so the next outage starts fast again.
    pub fn reset(&mut self) {
        self.current = self.initial;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_then_holds_at_the_cap() {
        let mut backoff = Backoff::new(Duration::from_millis(100), Duration::from_millis(400));
        assert_eq!(backoff.next_delay(), Duration::from_millis(100));
        assert_eq!(backoff.next_delay(), Duration::from_millis(200));
        assert_eq!(backoff.next_delay(), Duration::from_millis(400));
        assert_eq!(backoff.next_delay(), Duration::from_millis(400));
    }

    #[test]
    fn reset_restores_the_initial_delay() {
        let mut backoff = Backoff::new(Duration::from_millis(100), Duration::from_millis(400));
        backoff.next_delay();
        backoff.next_delay();
        backoff.reset();
        assert_eq!(backoff.next_delay(), Duration::from_millis(100));
    }
}
