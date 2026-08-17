//! This machine's long-lived cryptographic identity.
//!
//! Generated once and stored beside the config. Deleting it is equivalent to
//! becoming a new machine: every peer will treat it as unknown and it has to be
//! paired again.

use std::path::Path;

use sha2::{Digest, Sha256};
use tether_core::layout::MachineId;

use crate::transport::{NetError, Result};

const CERT_FILE: &str = "identity.crt.der";
const KEY_FILE: &str = "identity.key.der";

/// A self-signed certificate and its private key, plus the derived identifiers.
#[derive(Clone)]
pub struct Identity {
    pub cert_der: Vec<u8>,
    /// PKCS#8 DER. Never leaves this machine.
    pub key_der: Vec<u8>,
    /// Lowercase hex SHA-256 of `cert_der` — what the user compares when
    /// pairing, and what peers pin.
    pub fingerprint: String,
    pub machine_id: MachineId,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never let the private key reach a log line.
        f.debug_struct("Identity")
            .field("machine_id", &self.machine_id)
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

impl Identity {
    pub fn generate() -> Result<Identity> {
        // The SAN is a fixed placeholder: name-based validation is switched
        // off entirely in favour of fingerprint pinning, and a real hostname
        // here would only invite someone to start trusting it.
        let certified = rcgen::generate_simple_self_signed(vec!["tether.local".to_string()])
            .map_err(|e| NetError::Identity(format!("could not generate a certificate: {e}")))?;

        let cert_der = certified.cert.der().to_vec();
        let key_der = certified.key_pair.serialize_der();
        Ok(Identity::from_parts(cert_der, key_der))
    }

    fn from_parts(cert_der: Vec<u8>, key_der: Vec<u8>) -> Identity {
        let digest = Sha256::digest(&cert_der);
        let fingerprint = hex(&digest);
        // First 8 bytes of the fingerprint, big-endian. Collisions would need a
        // 64-bit prefix collision in SHA-256; the full fingerprint is still
        // what gets checked, so an ID clash is a display problem, not a
        // security one.
        let mut id_bytes = [0u8; 8];
        id_bytes.copy_from_slice(&digest[..8]);

        Identity {
            cert_der,
            key_der,
            fingerprint,
            machine_id: MachineId(u64::from_be_bytes(id_bytes)),
        }
    }

    /// Load from `dir`, generating and saving a new identity if none is there.
    pub fn load_or_generate(dir: &Path) -> Result<Identity> {
        let cert_path = dir.join(CERT_FILE);
        let key_path = dir.join(KEY_FILE);

        match (std::fs::read(&cert_path), std::fs::read(&key_path)) {
            (Ok(cert_der), Ok(key_der)) => Ok(Identity::from_parts(cert_der, key_der)),
            _ => {
                let identity = Identity::generate()?;
                identity.save(dir)?;
                tracing::info!(
                    fingerprint = %identity.fingerprint,
                    "generated a new machine identity"
                );
                Ok(identity)
            }
        }
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir).map_err(|e| NetError::Identity(format!("{dir:?}: {e}")))?;

        let key_path = dir.join(KEY_FILE);
        std::fs::write(dir.join(CERT_FILE), &self.cert_der)
            .map_err(|e| NetError::Identity(format!("writing the certificate: {e}")))?;
        std::fs::write(&key_path, &self.key_der)
            .map_err(|e| NetError::Identity(format!("writing the private key: {e}")))?;

        restrict_to_owner(&key_path)?;
        Ok(())
    }

    /// Grouped into colon-separated bytes for reading aloud during pairing.
    pub fn display_fingerprint(&self) -> String {
        group_fingerprint(&self.fingerprint)
    }
}

/// `0600` on Unix. On Windows the file inherits the user profile's ACL, which
/// already excludes other users.
fn restrict_to_owner(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| NetError::Identity(format!("restricting key permissions: {e}")))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// `aabbcc…` → `aa:bb:cc:…`
pub fn group_fingerprint(fingerprint: &str) -> String {
    fingerprint
        .as_bytes()
        .chunks(2)
        .map(|pair| String::from_utf8_lossy(pair).into_owned())
        .collect::<Vec<_>>()
        .join(":")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_identity_is_self_consistent() {
        let identity = Identity::generate().unwrap();
        assert_eq!(identity.fingerprint.len(), 64);
        assert_eq!(
            identity.fingerprint,
            hex(&Sha256::digest(&identity.cert_der))
        );
    }

    #[test]
    fn two_identities_differ() {
        let a = Identity::generate().unwrap();
        let b = Identity::generate().unwrap();
        assert_ne!(a.fingerprint, b.fingerprint);
        assert_ne!(a.machine_id, b.machine_id);
    }

    #[test]
    fn an_identity_survives_a_round_trip_through_disk() {
        let dir = std::env::temp_dir().join(format!("tether-id-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let first = Identity::load_or_generate(&dir).unwrap();
        let second = Identity::load_or_generate(&dir).unwrap();
        assert_eq!(first.fingerprint, second.fingerprint);
        assert_eq!(first.machine_id, second.machine_id);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_display_form_is_byte_grouped() {
        assert_eq!(group_fingerprint("aabbcc"), "aa:bb:cc");
    }

    #[test]
    fn debug_output_hides_the_private_key() {
        let identity = Identity::generate().unwrap();
        let rendered = format!("{identity:?}");
        assert!(!rendered.contains("key_der"));
        assert!(rendered.contains(&identity.fingerprint));
    }
}
