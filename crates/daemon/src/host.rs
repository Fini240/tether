//! The host: owns the physical keyboard and mouse, routes them to whoever has
//! the cursor.
//!
//! Everything that mutates session state happens in one task, driven by a
//! single event channel. Connection tasks only translate bytes into
//! `HostEvent`s. That keeps the router — the part with the interesting bugs —
//! free of locks and reproducible from a plain list of events.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use tether_core::config::{Config, PairedPeer};
use tether_core::hotkey::Action;
use tether_core::keymap::ModifierMap;
use tether_core::layout::{Located, MachineId};
use tether_core::transition::{CursorRouter, Transition};
use tether_net::discovery::Advertiser;
use tether_net::server::Listener;
use tether_net::tls::TrustStore;
use tether_net::Identity;
use tether_platform::{Backend, BackendKind, LocalEvent};
use tether_proto::{
    ClipboardContents, Frame, InputEvent, MachinePlacement, Platform, Point, SourceEvent,
};

use crate::control::{Command, DaemonControl, PeerInfo};
use crate::session;
use crate::Outcome;

pub struct Options {
    pub bind: String,
    pub pairing: bool,
    pub config_path: PathBuf,
    /// Advertise over mDNS. Off in tests, which do not want to touch the LAN.
    pub advertise: bool,
    /// Fires with the actually-bound address once listening. Lets a caller that
    /// passed port 0 learn which port it got.
    pub ready: Option<tokio::sync::oneshot::Sender<std::net::SocketAddr>>,
    /// Publishes status and receives commands. `None` when run headlessly from
    /// the CLI, where nothing is watching.
    pub control: Option<DaemonControl>,
    /// Goes true when a machine with a stronger claim to arbitrate turns up.
    ///
    /// Only `auto` sets it, and it is honoured only while no client is
    /// connected: handing the job over is cheap when nobody is relying on us
    /// and rude once somebody is. `None` for a role the user chose, which is
    /// never taken away.
    pub supersede: Option<tokio::sync::watch::Receiver<bool>>,
}

/// Everything the session loop reacts to.
enum HostEvent {
    /// The physical keyboard or mouse did something.
    Local(LocalEvent),
    /// A client finished the handshake.
    Joined(Box<ClientHandle>),
    /// A frame arrived from a connected client.
    FromClient(MachineId, Frame),
    /// A client's connection ended.
    Gone(MachineId),
}

struct ClientHandle {
    machine: MachineId,
    name: String,
    platform: Platform,
    fingerprint: String,
    address: String,
    /// The client's displays, in its own coordinate space.
    monitors: Vec<tether_proto::MonitorInfo>,
    /// Frames queued for this client. Unbounded: a slow client must never
    /// block the router, and dropping input frames would leave stuck keys.
    tx: UnboundedSender<Frame>,
}

struct Client {
    name: String,
    platform: Platform,
    address: String,
    tx: UnboundedSender<Frame>,
    /// Modifier translation for events sent *to* this client.
    keymap: ModifierMap,
    /// ...and for events arriving *from* it, when it is the one driving.
    keymap_in: ModifierMap,
}

