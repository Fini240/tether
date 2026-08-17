//! Framed connections and shared error types.

use sha2::{Digest, Sha256};
use tokio_util::codec::Framed;

use tether_proto::Codec;

pub type Result<T> = std::result::Result<T, NetError>;

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("tls error: {0}")]
    Tls(#[from] rustls::Error),

    #[error("protocol error: {0}")]
    Protocol(#[from] tether_proto::CodecError),

    #[error("identity error: {0}")]
    Identity(String),

    /// The peer's certificate is not one we have paired with. Carries the
    /// fingerprint so the UI can offer to pair with it.
    #[error("unknown peer {fingerprint}; pair with it first")]
    UnknownPeer { fingerprint: String },

    /// A peer presented a certificate other than the one recorded for it.
    /// Either the peer was reinstalled, or somebody is in the middle.
    #[error("fingerprint mismatch for {name}: expected {expected}, got {actual}")]
    FingerprintMismatch {
        name: String,
        expected: String,
        actual: String,
    },

    #[error("peer speaks protocol version {theirs}, we speak {ours}")]
    VersionMismatch { ours: u16, theirs: u16 },

    #[error("connection closed before the handshake completed")]
    HandshakeIncomplete,

    #[error("peer rejected us: {0}")]
    Rejected(String),

    #[error("discovery error: {0}")]
    Discovery(String),
}

/// A framed, encrypted connection to one peer.
pub type Connection<S> = Framed<S, Codec>;

pub fn framed<S>(stream: S) -> Connection<S>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite,
{
    Framed::new(stream, Codec::new())
}

/// Lowercase hex SHA-256 of a DER certificate.
pub fn fingerprint_of_der(der: &[u8]) -> String {
    crate::identity::hex(&Sha256::digest(der))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fingerprint_is_a_hex_sha256() {
        let fp = fingerprint_of_der(b"");
        assert_eq!(fp.len(), 64);
        // SHA-256 of the empty input, as a canary against a hex-encoding slip.
        assert_eq!(
            fp,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
