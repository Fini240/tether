//! The daemon's session logic, as a library.
//!
//! Exposed rather than buried in `main.rs` so the host and client loops can be
//! driven from an integration test — both roles running in one process over a
//! real TLS socket, against headless backends. Edge switching is the kind of
//! thing that looks right in review and is wrong on the wire, so it wants a
//! test that actually crosses a machine boundary.

pub mod clientmode;
pub mod host;
pub mod session;