pub async fn run(
    mut options: Options,
    mut config: Config,
    identity: Identity,
    backend: &mut Backend,
) -> Result<Outcome> {
    if backend.kind == BackendKind::Native {
        // Fail here rather than after the tap silently swallows everything.
        tether_platform::check_capture_permission()
            .context("cannot capture input on this machine")?;
    }

    let monitors = backend
        .monitors
        .enumerate()
        .context("could not enumerate this machine's displays")?;
    tracing::info!(count = monitors.len(), "host displays detected");

    // Seed the canvas with this machine, keeping any saved arrangement.
    let mut layout = config.layout.clone();
    let local = session::local_placement(&config, &identity, monitors);
    if layout.contains(identity.machine_id) {
        layout.upsert(local);
    } else {
        layout.auto_place(local);
    }

    let mut router = CursorRouter::new(layout, identity.machine_id);
    router.set_locked(config.options.cursor_lock_on_start);

    let trust = TrustStore::new(config.peers.iter().map(|p| p.fingerprint.clone()));
    trust.set_pairing(options.pairing);
    if options.pairing {
        tracing::warn!(
            fingerprint = %identity.display_fingerprint(),
            "pairing mode is ON — any machine on this network may connect. \
             Compare this fingerprint on the client, then restart without --pair."
        );
    }

    let listener = Listener::bind(&options.bind, &identity, trust.clone())
        .await
        .with_context(|| format!("could not listen on {}", options.bind))?;
    let local_addr = listener.local_addr();
    tracing::info!(%local_addr, "host listening");

    let control = options.control.take();
    if let Some(control) = &control {
        control.status.update(|status| {
            status.running = true;
            status.role = Some(tether_core::config::Role::Host);
            status.listening = Some(local_addr.to_string());
            status.this_machine = Some(identity.machine_id);
            status.input_owner = Some(identity.machine_id);
            status.error = None;
            status.detail = format!("listening on {local_addr}");
        });
    }

    // Advertising is a convenience; a host that cannot do mDNS is still usable
    // with an explicit --host on the client, so a failure here is not fatal.
    let _advertiser = if options.advertise {
        match Advertiser::start(
            &config.name,
            local_addr.port(),
            &identity.fingerprint,
            &Platform::current().to_string(),
            identity.machine_id.0,
        ) {
            Ok(advertiser) => Some(advertiser),
            Err(err) => {
                tracing::warn!(%err, "not advertising; clients must connect with --host");
                None
            }
        }
    } else {
        None
    };

    if let Some(ready) = options.ready.take() {
        let _ = ready.send(local_addr);
    }

    let (events_tx, mut events) = mpsc::unbounded_channel::<HostEvent>();

    // Capture pushes LocalEvents; adapt them into the single event stream.
    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<LocalEvent>();
    backend
        .capture
        .start(input_tx)
        .context("could not start input capture")?;
    {
        let events_tx = events_tx.clone();
        tokio::spawn(async move {
            while let Some(event) = input_rx.recv().await {
                if events_tx.send(HostEvent::Local(event)).is_err() {
                    break;
                }
            }
        });
    }

    tokio::spawn(accept_loop(
        listener,
        events_tx.clone(),
        config.clone(),
        identity.clone(),
    ));

    let mut clients: HashMap<MachineId, Client> = HashMap::new();
    // Which machine's physical keyboard and mouse are driving. Starts here;
    // moves to whichever machine the user actually touches.
    let mut input_owner = identity.machine_id;
    let mut heartbeat = tokio::time::interval(Duration::from_millis(
        config.options.heartbeat_ms.max(250) as u64,
    ));
    let mut clipboard_poll = tokio::time::interval(Duration::from_millis(500));
    let mut clipboard_seq: u64 = 0;

    tracing::info!("ready — move the pointer off a screen edge to cross machines");

    let mut outcome = Outcome::Stopped;

    // Held apart so the loop can await a command and publish status in the
    // same iteration.
    let (status, mut commands) = match control {
        Some(control) => (Some(control.status), Some(control.commands)),
        None => (None, None),
    };

    loop {
        tokio::select! {
            biased;

            _ = session::shutdown_signal() => {
                tracing::info!("shutting down");
                break;
            }

            // Guarded on having no clients: once a machine is relying on this
            // one, the tie-break stops being worth a disconnection. The signal
            // stays pending rather than being consumed, so it is still there
            // when the last client leaves.
            _ = stronger_claim(&mut options.supersede), if clients.is_empty() => {
                tracing::info!("another machine should be arbitrating; standing down");
                outcome = Outcome::Supersede;
                break;
            }

            Some(event) = events.recv() => {
                handle_event(
                    event,
                    &mut router,
                    &mut clients,
                    &mut config,
                    &identity,
                    backend,
                    &options,
                    &trust,
                    &mut input_owner,
                )?;
                publish(&status, &router, &clients, input_owner, identity.machine_id);
            }

            Some(command) = async { match &mut commands {
                Some(rx) => rx.recv().await,
                // No UI attached: park forever so this arm never fires rather
                // than spinning on a `None` that resolves instantly.
                None => std::future::pending().await,
            } } => {
                match command {
                    Command::Stop => {
                        tracing::info!("stopping at the interface's request");
                        break;
                    }
                    Command::SetLayout(layout) => {
                        tracing::info!("arrangement changed");
                        config.layout = layout.clone();
                        let recalled = router.set_layout(layout);
                        if let Err(err) = config.save(&options.config_path) {
                            tracing::warn!(%err, "could not save the arrangement");
                        }
                        broadcast_arrangement(&clients, &router);
                        // Rearranging the canvas out from under the pointer
                        // brings it home. Tell whoever was holding it, and
                        // stop swallowing input that now has nowhere to go.
                        if let Some(from) = recalled {
                            release_holder(from, &clients);
                            reclaim_cursor(&mut router, backend);
                        }
                    }
                    Command::JumpTo(machine) => {
                        let transition = router.jump_to(machine);
                        apply_transition(transition, &router, &clients, backend, input_owner);
                    }
                    Command::ToggleCursorLock => {
                        let locked = router.toggle_lock();
                        tracing::info!(locked, "cursor lock toggled");
                    }
                }
                publish(&status, &router, &clients, input_owner, identity.machine_id);
            }

            _ = heartbeat.tick() => {
                let now = heartbeat_token();
                // Collect first, evict through the ordinary disconnect path
                // second. Dropping the entry here and nothing else would leave
                // the cursor routed at a machine no longer in the map: every
                // move would be sent into a closed socket, and this host would
                // go on suppressing its own input with the pointer nowhere.
                let dead: Vec<MachineId> = clients
                    .iter()
                    .filter(|(_, client)| client.tx.send(Frame::Ping(now)).is_err())
                    .map(|(machine, _)| *machine)
                    .collect();
                for machine in dead {
                    tracing::info!(%machine, "client writer gone");
                    handle_event(
                        HostEvent::Gone(machine),
                        &mut router,
                        &mut clients,
                        &mut config,
                        &identity,
                        backend,
                        &options,
                        &trust,
                        &mut input_owner,
                    )?;
                }
                publish(&status, &router, &clients, input_owner, identity.machine_id);
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
                        tracing::debug!(formats = ?contents.formats(), "local clipboard changed");
                        broadcast(&clients, Frame::ClipboardOffer {
                            stamp: contents.stamp,
                            formats: contents.formats(),
                        });
                        // Held for the pull that follows the offer.
                        pending_clipboard_set(backend, contents);
                    }
                    Ok(None) => {}
                    Err(err) => tracing::debug!(%err, "clipboard poll failed"),
                }
            }
        }
    }

    // Leave every machine in a sane state: no stuck modifiers, cursor visible,
    // input reaching the host again.
    for (machine, client) in &clients {
        let _ = client.tx.send(Frame::ReleaseAll);
        let _ = client.tx.send(Frame::Bye("host shutting down".into()));
        tracing::debug!(%machine, "said goodbye");
    }
    backend.capture.set_swallow(false);
    pin_cursor(backend, false);
    let _ = backend.pointer.set_visible(true);
    backend.capture.stop();

    config.layout = router.layout().clone();
    if let Err(err) = config.save(&options.config_path) {
        tracing::warn!(%err, "could not save the config");
    }

    if let Some(status) = &status {
        status.update(|s| {
            s.running = false;
            s.peers.clear();
            s.detail = "stopped".into();
        });
    }

    Ok(outcome)
}

