//! The client: receives input from a host and injects it locally.
//!
//! A client is deliberately dumb. It does no layout maths, keeps no cursor
//! authority, and never decides where the pointer should be — it is told
//! absolute coordinates and obeys. That is what keeps a three-machine setup
//! from needing distributed agreement about where the cursor is.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};

use tether_core::config::{Config, PairedPeer};
use tether_core::layout::MachineId;
use tether_net::client::{connect, Backoff};
use tether_net::tls::TrustStore;
use tether_net::Identity;
use tether_platform::{Backend, BackendKind};
use tether_proto::{Frame, Platform, Point};

use crate::session;

pub struct Options {
    pub address: Option<String>,
    pub pairing: bool,
    pub config_path: PathBuf,
}

/// Why a session ended. Drives whether we back off before retrying.
enum Ended {
    /// Clean: the host said goodbye, or the user asked us to stop.
    Graceful,
    /// The socket died. Retry with backoff.
    Dropped,
    /// Stop trying entirely.
    Fatal(anyhow::Error),
}

pub async fn run(
    options: Options,
    mut config: Config,
    identity: Identity,
    mut backend: Backend,
) -> Result<()> {
    if backend.kind == BackendKind::Native {
        // Injection needs the same grant as capture on macOS, and fails
        // silently without it — check up front rather than let the user
        // conclude the network is broken.
        tether_platform::check_capture_permission()
            .context("cannot inject input on this machine")?;
    }

    let monitors = backend
        .monitors
        .enumerate()
        .context("could not enumerate this machine's displays")?;
    tracing::info!(count = monitors.len(), "client displays detected");

    let trust = TrustStore::new(config.peers.iter().map(|p| p.fingerprint.clone()));
    trust.set_pairing(options.pairing);
    if options.pairing {
        tracing::warn!(
            fingerprint = %identity.display_fingerprint(),
            "pairing mode is ON — the first host to answer will be trusted. \
             Compare this fingerprint on the host."
        );
    }

    let mut backoff = Backoff::default();

    loop {
        let address = match resolve_address(&options, &config).await {
            Some(address) => address,
            None => {
                tracing::warn!("no host found; retrying");
                tokio::time::sleep(backoff.next_delay()).await;
                continue;
            }
        };

        tracing::info!(%address, "connecting");
        let outcome = session_once(
            &address,
            &mut config,
            &identity,
            &trust,
            &mut backend,
            &monitors,
            &options,
        )
        .await;

        match outcome {
            Ok(Ended::Graceful) => {
                tracing::info!("disconnected");
                backoff.reset();
                // Ctrl-C returns Graceful too; distinguish by checking whether
                // the user asked to stop.
                if STOP.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
            }
            Ok(Ended::Dropped) => {
                backoff.reset();
            }
            Ok(Ended::Fatal(err)) => return Err(err),
            Err(err) => {
                tracing::warn!(%err, "connection failed");
            }
        }

        // Always leave the machine in a clean state between sessions.
        let _ = backend.inject.release_all();

        let delay = backoff.next_delay();
        tracing::debug!(?delay, "retrying");
        tokio::time::sleep(delay).await;
    }

    let _ = backend.inject.release_all();
    let _ = backend.pointer.set_visible(true);
    config.save(&options.config_path).ok();
    Ok(())
}

static STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Explicit address, then the last one that worked, then mDNS.
async fn resolve_address(options: &Options, config: &Config) -> Option<String> {
    if let Some(address) = &options.address {
        return Some(address.clone());
    }

    let found = match tether_net::discovery::browse(Duration::from_secs(3)).await {
        Ok(found) => found,
        Err(err) => {
            tracing::warn!(%err, "discovery failed");
            Vec::new()
        }
    };

    // Prefer a host we have already paired with — on a network with several,
    // silently attaching to a stranger would be surprising.
    let paired = found.iter().find(|host| {
        host.fingerprint
            .as_deref()
            .map(|fp| config.peer_by_fingerprint(fp).is_some())
            .unwrap_or(false)
    });

    if let Some(host) = paired.or_else(|| found.first()) {
        tracing::info!(name = %host.name, "found a host");
        return host.socket_addr();
    }

    // Nothing on mDNS: fall back to whatever address last worked.
    config
        .peers
        .iter()
        .find_map(|peer| peer.last_address.clone())
}

