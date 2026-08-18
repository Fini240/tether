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
use tether_platform::{Backend, BackendKind, LocalEvent};
use tether_proto::{Frame, Point, SourceEvent};

use crate::control::{Command, DaemonControl, PeerInfo};
use crate::session;
use crate::Outcome;

pub struct Options {
    pub address: Option<String>,
    pub pairing: bool,
    pub config_path: PathBuf,
    /// Publishes status and receives commands. `None` when run from the CLI.
    pub control: Option<DaemonControl>,
    /// Give up and hand back rather than retrying forever when there is no
    /// host to be found.
    ///
    /// A client the user asked for keeps waiting: they said this machine is a
    /// client, so the host is presumably coming back. Under `auto`, nothing
    /// answering means nobody is arbitrating, and the right move is to do it
    /// ourselves rather than sit waiting for a machine that may be switched
    /// off for the night.
    pub auto: bool,
}

/// What this client believes about who is driving and where the cursor is.
///
/// Both are told to us by the host; a client never decides either for itself.
/// Keeping them together makes the one rule that matters easy to state:
/// suppress local input only while we are driving *and* the pointer is
/// elsewhere.
#[derive(Debug, Clone, Copy)]
struct Ownership {
    /// This machine's keyboard and mouse are the ones driving.
    owns_input: bool,
    /// The pointer is on this machine's screen.
    cursor_here: bool,
}

impl Ownership {
    fn new() -> Self {
        Self {
            owns_input: false,
            cursor_here: false,
        }
    }

    /// Suppress local input only when our own events are being sent elsewhere.
    ///
    /// Crucially this is false whenever we are *not* driving, which is what
    /// keeps this machine's own trackpad alive so its user can take control
    /// back by touching it — and what stops a dropped connection leaving the
    /// keyboard dead.
    fn should_swallow(&self) -> bool {
        self.owns_input && !self.cursor_here
    }

    fn apply(&self, backend: &mut Backend) {
        let swallow = self.should_swallow();
        if swallow {
            // Hide, pin, swallow — the same order the host uses, and mirrored
            // on the way back. Suppression alone stops applications seeing the
            // movement but not the cursor from following the mouse around the
            // screen, which is what the user actually notices once the pointer
            // is on another machine.
            let _ = backend.pointer.set_visible(false);
            pin(backend, true);
            backend.capture.set_swallow(true);
        } else {
            backend.capture.set_swallow(false);
            pin(backend, false);
            let _ = backend.pointer.set_visible(true);
        }
    }
}

/// Freeze or release this machine's cursor, warning rather than failing: the
/// pointer still routes correctly without it, the cursor is just less well
/// behaved, and that is no reason to drop a session.
fn pin(backend: &Backend, pinned: bool) {
    if let Err(err) = backend.pointer.set_pinned(pinned) {
        tracing::warn!(%err, pinned, "could not pin the cursor");
    }
}

/// Worth taking control for? Mirrors the host's rule: a pixel of drift should
/// not steal control from the machine somebody is typing on.
fn is_deliberate(event: &LocalEvent) -> bool {
    match event {
        LocalEvent::Key { pressed, .. } => *pressed,
        LocalEvent::Button { pressed, .. } => *pressed,
        LocalEvent::MouseDelta { dx, dy } | LocalEvent::MouseMoved { dx, dy, .. } => {
            dx.abs() + dy.abs() >= 3
        }
        LocalEvent::Wheel { dx, dy } => dx.abs() + dy.abs() >= 1.0,
    }
}

fn to_source(event: LocalEvent) -> SourceEvent {
    match event {
        LocalEvent::MouseDelta { dx, dy } => SourceEvent::MouseDelta { dx, dy },
        LocalEvent::MouseMoved { x, y, dx, dy } => SourceEvent::MouseMoved { x, y, dx, dy },
        LocalEvent::Button { button, pressed } => SourceEvent::Button { button, pressed },
        LocalEvent::Wheel { dx, dy } => SourceEvent::Wheel { dx, dy },
        LocalEvent::Key {
            key,
            pressed,
            modifiers,
            repeat,
        } => SourceEvent::Key {
            key,
            pressed,
            modifiers,
            repeat,
        },
    }
}

