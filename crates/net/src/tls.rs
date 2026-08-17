//! TLS configuration with fingerprint pinning instead of a CA.
//!
//! Both verifiers below deliberately bypass rustls' chain validation
//! (`dangerous()` / a custom `ClientCertVerifier`). That is not a shortcut —
//! there is no certificate authority on a LAN, and a self-signed certificate
//! cannot chain to anything. What replaces it is a stricter check: the peer's
//! certificate must hash to a fingerprint we have already recorded. Signature
//! verification itself is *not* bypassed; the peer still has to prove it holds
//! the matching private key.

use std::sync::{Arc, Mutex};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{
    ClientConfig, DigitallySignedStruct, DistinguishedName, ServerConfig, SignatureScheme,
};
use rustls_pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName, UnixTime};

use crate::identity::Identity;
use crate::transport::{fingerprint_of_der, NetError, Result};

/// SNI value used when connecting. Meaningless — nothing validates names — but
/// TLS requires one.
pub const SNI: &str = "tether.local";

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// The set of fingerprints a machine will accept, plus a pairing switch.
///
/// Shared between the TLS verifier and the UI: turning pairing on from the
/// setup wizard must take effect on the next handshake without a restart.
#[derive(Clone, Default)]
pub struct TrustStore {
    inner: Arc<Mutex<TrustInner>>,
}

#[derive(Default)]
struct TrustInner {
    allowed: Vec<String>,
    pairing: bool,
    /// Fingerprints seen while pairing, for the UI to confirm.
    seen_while_pairing: Vec<String>,
}

impl TrustStore {
    pub fn new(allowed: impl IntoIterator<Item = String>) -> Self {
        let store = Self::default();
        for fingerprint in allowed {
            store.allow(fingerprint);
        }
        store
    }

    pub fn allow(&self, fingerprint: String) {
        let mut inner = self.lock();
        let fingerprint = fingerprint.to_ascii_lowercase();
        if !inner.allowed.contains(&fingerprint) {
            inner.allowed.push(fingerprint);
        }
    }

    pub fn revoke(&self, fingerprint: &str) {
        self.lock()
            .allowed
            .retain(|f| !f.eq_ignore_ascii_case(fingerprint));
    }

    pub fn is_allowed(&self, fingerprint: &str) -> bool {
        self.lock()
            .allowed
            .iter()
            .any(|f| f.eq_ignore_ascii_case(fingerprint))
    }

    /// While on, unknown fingerprints are accepted and recorded in
    /// `pairing_candidates` for the user to confirm.
    pub fn set_pairing(&self, on: bool) {
        let mut inner = self.lock();
        inner.pairing = on;
        if !on {
            inner.seen_while_pairing.clear();
        }
    }

    pub fn is_pairing(&self) -> bool {
        self.lock().pairing
    }

    pub fn pairing_candidates(&self) -> Vec<String> {
        self.lock().seen_while_pairing.clone()
    }

    fn note_candidate(&self, fingerprint: &str) {
        let mut inner = self.lock();
        let fingerprint = fingerprint.to_ascii_lowercase();
        if !inner.seen_while_pairing.contains(&fingerprint) {
            inner.seen_while_pairing.push(fingerprint);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, TrustInner> {
        // A poisoned trust store would mean a panic inside a verifier. Failing
        // closed is not an option (nothing would ever connect again) and the
        // data is a plain list, so recovering it is safe.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Verifies the *server* we dialled, from a client.
#[derive(Debug)]
struct PinnedServerVerifier {
    trust: TrustStore,
    provider: Arc<CryptoProvider>,
}

impl std::fmt::Debug for TrustStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrustStore")
            .field("allowed", &self.lock().allowed.len())
            .field("pairing", &self.lock().pairing)
            .finish()
    }
}

impl ServerCertVerifier for PinnedServerVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        let fingerprint = fingerprint_of_der(end_entity);

        if self.trust.is_allowed(&fingerprint) {
            return Ok(ServerCertVerified::assertion());
        }
        if self.trust.is_pairing() {
            self.trust.note_candidate(&fingerprint);
            tracing::warn!(%fingerprint, "accepting an unpaired host because pairing is on");
            return Ok(ServerCertVerified::assertion());
        }

        Err(rustls::Error::General(format!(
            "host fingerprint {fingerprint} is not paired"
        )))
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Verifies a *client* that dialled us, from the host.
#[derive(Debug)]
struct PinnedClientVerifier {
    trust: TrustStore,
    provider: Arc<CryptoProvider>,
}

impl ClientCertVerifier for PinnedClientVerifier {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        // No CA to hint at, so send an empty list and let the client offer its
        // self-signed certificate.
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> std::result::Result<ClientCertVerified, rustls::Error> {
        let fingerprint = fingerprint_of_der(end_entity);

        if self.trust.is_allowed(&fingerprint) {
            return Ok(ClientCertVerified::assertion());
        }
        if self.trust.is_pairing() {
            self.trust.note_candidate(&fingerprint);
            tracing::warn!(%fingerprint, "accepting an unpaired client because pairing is on");
            return Ok(ClientCertVerified::assertion());
        }

        Err(rustls::Error::General(format!(
            "client fingerprint {fingerprint} is not paired"
        )))
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn cert_and_key(
    identity: &Identity,
) -> (Vec<CertificateDer<'static>>, PrivatePkcs8KeyDer<'static>) {
    (
        vec![CertificateDer::from(identity.cert_der.clone())],
        PrivatePkcs8KeyDer::from(identity.key_der.clone()),
    )
}

/// Host side. Requires and pins a client certificate.
pub fn server_config(identity: &Identity, trust: TrustStore) -> Result<Arc<ServerConfig>> {
    let provider = provider();
    let verifier = Arc::new(PinnedClientVerifier {
        trust,
        provider: Arc::clone(&provider),
    });
    let (certs, key) = cert_and_key(identity);

    let config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(NetError::Tls)?
        .with_client_cert_verifier(verifier)
        .with_single_cert(certs, key.into())
        .map_err(NetError::Tls)?;

    Ok(Arc::new(config))
}

/// Client side. Pins the host and presents our own certificate.
pub fn client_config(identity: &Identity, trust: TrustStore) -> Result<Arc<ClientConfig>> {
    let provider = provider();
    let verifier = Arc::new(PinnedServerVerifier {
        trust,
        provider: Arc::clone(&provider),
    });
    let (certs, key) = cert_and_key(identity);

    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(NetError::Tls)?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(certs, key.into())
        .map_err(NetError::Tls)?;

    Ok(Arc::new(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_fingerprint_is_rejected_unless_pairing() {
        let trust = TrustStore::new(["abc".to_string()]);
        assert!(trust.is_allowed("ABC"), "matching must ignore case");
        assert!(!trust.is_allowed("def"));

        trust.set_pairing(true);
        assert!(trust.is_pairing());
        trust.set_pairing(false);
        assert!(trust.pairing_candidates().is_empty());
    }

    #[test]
    fn revoking_removes_a_fingerprint() {
        let trust = TrustStore::new(["abc".to_string()]);
        trust.revoke("ABC");
        assert!(!trust.is_allowed("abc"));
    }

    #[test]
    fn both_configs_build_from_a_generated_identity() {
        let identity = Identity::generate().unwrap();
        let trust = TrustStore::default();
        assert!(server_config(&identity, trust.clone()).is_ok());
        assert!(client_config(&identity, trust).is_ok());
    }
}
