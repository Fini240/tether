//! Transport: TLS over the LAN, plus mDNS discovery.
//!
//! ## Trust model
//!
//! There is no certificate authority, because there is nobody to be one. Each
//! machine generates a self-signed certificate once and keeps it forever; its
//! SHA-256 fingerprint *is* its identity. Pairing is trust-on-first-use, the
//! same model as SSH's `known_hosts`:
//!
//! 1. A machine in pairing mode accepts an unknown fingerprint, shows it to the
//!    user for comparison, and records it.
//! 2. Afterwards, only recorded fingerprints are accepted.
//!
//! This is stronger than a shared password (nothing guessable crosses the wire,
//! and a compromised peer cannot impersonate a third one) and weaker than a
//! real PKI (a machine-in-the-middle during the pairing window succeeds). The
//! fingerprint is displayed on both ends so that window can be checked.
//!
//! Both directions authenticate: clients pin the host and the host pins its
//! clients. A KVM connection carries every keystroke on the machine, including
//! passwords, so one-sided authentication would not be enough.

pub mod client;
pub mod discovery;
pub mod identity;
pub mod server;
pub mod tls;
pub mod transport;

pub use identity::Identity;
pub use transport::{fingerprint_of_der, Connection, NetError, Result};