#[allow(clippy::too_many_arguments)]
async fn session_once(
    address: &str,
    config: &mut Config,
    identity: &Identity,
    trust: &TrustStore,
    backend: &mut Backend,
    monitors: &[tether_proto::MonitorInfo],
    options: &Options,
) -> Result<Ended> {
    let connected = connect(address, identity, trust.clone()).await?;
    let host_fingerprint = connected.fingerprint;
    let mut connection = connected.connection;

    connection
        .send(Frame::Hello(session::hello(
            config,
            identity,
            monitors.to_vec(),
        )))
        .await?;

    let welcome = match connection.next().await {
        Some(Ok(Frame::Welcome(welcome))) => welcome,
        Some(Ok(Frame::Rejected(reason))) => {
            // A refusal is about configuration, not connectivity; retrying at
            // speed would just spam the host's log.
            return Ok(Ended::Fatal(anyhow::anyhow!("host rejected us: {reason}")));
        }
        Some(Ok(other)) => anyhow::bail!("expected Welcome, got {other:?}"),
        Some(Err(err)) => return Err(err.into()),
        None => anyhow::bail!("host closed before welcoming us"),
    };

    if let Err(err) = session::check_version(welcome.protocol_version) {
        return Ok(Ended::Fatal(err));
    }

    let host_machine = MachineId(welcome.machine_id);
    if let Some(known) = config.peer(host_machine) {
        if !known.fingerprint.eq_ignore_ascii_case(&host_fingerprint) {
            return Ok(Ended::Fatal(anyhow::anyhow!(
                "host {host_machine} presented fingerprint {host_fingerprint}, \
                 but {} is on record. Either it was reinstalled — remove it from \
                 the config and pair again — or something is impersonating it.",
                known.fingerprint
            )));
        }
    } else {
        println!(
            "\n  Paired with {} ({})\n  fingerprint {}\n  Verify this matches `tether id` on the host.\n",
            welcome.name,
            host_machine,
            tether_net::identity::group_fingerprint(&host_fingerprint)
        );
    }

    config.add_peer(PairedPeer {
        machine: host_machine,
        name: welcome.name.clone(),
        fingerprint: host_fingerprint.clone(),
        last_address: Some(address.to_string()),
    });
    trust.allow(host_fingerprint);
    if let Err(err) = config.save(&options.config_path) {
        tracing::warn!(%err, "could not persist the pairing");
    }

    tracing::info!(
        host = %welcome.name,
        platform = %welcome.platform,
        "connected — this machine is now reachable from the host's screen edge"
    );

    let mut clipboard_poll = tokio::time::interval(Duration::from_millis(500));
    let mut clipboard_seq: u64 = 0;

    loop {
        tokio::select! {
            biased;

            _ = tokio::signal::ctrl_c() => {
                STOP.store(true, std::sync::atomic::Ordering::SeqCst);
                let _ = connection.send(Frame::Bye("client shutting down".into())).await;
                return Ok(Ended::Graceful);
            }

            frame = connection.next() => {
                let Some(frame) = frame else {
                    return Ok(Ended::Dropped);
                };
                let frame = match frame {
                    Ok(frame) => frame,
                    Err(err) => {
                        tracing::debug!(%err, "read error");
                        return Ok(Ended::Dropped);
                    }
                };

                if let Some(ended) =
                    apply(frame, backend, &mut connection, config, welcome.platform).await?
                {
                    return Ok(ended);
                }
            }

            _ = clipboard_poll.tick(), if config.options.sync_clipboard => {
                match backend.clipboard.poll_change() {
                    Ok(Some(mut contents)) => {
                        clipboard_seq += 1;
                        contents.stamp.owner = identity.machine_id.0;
                        contents.stamp.seq = clipboard_seq;
                        if !config.options.sync_clipboard_images {
                            contents.png = None;
                        }
                        connection.send(Frame::ClipboardOffer {
                            stamp: contents.stamp,
                            formats: contents.formats(),
                        }).await?;
                    }
                    Ok(None) => {}
                    Err(err) => tracing::debug!(%err, "clipboard poll failed"),
                }
            }
        }
    }
}

/// Act on one frame. `Some(_)` ends the session.
async fn apply<S>(
    frame: Frame,
    backend: &mut Backend,
    connection: &mut tether_net::Connection<S>,
    config: &Config,
    _host_platform: Platform,
) -> Result<Option<Ended>>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    match frame {
        Frame::Enter { x, y, .. } => {
            // Place the cursor before the first motion event arrives, so the
            // pointer appears at the edge the user crossed rather than jumping
            // from wherever it was parked.
            let _ = backend.pointer.warp(Point::new(x, y));
            let _ = backend.pointer.set_visible(true);
            tracing::debug!(x, y, "cursor entered this machine");
        }

        Frame::Leave => {
            // Release before the host starts sending to someone else, or a
            // modifier held during the crossing stays down here forever.
            let _ = backend.inject.release_all();
            tracing::debug!("cursor left this machine");

            if config.options.lock_screen_on_leave {
                if let Err(err) = backend.lock.lock() {
                    tracing::warn!(%err, "could not lock the screen");
                }
            }
        }

        Frame::ReleaseAll => {
            let _ = backend.inject.release_all();
        }

        Frame::Input(event) => {
            // Already remapped by the host, which knows both platforms and
            // holds any per-machine overrides.
            if let Err(err) = backend.inject.inject(&event) {
                tracing::warn!(%err, "injection failed");
            }
        }

        Frame::Ping(token) => connection.send(Frame::Pong(token)).await?,
        Frame::Pong(_) => {}

        Frame::ClipboardOffer { stamp, formats } => {
            if config.options.sync_clipboard {
                connection
                    .send(Frame::ClipboardRequest { stamp, formats })
                    .await?;
            }
        }

        Frame::ClipboardRequest { .. } => match backend.clipboard.read() {
            Ok(contents) => connection.send(Frame::ClipboardData(contents)).await?,
            Err(err) => tracing::warn!(%err, "could not read the clipboard"),
        },

        Frame::ClipboardData(contents) => {
            if config.options.sync_clipboard {
                match backend.clipboard.write(&contents) {
                    Ok(()) => tracing::info!("clipboard received"),
                    Err(err) => tracing::warn!(%err, "could not write the clipboard"),
                }
            }
        }

        Frame::LockScreen => {
            if let Err(err) = backend.lock.lock() {
                tracing::warn!(%err, "could not lock the screen");
            }
        }

        Frame::FileOffer { transfer, name, .. } => {
            tracing::info!(%name, "refusing a file offer");
            connection
                .send(Frame::FileAbort {
                    transfer,
                    reason: "file transfer is not implemented yet".into(),
                })
                .await?;
        }

        Frame::Bye(reason) => {
            tracing::info!(%reason, "host said goodbye");
            let _ = backend.inject.release_all();
            return Ok(Some(Ended::Dropped));
        }

        Frame::Rejected(reason) => {
            return Ok(Some(Ended::Fatal(anyhow::anyhow!(
                "host rejected us: {reason}"
            ))));
        }

        other => tracing::debug!(?other, "ignoring an unexpected frame"),
    }

    Ok(None)
}