/// The host's own clipboard is the source of truth; this just records the last
/// broadcast contents so a `ClipboardRequest` can be answered.
fn pending_clipboard_set(backend: &mut Backend, contents: ClipboardContents) {
    // Writing it straight back is a no-op locally (it is already there) but
    // keeps `poll_change` from re-reporting it on the next tick.
    let _ = backend.clipboard.write(&contents);
}

fn heartbeat_token() -> u64 {
    // Monotonic-ish and cheap; only used to correlate a Pong with its Ping.
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// Tell every client where all the screens are.
///
/// Clients have no say in the canvas — the host owns it — but without this a
/// client's window shows only its own screen, with no way to tell whether the
/// other machine is attached or where its edges lead.
fn broadcast_arrangement(clients: &HashMap<MachineId, Client>, router: &CursorRouter) {
    if clients.is_empty() {
        return;
    }
    let placements: Vec<MachinePlacement> = router
        .layout()
        .machines
        .iter()
        .map(|p| MachinePlacement {
            machine: p.machine.0,
            name: p.name.clone(),
            platform: p.platform,
            origin: p.origin,
            monitors: p.monitors.clone(),
        })
        .collect();
    broadcast(clients, Frame::Arrangement(placements));
}

fn broadcast(clients: &HashMap<MachineId, Client>, frame: Frame) {
    for client in clients.values() {
        let _ = client.tx.send(frame.clone());
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_event(
    event: HostEvent,
    router: &mut CursorRouter,
    clients: &mut HashMap<MachineId, Client>,
    config: &mut Config,
    identity: &Identity,
    backend: &mut Backend,
    options: &Options,
    trust: &TrustStore,
    input_owner: &mut MachineId,
) -> Result<()> {
    match event {
        HostEvent::Local(local) => {
            // The host's own keyboard was touched. If somebody else was
            // driving, take it back — the machine you are physically at wins.
            if *input_owner != identity.machine_id
                && config.options.auto_input_handoff
                && is_deliberate(&local)
            {
                set_input_owner(
                    identity.machine_id,
                    input_owner,
                    router,
                    clients,
                    config,
                    backend,
                );
            }
            if *input_owner != identity.machine_id {
                return Ok(());
            }
            handle_local(local, router, clients, config, backend, *input_owner)
        }

        HostEvent::Joined(handle) => {
            let handle = *handle;
            tracing::info!(
                machine = %handle.machine,
                name = %handle.name,
                platform = %handle.platform,
                address = %handle.address,
                "client joined"
            );

            // Record the pairing so the next run does not need --pair.
            if config.peer(handle.machine).is_none() {
                println!(
                    "\n  Paired with {} ({})\n  fingerprint {}\n  Verify this matches `tether id` on that machine.\n",
                    handle.name,
                    handle.machine,
                    tether_net::identity::group_fingerprint(&handle.fingerprint)
                );
            }
            config.add_peer(PairedPeer {
                machine: handle.machine,
                name: handle.name.clone(),
                fingerprint: handle.fingerprint.clone(),
                last_address: Some(handle.address.clone()),
            });
            trust.allow(handle.fingerprint.clone());

            // Put it on the canvas. A saved arrangement wins; a machine seen
            // for the first time is appended to the right of everything else,
            // which is the wizard's proposal until the user drags it.
            let mut layout = router.layout().clone();
            let placement = session::placement_for(
                handle.machine,
                handle.name.clone(),
                handle.platform,
                handle.monitors.clone(),
                config
                    .layout
                    .get(handle.machine)
                    .map(|saved| saved.origin)
                    .unwrap_or(Point::new(0, 0)),
            );
            if layout.contains(handle.machine) || config.layout.contains(handle.machine) {
                layout.upsert(placement);
            } else {
                layout.auto_place(placement);
            }
            if let Some(placed) = layout.get(handle.machine) {
                let bounds = placed.global_bounds();
                tracing::info!(
                    machine = %handle.machine,
                    x = bounds.x, y = bounds.y,
                    width = bounds.width, height = bounds.height,
                    "placed on the canvas"
                );
            }
            let recalled = router.set_layout(layout.clone());
            config.layout = layout;
            if let Some(from) = recalled {
                release_holder(from, clients);
                reclaim_cursor(router, backend);
            }

            // Give it a switch hotkey if it does not have one, so "jump to that
            // machine" works without opening a config file.
            assign_switch_hotkey(config, handle.machine);

            // Persist now, not at shutdown. The pairing, the placement on the
            // canvas and the hotkey are all set — and a daemon that is killed
            // rather than asked to stop would otherwise lose all three.
            if let Err(err) = config.save(&options.config_path) {
                tracing::warn!(%err, "could not persist the new pairing");
            }

            let keymap =
                config.modifier_map_for(handle.machine, Platform::current(), handle.platform);
            // The reverse direction, for when this client is the one driving.
            let keymap_in =
                config.modifier_map_for(handle.machine, handle.platform, Platform::current());
            if !keymap.is_identity() {
                tracing::info!(
                    machine = %handle.machine,
                    "remapping Control and Meta for this client"
                );
            }

            clients.insert(
                handle.machine,
                Client {
                    name: handle.name,
                    platform: handle.platform,
                    address: handle.address.clone(),
                    tx: handle.tx,
                    keymap,
                    keymap_in,
                },
            );
            broadcast_arrangement(clients, router);
            Ok(())
        }

        HostEvent::FromClient(machine, frame) => handle_client_frame(
            machine,
            frame,
            clients,
            backend,
            config,
            identity,
            router,
            input_owner,
        ),

        HostEvent::Gone(machine) => {
            let gone = clients.remove(&machine);
            let name = gone
                .as_ref()
                .map(|c| c.name.clone())
                .unwrap_or_else(|| machine.to_string());
            let platform = gone
                .as_ref()
                .map(|c| c.platform.to_string())
                .unwrap_or_else(|| "?".into());
            tracing::info!(%machine, %name, %platform, "client disconnected");

            // If it held the cursor, take it back — otherwise input would keep
            // being routed into a closed socket and the user would have no
            // pointer at all.
            let mut layout = router.layout().clone();
            layout.remove(machine);
            let had_cursor = router.active() == machine;
            // The recall is expected here and needs no `Leave`: the machine it
            // would go to is the one that just disconnected.
            let _ = router.set_layout(layout);
            broadcast_arrangement(clients, router);
            if had_cursor {
                reclaim_cursor(router, backend);
            }
            // If the machine that vanished was the one driving, take input
            // back here, or the keyboard on this desk stays dead.
            if *input_owner == machine {
                set_input_owner(
                    identity.machine_id,
                    input_owner,
                    router,
                    clients,
                    config,
                    backend,
                );
            }
            Ok(())
        }
    }
}

/// Copy the live session state into the published snapshot.
fn publish(
    status: &Option<crate::control::StatusHandle>,
    router: &CursorRouter,
    clients: &HashMap<MachineId, Client>,
    input_owner: MachineId,
    this: MachineId,
) {
    let Some(status) = status else { return };

    let peers: Vec<PeerInfo> = clients
        .iter()
        .map(|(machine, client)| PeerInfo {
            machine: *machine,
            name: client.name.clone(),
            platform: client.platform,
            address: client.address.clone(),
            connected: true,
        })
        .collect();

    let detail = match peers.len() {
        0 => "waiting for a machine to connect".to_string(),
        1 => format!("{} connected", peers[0].name),
        n => format!("{n} machines connected"),
    };

    status.update(|s| {
        s.running = true;
        s.peers = peers;
        s.layout = router.layout().clone();
        s.input_owner = Some(input_owner);
        s.cursor_on = Some(router.active());
        s.cursor_position = Some(router.position());
        s.cursor_locked = router.is_locked();
        s.this_machine = Some(this);
        s.detail = detail;
    });
}

/// Resolves when someone else should be arbitrating, and never otherwise.
///
/// Pends forever when there is no signal, so the `select!` arm holding it
/// simply never fires for a role the user picked by hand.
async fn stronger_claim(supersede: &mut Option<tokio::sync::watch::Receiver<bool>>) {
    let Some(rx) = supersede else {
        return std::future::pending().await;
    };
    loop {
        if *rx.borrow_and_update() {
            return;
        }
        if rx.changed().await.is_err() {
            // The supervisor is gone, which means nothing will ever ask us to
            // stand down. Wait forever rather than spinning on a dead channel.
            return std::future::pending().await;
        }
    }
}

/// Tell a client that no longer has the pointer to let go of what it is
/// holding. Silent if it has already disconnected — there is nobody to tell.
fn release_holder(from: MachineId, clients: &HashMap<MachineId, Client>) {
    if let Some(client) = clients.get(&from) {
        let _ = client.tx.send(Frame::ReleaseAll);
        let _ = client.tx.send(Frame::Leave);
    }
}

/// Bring the cursor back to the host and restore local input.
fn reclaim_cursor(router: &mut CursorRouter, backend: &mut Backend) {
    if let Some(located) = router.recall_to_host() {
        // Warp first, unpin second: re-attaching the mouse to a cursor that is
        // still parked wherever it was frozen shows it in the wrong place for
        // as long as it takes the warp to land.
        let _ = backend.pointer.warp(located.local);
    }
    backend.capture.set_swallow(false);
    pin_cursor(backend, false);
    let _ = backend.pointer.set_visible(true);
    tracing::info!("cursor is back on the host");
}

/// Is this worth taking control for?
///
/// A single pixel of mouse drift or a stray scroll should not yank control
/// away from the machine you are typing on. A keypress, a click, or a real
/// movement should.
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

/// Hand the physical-input role to `next`.
fn set_input_owner(
    next: MachineId,
    input_owner: &mut MachineId,
    router: &mut CursorRouter,
    clients: &HashMap<MachineId, Client>,
    config: &Config,
    backend: &mut Backend,
) {
    if *input_owner == next {
        return;
    }
    let previous = *input_owner;
    *input_owner = next;
    tracing::info!(from = %previous, to = %next, "input handed over");

    // Whoever was driving may be holding keys down. Let go of them everywhere
    // before the new owner starts sending its own.
    let _ = backend.inject.release_all();
    broadcast(clients, Frame::ReleaseAll);
    broadcast(clients, Frame::InputOwner { machine: next.0 });

    if config.options.cursor_follows_input {
        // Bring the pointer to the machine being touched. Without this,
        // touching the Mac's trackpad drives a cursor still sitting on the PC,
        // which feels broken even though it is working exactly as designed.
        let transition = router.jump_to(next);
        apply_transition(transition, router, clients, backend, next);
    }

    update_host_swallow(router, backend, next);
}

/// The host suppresses its own input only while it is the one driving *and*
/// the cursor is somewhere else. Any other time, local input must reach local
/// apps — including so the user can touch this machine to take control back.
fn update_host_swallow(router: &CursorRouter, backend: &mut Backend, input_owner: MachineId) {
    let owns = input_owner == router.host();
    let cursor_here = router.active() == router.host();
    let swallow = owns && !cursor_here;

    if swallow {
        // Hide, pin, swallow — in that order, and mirrored on the way back.
        //
        // Hiding first because it is the step that talks to the window server
        // about the cursor, and the pin is the state that must survive.
        // Swallowing last because both orders leave a one-event window and
        // this is the harmless one: pinned-but-not-swallowed loses a flick of
        // movement, while swallowed-but-not-pinned is the original complaint —
        // the local cursor sliding away with nothing reporting where it went.
        let _ = backend.pointer.set_visible(false);
        pin_cursor(backend, true);
        backend.capture.set_swallow(true);
    } else {
        backend.capture.set_swallow(false);
        pin_cursor(backend, false);
        let _ = backend.pointer.set_visible(true);
    }
}

/// Freeze or release this machine's own cursor, complaining once if the
/// platform refuses.
///
/// Not fatal: without it the pointer still routes correctly, the host's cursor
/// just wanders about while somebody drives another machine. Worth a warning
/// rather than a silent shrug, because that wandering is what the user sees.
fn pin_cursor(backend: &Backend, pinned: bool) {
    if let Err(err) = backend.pointer.set_pinned(pinned) {
        tracing::warn!(%err, pinned, "could not pin the cursor");
    }
}

fn handle_local(
    local: LocalEvent,
    router: &mut CursorRouter,
    clients: &mut HashMap<MachineId, Client>,
    config: &Config,
    backend: &mut Backend,
    input_owner: MachineId,
) -> Result<()> {
    match local {
        LocalEvent::MouseMoved { x, y, dx, dy } => {
            // Position *and* device movement, and the router needs both: the
            // position carries any sideways slide the OS performed crossing
            // between screens of different heights, and whatever the OS
            // refused to apply at the outer edge is the push that crosses to
            // the next machine. Applying both in full moves the cursor twice
            // per event — see `CursorRouter::move_reported`.
            let transition = router.move_reported(input_owner, Point::new(x, y), dx, dy);
            apply_transition(transition, router, clients, backend, input_owner);
            Ok(())
        }

        LocalEvent::MouseDelta { dx, dy } => {
            let transition = router.move_by(dx, dy);
            apply_transition(transition, router, clients, backend, input_owner);
            Ok(())
        }

        LocalEvent::Button { button, pressed } => {
            deliver(
                router,
                clients,
                backend,
                config,
                InputEvent::MouseButton { button, pressed },
                input_owner,
            );
            Ok(())
        }

        LocalEvent::Wheel { dx, dy } => {
            deliver(
                router,
                clients,
                backend,
                config,
                InputEvent::MouseWheel { dx, dy },
                input_owner,
            );
            Ok(())
        }

        LocalEvent::Key {
            key,
            pressed,
            modifiers,
            repeat,
        } => {
            if let Some(action) = config.hotkeys.lookup(key, modifiers, pressed) {
                // TODO(hotkey-suppression): while the cursor is on the host we
                // are not swallowing, so the chord also reaches the local app.
                // Fixing it needs a per-event "drop this one" path into the
                // capture backend rather than the current global switch.
                run_action(action.clone(), router, clients, backend, input_owner);
                return Ok(());
            }

            deliver(
                router,
                clients,
                backend,
                config,
                InputEvent::Key {
                    key,
                    pressed,
                    modifiers,
                    repeat,
                },
                input_owner,
            );
            Ok(())
        }
    }
}

/// Deliver an event to whichever machine holds the cursor.
///
/// Three cases, and getting the third wrong is what made a client-driven
/// session look broken:
///
/// * the cursor is on the machine being physically touched — its own OS has
///   already delivered the event, and we are not suppressing there, so
///   injecting would double it;
/// * the cursor is on a client — send it;
/// * the cursor is on *this* machine while a client is driving — nothing local
///   is producing these events, so this machine has to inject them itself.
///
/// That last case used to return early, left over from when the host was
/// always the machine being touched. The symptom was a pointer that moved only
/// at the moment it crossed onto the host and then sat still, which reads as
/// severe lag rather than as nothing being delivered at all.
fn deliver(
    router: &CursorRouter,
    clients: &HashMap<MachineId, Client>,
    backend: &mut Backend,
    config: &Config,
    event: InputEvent,
    input_owner: MachineId,
) {
    let active = router.active();
    if active == input_owner {
        return;
    }

    if active == router.host() {
        // Translate into this machine's terms on the way in — ⌘C from a Mac
        // has to arrive here as Ctrl+C.
        let event = match clients.get(&input_owner) {
            Some(owner) => owner.keymap_in.remap_event(event),
            None => event,
        };
        // How scrolling feels is decided where it lands, not where the wheel
        // turned. Applied after the keymap so it sees the finished event.
        let event = config.options.shape_incoming(event);
        if let Err(err) = backend.inject.inject(&event) {
            tracing::warn!(%err, "could not inject locally");
        }
        return;
    }

    let Some(client) = clients.get(&active) else {
        return;
    };
    // Remap on the way out: each client gets the translation appropriate to
    // its own platform.
    let translated = client.keymap.remap_event(event);
    let _ = client.tx.send(Frame::Input(translated));
}

fn apply_transition(
    transition: Transition,
    router: &CursorRouter,
    clients: &HashMap<MachineId, Client>,
    backend: &mut Backend,
    input_owner: MachineId,
) {
    // Derived from where the router now has the pointer, and recomputed on
    // every event rather than assigned at the crossings. Set only when
    // crossing, it can be left contradicting the router — suppressing input
    // that has nowhere to go — and nothing after that recomputes it. Doing it
    // here also gets it done before the switch is announced, so no stray local
    // event lands on this machine in between.
    update_host_swallow(router, backend, input_owner);

    match transition {
        Transition::Blocked => {
            // Worth seeing: a move refused because the canvas has a gap there,
            // or because the pointer is at the outer edge, is indistinguishable
            // from a broken connection unless you are told.
            tracing::debug!(
                at = ?router.position(),
                on = %router.active(),
                "movement refused — no screen that way"
            );
        }

        Transition::Stay(located) => {
            // Only the machine being physically touched moves its own cursor
            // natively; injecting there would double the motion, and leaving
            // it alone is what keeps it usable if the link drops. Everywhere
            // else — including this machine when a client is driving — has to
            // be moved explicitly.
            if located.machine == input_owner {
                return;
            }
            if located.machine == router.host() {
                let _ = backend.inject.inject(&InputEvent::MouseMove {
                    x: located.local.x,
                    y: located.local.y,
                });
                return;
            }
            send_move(clients, located);
        }

        Transition::Switch { from, to } => {
            tracing::debug!(%from, to = %to.machine, "cursor crossed");

            // Whatever we are leaving must not be left holding keys down.
            if from != router.host() {
                release_holder(from, clients);
            } else {
                let _ = backend.inject.release_all();
            }

            if to.machine == router.host() {
                let _ = backend.pointer.warp(to.local);
            } else if let Some(client) = clients.get(&to.machine) {
                let _ = client.tx.send(Frame::Enter {
                    monitor: to.monitor,
                    x: to.local.x,
                    y: to.local.y,
                });
            }
        }
    }
}

fn send_move(clients: &HashMap<MachineId, Client>, located: Located) {
    if let Some(client) = clients.get(&located.machine) {
        let _ = client.tx.send(Frame::Input(InputEvent::MouseMove {
            x: located.local.x,
            y: located.local.y,
        }));
    }
}

fn run_action(
    action: Action,
    router: &mut CursorRouter,
    clients: &HashMap<MachineId, Client>,
    backend: &mut Backend,
    input_owner: MachineId,
) {
    match action {
        Action::ToggleLock => {
            let locked = router.toggle_lock();
            tracing::info!(locked, "cursor lock toggled");
        }
        Action::RecallCursor => {
            let from = router.active();
            if from != router.host() {
                release_holder(from, clients);
            }
            reclaim_cursor(router, backend);
        }
        Action::SwitchTo { machine } => {
            let transition = router.jump_to(machine);
            apply_transition(transition, router, clients, backend, input_owner);
        }
        Action::LockAllScreens => {
            broadcast(clients, Frame::LockScreen);
            if let Err(err) = backend.lock.lock() {
                tracing::warn!(%err, "could not lock this screen");
            }
        }
        Action::PushClipboard => match backend.clipboard.read() {
            Ok(contents) => broadcast(clients, Frame::ClipboardData(contents)),
            Err(err) => tracing::warn!(%err, "could not read the clipboard"),
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_client_frame(
    machine: MachineId,
    frame: Frame,
    clients: &mut HashMap<MachineId, Client>,
    backend: &mut Backend,
    config: &Config,
    identity: &Identity,
    router: &mut CursorRouter,
    input_owner: &mut MachineId,
) -> Result<()> {
    match frame {
        Frame::ClaimInput => {
            if config.options.auto_input_handoff {
                set_input_owner(machine, input_owner, router, clients, config, backend);
            } else {
                tracing::debug!(%machine, "ignoring an input claim; handoff is disabled");
            }
        }

        Frame::SourceInput(event) => {
            // Only from whoever currently holds the role. A late frame from the
            // previous owner arriving after a handover must not move anything.
            if *input_owner != machine {
                return Ok(());
            }
            let local = match event {
                SourceEvent::MouseDelta { dx, dy } => LocalEvent::MouseDelta { dx, dy },
                SourceEvent::MouseMoved { x, y, dx, dy } => LocalEvent::MouseMoved { x, y, dx, dy },
                SourceEvent::Button { button, pressed } => LocalEvent::Button { button, pressed },
                SourceEvent::Wheel { dx, dy } => LocalEvent::Wheel { dx, dy },
                SourceEvent::Key {
                    key,
                    pressed,
                    modifiers,
                    repeat,
                } => LocalEvent::Key {
                    key,
                    pressed,
                    modifiers,
                    repeat,
                },
            };
            handle_local(local, router, clients, config, backend, *input_owner)?;
        }

        Frame::Ping(token) => {
            if let Some(client) = clients.get(&machine) {
                let _ = client.tx.send(Frame::Pong(token));
            }
        }
        Frame::Pong(_) => {}

        Frame::ClipboardOffer { stamp, formats } => {
            if !config.options.sync_clipboard || stamp.owner == identity.machine_id.0 {
                return Ok(());
            }
            tracing::debug!(%machine, ?formats, "client offered a clipboard");
            if let Some(client) = clients.get(&machine) {
                // TODO(lazy-paste): pull immediately for now. Deferring until a
                // paste actually happens needs paste detection per platform;
                // until then a large image copy does cross the wire eagerly.
                let _ = client.tx.send(Frame::ClipboardRequest { stamp, formats });
            }
        }

        Frame::ClipboardRequest { .. } => match backend.clipboard.read() {
            Ok(contents) => {
                if let Some(client) = clients.get(&machine) {
                    let _ = client.tx.send(Frame::ClipboardData(contents));
                }
            }
            Err(err) => tracing::warn!(%err, "could not read the clipboard"),
        },

        Frame::ClipboardData(contents) => {
            if !config.options.sync_clipboard {
                return Ok(());
            }
            if let Err(err) = backend.clipboard.write(&contents) {
                tracing::warn!(%err, "could not write the clipboard");
            } else {
                tracing::info!(%machine, "clipboard received");
            }
        }

        Frame::MonitorsChanged(monitors) => {
            tracing::info!(%machine, count = monitors.len(), "client displays changed");
            // TODO(relayout): rebuild the canvas for this machine and push the
            // new arrangement. Until then the old geometry stays in use, which
            // means an edge may land in the wrong place after a resolution
            // change; reconnecting the client fixes it.
        }

        Frame::FileOffer { transfer, name, .. } => {
            tracing::info!(%machine, %name, "refusing a file offer");
            if let Some(client) = clients.get(&machine) {
                let _ = client.tx.send(Frame::FileAbort {
                    transfer,
                    reason: "file transfer is not implemented yet".into(),
                });
            }
        }

        Frame::Bye(reason) => {
            tracing::info!(%machine, %reason, "client said goodbye");
            clients.remove(&machine);
        }

        other => tracing::debug!(%machine, ?other, "ignoring an unexpected frame"),
    }
    Ok(())
}

/// Accept clients forever, handshaking each and reporting it as `Joined`.
async fn accept_loop(
    listener: Listener,
    events: UnboundedSender<HostEvent>,
    config: Config,
    identity: Identity,
) {
    loop {
        let accepted = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(err) => {
                // One rejected machine retrying in a loop must not kill the
                // host, so log and keep accepting.
                tracing::warn!(%err, "rejected an incoming connection");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };

        let events = events.clone();
        let config = config.clone();
        let identity = identity.clone();
        tokio::spawn(async move {
            if let Err(err) = handshake_client(accepted, events, config, identity).await {
                tracing::warn!(%err, "client handshake failed");
            }
        });
    }
}

async fn handshake_client(
    accepted: tether_net::server::Accepted,
    events: UnboundedSender<HostEvent>,
    config: Config,
    identity: Identity,
) -> Result<()> {
    let address = accepted.addr.to_string();
    let fingerprint = accepted.fingerprint;
    let mut connection = accepted.connection;

    let hello = match connection.next().await {
        Some(Ok(Frame::Hello(hello))) => hello,
        Some(Ok(other)) => anyhow::bail!("expected Hello, got {other:?}"),
        Some(Err(err)) => return Err(err.into()),
        None => anyhow::bail!("client closed before saying Hello"),
    };

    if let Err(err) = session::check_version(hello.protocol_version) {
        let _ = connection.send(Frame::Rejected(err.to_string())).await;
        return Err(err);
    }

    // The Hello is self-reported; the certificate is not. Bind them, or a
    // paired machine could claim to be a different paired machine.
    let machine = MachineId(hello.machine_id);
    if let Some(known) = config.peer(machine) {
        if !known.fingerprint.eq_ignore_ascii_case(&fingerprint) {
            let message = format!(
                "machine {machine} presented fingerprint {fingerprint}, but {} is on record",
                known.fingerprint
            );
            let _ = connection.send(Frame::Rejected(message.clone())).await;
            anyhow::bail!(message);
        }
    }

    connection
        .send(Frame::Welcome(session::welcome(&config, &identity)))
        .await?;

    let (mut sink, mut stream) = connection.split();
    let (tx, mut rx): (UnboundedSender<Frame>, UnboundedReceiver<Frame>) =
        mpsc::unbounded_channel();

    events
        .send(HostEvent::Joined(Box::new(ClientHandle {
            machine,
            name: hello.name.clone(),
            platform: hello.platform,
            fingerprint,
            address,
            monitors: hello.monitors.clone(),
            tx,
        })))
        .map_err(|_| anyhow::anyhow!("host loop is gone"))?;

    // Writer.
    let writer = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            if sink.send(frame).await.is_err() {
                break;
            }
        }
    });

    // Reader — owns the "connection ended" signal.
    while let Some(frame) = stream.next().await {
        match frame {
            Ok(frame) => {
                if events.send(HostEvent::FromClient(machine, frame)).is_err() {
                    break;
                }
            }
            Err(err) => {
                tracing::debug!(%machine, %err, "read error");
                break;
            }
        }
    }

    writer.abort();
    let _ = events.send(HostEvent::Gone(machine));
    Ok(())
}

/// Bind `ctrl+alt+N` for a machine, using the lowest digit not already taken.
fn assign_switch_hotkey(config: &mut Config, machine: MachineId) {
    let already = config
        .hotkeys
        .bindings
        .iter()
        .any(|b| b.action == Action::SwitchTo { machine });
    if already {
        return;
    }

    for digit in 1..=9u8 {
        let Ok(hotkey) = format!("ctrl+alt+{digit}").parse::<tether_core::hotkey::Hotkey>() else {
            continue;
        };
        let taken = config.hotkeys.bindings.iter().any(|b| b.hotkey == hotkey);
        if !taken {
            config.hotkeys.bind(hotkey, Action::SwitchTo { machine });
            tracing::info!(%machine, hotkey = %hotkey, "bound a switch hotkey");
            return;
        }
    }
    tracing::debug!(%machine, "no free switch hotkey (ctrl+alt+1..9 are all taken)");
}