/// Why a session ended. Drives whether we reconnect.
enum Ended {
    /// The user asked us to stop. Do not reconnect, and return from `run`.
    Stopped,
    /// The host said goodbye, or the socket died. Reconnect.
    Dropped,
    /// Something is misconfigured. Stop trying and report it.
    Fatal(anyhow::Error),
}

pub async fn run(
    mut options: Options,
    mut config: Config,
    identity: Identity,
    backend: &mut Backend,
) -> Result<Outcome> {
    let control = options.control.take();
    let (status, mut commands) = match control {
        Some(control) => (Some(control.status), Some(control.commands)),
        None => (None, None),
    };
    if let Some(status) = &status {
        status.update(|s| {
            s.running = true;
            s.role = Some(tether_core::config::Role::Client);
            s.this_machine = Some(identity.machine_id);
            s.detail = "looking for a host".into();
        });
    }

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

    // A client captures too, so touching its keyboard can take control. The
    // channel outlives individual connections: the capture backend is started
    // once, not re-armed on every reconnect.
    let (input_tx, mut input_rx) = tokio::sync::mpsc::unbounded_channel::<LocalEvent>();
    if config.options.auto_input_handoff {
        if let Err(err) = backend.capture.start(input_tx) {
            tracing::warn!(
                %err,
                "could not capture local input; this machine cannot take control by being touched"
            );
        }
    }

    let mut backoff = Backoff::default();

    loop {
        let candidates = resolve_addresses(&options, &config).await;
        if candidates.is_empty() {
            if options.auto {
                tracing::info!("nothing is arbitrating on this network; taking it on");
                return Ok(Outcome::Supersede);
            }
            tracing::warn!("no host found; retrying");
            tokio::time::sleep(backoff.next_delay()).await;
            continue;
        }

        // Try each in turn. The timeout is the whole point: without it an
        // address that is advertised but black-holed hangs for the OS default
        // of over a minute, and four of them in a row look exactly like the
        // app having frozen.
        let mut address = None;
        for candidate in &candidates {
            tracing::debug!(%candidate, "trying");
            match tokio::time::timeout(
                tether_net::client::CONNECT_TIMEOUT,
                tokio::net::TcpStream::connect(candidate),
            )
            .await
            {
                Ok(Ok(_)) => {
                    address = Some(candidate.clone());
                    break;
                }
                Ok(Err(err)) => tracing::debug!(%candidate, %err, "refused"),
                Err(_) => tracing::debug!(%candidate, "timed out"),
            }
        }

        let Some(address) = address else {
            tracing::warn!(
                tried = candidates.len(),
                "found a host but could not reach it on any of its addresses"
            );
            if let Some(status) = &status {
                let detail = format!(
                    "found a host but none of its {} addresses answered",
                    candidates.len()
                );
                status.update(|s| s.detail = detail);
            }
            tokio::time::sleep(backoff.next_delay()).await;
            continue;
        };

        tracing::info!(%address, "connecting");
        if let Some(status) = &status {
            let address = address.clone();
            status.update(|s| s.detail = format!("connecting to {address}"));
        }

        // Between attempts as well as during a session, so stopping while
        // reconnecting does not have to wait out the backoff.
        if let Some(rx) = &mut commands {
            if let Ok(Command::Stop) = rx.try_recv() {
                break;
            }
        }
        let outcome = session_once(
            &address,
            &mut config,
            &identity,
            &trust,
            backend,
            &monitors,
            &options,
            &mut input_rx,
            &status,
            &mut commands,
        )
        .await;

        match outcome {
            Ok(Ended::Stopped) => {
                tracing::info!("stopped");
                break;
            }
            Ok(Ended::Dropped) => {
                tracing::info!("disconnected");
                backoff.reset();
            }
            Ok(Ended::Fatal(err)) => {
                if let Some(status) = &status {
                    let message = format!("{err:#}");
                    status.update(|s| {
                        s.running = false;
                        s.error = Some(message);
                        s.detail = "stopped".into();
                    });
                }
                return Err(err);
            }
            Err(err) => {
                tracing::warn!(%err, "connection failed");
            }
        }

        // Always leave the machine usable between sessions: never leave local
        // input suppressed just because the link dropped mid-session.
        let _ = backend.inject.release_all();
        backend.capture.set_swallow(false);
        pin(backend, false);
        let _ = backend.pointer.set_visible(true);

        let delay = backoff.next_delay();
        tracing::debug!(?delay, "retrying");
        tokio::time::sleep(delay).await;
    }

    let _ = backend.inject.release_all();
    backend.capture.set_swallow(false);
    pin(backend, false);
    let _ = backend.pointer.set_visible(true);
    backend.capture.stop();
    config.save(&options.config_path).ok();
    if let Some(status) = &status {
        status.update(|s| {
            s.running = false;
            s.peers.clear();
            s.detail = "stopped".into();
        });
    }
    Ok(Outcome::Stopped)
}

