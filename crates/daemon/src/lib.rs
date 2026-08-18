//! The daemon's session logic, as a library.
//!
//! Exposed rather than buried in `main.rs` so the host and client loops can be
//! driven from an integration test — both roles running in one process over a
//! real TLS socket, against headless backends. Edge switching is the kind of
//! thing that looks right in review and is wrong on the wire, so it wants a
//! test that actually crosses a machine boundary.

pub mod auto;
pub mod clientmode;
pub mod control;
pub mod host;
pub mod session;

/// Why a role stopped.
///
/// A role chosen by hand only ever ends because the user ended it. `Auto`
/// needs the other answer too: the network changed, this machine should be
/// doing the other job now, and the supervisor should start it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The process was asked to stop, and should.
    Stopped,
    /// This role is over but the session is not. Run the other one.
    Supersede,
}
