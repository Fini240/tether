//! Handshake pieces shared by both roles.

use anyhow::{bail, Result};
use tether_core::config::Config;
use tether_core::layout::{MachineId, Placement};
use tether_net::Identity;
use tether_proto::{Hello, MonitorInfo, Platform, Point, Welcome, PROTOCOL_VERSION};

pub fn hello(config: &Config, identity: &Identity, monitors: Vec<MonitorInfo>) -> Hello {
    Hello {
        protocol_version: PROTOCOL_VERSION,
        machine_id: identity.machine_id.0,
        name: config.name.clone(),
        platform: Platform::current(),
        monitors,
    }
}

pub fn welcome(config: &Config, identity: &Identity) -> Welcome {
    Welcome {
        protocol_version: PROTOCOL_VERSION,
        machine_id: identity.machine_id.0,
        name: config.name.clone(),
        platform: Platform::current(),
        heartbeat_ms: config.options.heartbeat_ms,
    }
}

/// Resolves when the process is asked to stop.
///
/// Ctrl-C *and* SIGTERM. SIGTERM is what `launchctl` sends, and what
/// Tether.app's Stop button sends — without it, quitting from the menu bar
/// skipped the clean shutdown entirely: no goodbye to clients, no release of
/// held keys, and the screen arrangement never written back to disk.
pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(term) => term,
            Err(err) => {
                tracing::warn!(%err, "cannot listen for SIGTERM; Ctrl-C only");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Refuse a peer on a different protocol version.
///
/// Silently tolerating a mismatch is not an option here: the frames would still
/// decode often enough to inject *something*, and "occasionally types the wrong
/// character" is a far worse failure than "refuses to connect".
pub fn check_version(theirs: u16) -> Result<()> {
    if theirs != PROTOCOL_VERSION {
        bail!(
            "peer speaks protocol version {theirs}, this build speaks {PROTOCOL_VERSION}. \
             Update both machines to the same release."
        );
    }
    Ok(())
}

/// Build a `Placement` for a peer. `origin` comes from the saved arrangement,
/// or the canvas origin for a machine being seen for the first time — in which
/// case `Layout::auto_place` overwrites it.
pub fn placement_for(
    machine: MachineId,
    name: String,
    platform: Platform,
    monitors: Vec<MonitorInfo>,
    origin: Point,
) -> Placement {
    Placement {
        machine,
        name,
        platform,
        origin,
        monitors,
    }
}

/// This machine's own placement.
pub fn local_placement(
    config: &Config,
    identity: &Identity,
    monitors: Vec<MonitorInfo>,
) -> Placement {
    Placement {
        machine: identity.machine_id,
        name: config.name.clone(),
        platform: Platform::current(),
        origin: Point::new(0, 0),
        monitors,
    }
}