/// Await the next command, or never, when nothing is driving this session.
///
/// `pending()` rather than returning `None`: a `None` arm in a `select!` loop
/// resolves instantly and spins the loop at full speed.
async fn next_command(
    commands: &mut Option<tokio::sync::mpsc::UnboundedReceiver<Command>>,
) -> Option<Command> {
    match commands {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// Every address worth trying, best first.
///
/// A list rather than one address: a host advertises whatever its interfaces
/// have, and some of those are routinely unreachable — a global IPv6 the host
/// is not listening on, or an interface that is up but not on this network.
/// Trying only the first is what makes two machines that can see each other
/// perfectly still fail to connect.
async fn resolve_addresses(options: &Options, config: &Config) -> Vec<String> {
    if let Some(address) = &options.address {
        return vec![address.clone()];
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

    let mut candidates = Vec::new();
    if let Some(host) = paired.or_else(|| found.first()) {
        tracing::info!(name = %host.name, "found a host");
        candidates.extend(host.socket_addrs());
    }

    // Whatever worked last time, in case mDNS is unhelpful today.
    for peer in &config.peers {
        if let Some(address) = &peer.last_address {
            if !candidates.contains(address) {
                candidates.push(address.clone());
            }
        }
    }

    candidates
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
    input_rx: &mut tokio::sync::mpsc::UnboundedReceiver<LocalEvent>,
    status: &Option<crate::control::StatusHandle>,
    commands: &mut Option<tokio::sync::mpsc::UnboundedReceiver<Command>>,
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
    if let Some(status) = &status {
        let peer = PeerInfo {
            machine: host_machine,
            name: welcome.name.clone(),
            platform: welcome.platform,
            address: address.to_string(),
            connected: true,
        };
        let detail = format!("connected to {}", welcome.name);
        status.update(|s| {
            s.peers = vec![peer];
            s.detail = detail;
            s.error = None;
        });
    }

    let mut clipboard_poll = tokio::time::interval(Duration::from_millis(500));
    let mut clipboard_seq: u64 = 0;
    let mut ownership = Ownership::new();
    ownership.apply(backend);

    // Anything captured while disconnected is stale by now and would replay as
    // a burst of movement the moment we reconnect.
    while input_rx.try_recv().is_ok() {}

    loop {
        tokio::select! {
            biased;

            _ = session::shutdown_signal() => {
                let _ = connection.send(Frame::Bye("client shutting down".into())).await;
                return Ok(Ended::Stopped);
            }

            // Without this arm a connected client never saw a Stop at all: the
            // only check was between connection attempts, so an interface
            // asking it to stop waited for the connection to break first —
            // which, for a healthy connection, is never.
            Some(command) = next_command(commands) => {
                if matches!(command, Command::Stop) {
                    tracing::info!("stopping at the interface's request");
                    let _ = connection.send(Frame::Bye("client stopping".into())).await;
                    return Ok(Ended::Stopped);
                }
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
                    apply(
                        frame,
                        backend,
                        &mut connection,
                        config,
                        &mut ownership,
                        identity.machine_id.0,
                        status,
                    )
                    .await?
                {
                    return Ok(ended);
                }
            }

            Some(event) = input_rx.recv(), if config.options.auto_input_handoff => {
                if !ownership.owns_input {
                    if !is_deliberate(&event) {
                        continue;
                    }
                    // Ask; do not assume. The host arbitrates, and it replies
                    // with InputOwner — which is what actually flips us over.
                    // With the event: "something local happened" is not
                    // enough to debug a machine that grabs control when it
                    // should not. Which kind, and how big, is the whole
                    // question — a stray one-pixel drift and a real keypress
                    // read identically in a log that only says "input".
                    tracing::debug!(?event, "local input detected; claiming control");
                    connection.send(Frame::ClaimInput).await?;
                }
                connection.send(Frame::SourceInput(to_source(event))).await?;
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
    ownership: &mut Ownership,
    my_machine: u64,
    status: &Option<crate::control::StatusHandle>,
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
            ownership.cursor_here = true;
            ownership.apply(backend);
            if let Some(status) = status {
                let me = tether_core::layout::MachineId(my_machine);
                status.update(|s| s.cursor_on = Some(me));
            }
            tracing::debug!(x, y, "cursor entered this machine");
        }

        Frame::Leave => {
            // Release before the host starts sending to someone else, or a
            // modifier held during the crossing stays down here forever.
            let _ = backend.inject.release_all();
            ownership.cursor_here = false;
            ownership.apply(backend);
            if let Some(status) = status {
                status.update(|s| s.cursor_on = None);
            }
            tracing::debug!("cursor left this machine");

            if config.options.lock_screen_on_leave {
                if let Err(err) = backend.lock.lock() {
                    tracing::warn!(%err, "could not lock the screen");
                }
            }
        }

        Frame::Arrangement(placements) => {
            let layout = tether_core::layout::Layout {
                machines: placements
                    .into_iter()
                    .map(|p| tether_core::layout::Placement {
                        machine: tether_core::layout::MachineId(p.machine),
                        name: p.name,
                        platform: p.platform,
                        origin: p.origin,
                        monitors: p.monitors,
                    })
                    .collect(),
            };
            tracing::debug!(machines = layout.machines.len(), "arrangement received");
            if let Some(status) = status {
                status.update(|s| s.layout = layout.clone());
            }
        }

        Frame::ReleaseAll => {
            let _ = backend.inject.release_all();
        }

        Frame::Input(event) => {
            // Already remapped by the host, which knows both platforms and
            // holds any per-machine overrides. Scroll shaping is ours though:
            // which way is up, and how far a detent goes, are properties of
            // the machine being scrolled rather than the one with the wheel.
            let event = config.options.shape_incoming(event);
            if let Err(err) = backend.inject.inject(&event) {
                tracing::warn!(%err, "injection failed");
            }
        }

        Frame::InputOwner { machine } => {
            let mine = machine == my_machine;
            if mine != ownership.owns_input {
                tracing::info!(
                    driving = mine,
                    "this machine is {} driving",
                    if mine { "now" } else { "no longer" }
                );
            }
            ownership.owns_input = mine;
            if let Some(status) = status {
                let owner = tether_core::layout::MachineId(machine);
                status.update(|s| s.input_owner = Some(owner));
            }
            // Whichever way it went, drop anything we were holding: a key held
            // as control moved would otherwise stay down on one side.
            let _ = backend.inject.release_all();
            ownership.apply(backend);
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
