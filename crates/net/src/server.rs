//! Accepting client connections on the host.

use std::net::SocketAddr;
use std::sync::Arc;

use rustls::ServerConfig;
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{server::TlsStream, TlsAcceptor};

use crate::identity::Identity;
use crate::tls::{self, TrustStore};
use crate::transport::{fingerprint_of_der, Connection, NetError, Result};

/// A client that completed the TLS handshake but has not yet said `Hello`.
pub struct Accepted {
    pub addr: SocketAddr,
    /// SHA-256 of the certificate the client authenticated with. The session
    /// layer must check the `Hello`'s `machine_id` against this — a client can
    /// claim any ID it likes in a frame, but it cannot fake this.
    pub fingerprint: String,
    pub connection: Connection<TlsStream<TcpStream>>,
}

pub struct Listener {
    listener: TcpListener,
    acceptor: TlsAcceptor,
    local_addr: SocketAddr,
}

impl Listener {
    pub async fn bind(bind_addr: &str, identity: &Identity, trust: TrustStore) -> Result<Listener> {
        let config: Arc<ServerConfig> = tls::server_config(identity, trust)?;
        let listener = TcpListener::bind(bind_addr).await?;
        let local_addr = listener.local_addr()?;

        Ok(Listener {
            listener,
            acceptor: TlsAcceptor::from(config),
            local_addr,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Accept one client.
    ///
    /// A failed handshake is returned as an error for the caller to log; it
    /// must not stop the accept loop, or one unpaired machine retrying in a
    /// tight loop would take the host down.
    pub async fn accept(&self) -> Result<Accepted> {
        let (stream, addr) = self.listener.accept().await?;
        // Input latency is the whole point of this program; Nagle's algorithm
        // would coalesce mouse deltas into 40 ms batches.
        stream.set_nodelay(true)?;

        let tls_stream = self.acceptor.accept(stream).await?;

        let fingerprint = {
            let (_, connection) = tls_stream.get_ref();
            connection
                .peer_certificates()
                .and_then(|certs| certs.first())
                .map(|cert| fingerprint_of_der(cert))
                .ok_or_else(|| {
                    NetError::Identity("client presented no certificate".to_string())
                })?
        };

        Ok(Accepted {
            addr,
            fingerprint,
            connection: crate::transport::framed(tls_stream),
        })
    }
}
