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
use tether_proto::{ClipboardContents, Frame, InputEvent, Platform, Point, SourceEvent};

use crate::session;

pub struct Options {
    pub bind: String,
    pub pairing: bool,
    pub config_path: PathBuf,
    /// Advertise over mDNS. Off in tests, which do not want to touch the LAN.
    pub advertise: bool,
    /// Fires with the actually-bound address once listening. Lets a caller that
    /// passed port 0 learn which port it got.
    pub ready: Option<tokio::sync::oneshot::Sender<std::net::SocketAddr>>,
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
    tx: UnboundedSender<Frame>,
    /// Modifier translation for this client, resolved once at join.
    keymap: ModifierMap,
}

pub async fn run(
    mut options: Options,
    mut config: Config,
    identity: Identity,
    mut backend: Backend,
) -> Result<()> {
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

    // Advertising is a convenience; a host that cannot do mDNS is still usable
    // with an explicit --host on the client, so a failure here is not fatal.
    let _advertiser = if options.advertise {
        match Advertiser::start(
            &config.name,
            local_addr.port(),
            &identity.fingerprint,
            &Platform::current().to_string(),
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

    loop {
        tokio::select! {
            biased;

            _ = session::shutdown_signal() => {
                tracing::info!("shutting down");
                break;
            }

            Some(event) = events.recv() => {
                handle_event(
                    event,
                    &mut router,
                    &mut clients,
                    &mut config,
                    &identity,
                    &mut backend,
                    &options,
                    &trust,
                    &mut input_owner,
                )?;
            }

            _ = heartbeat.tick() => {
                let now = heartbeat_token();
                clients.retain(|machine, client| {
                    let alive = client.tx.send(Frame::Ping(now)).is_ok();
                    if !alive {
                        tracing::info!(%machine, "client writer gone");
                    }
                    alive
                });
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
                        pending_clipboard_set(&mut backend, contents);
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
    let _ = backend.pointer.set_visible(true);
    backend.capture.stop();

    config.layout = router.layout().clone();
    if let Err(err) = config.save(&options.config_path) {
        tracing::warn!(%err, "could not save the config");
    }

    Ok(())
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
            router.set_layout(layout.clone());
            config.layout = layout;

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
                    tx: handle.tx,
                    keymap,
                },
            );
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
            router.set_layout(layout);
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

/// Bring the cursor back to the host and restore local input.
fn reclaim_cursor(router: &mut CursorRouter, backend: &mut Backend) {
    if let Some(located) = router.recall_to_host() {
        let _ = backend.pointer.warp(located.local);
    }
    backend.capture.set_swallow(false);
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
        LocalEvent::MouseDelta { dx, dy } => dx.abs() + dy.abs() >= 3,
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

    backend.capture.set_swallow(swallow);
    let _ = backend.pointer.set_visible(!swallow);
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
        LocalEvent::MouseDelta { dx, dy } => {
            let transition = router.move_by(dx, dy);
            apply_transition(transition, router, clients, backend, input_owner);
            Ok(())
        }

        LocalEvent::Button { button, pressed } => {
            forward_if_remote(
                router,
                clients,
                InputEvent::MouseButton { button, pressed },
                input_owner,
            );
            Ok(())
        }

        LocalEvent::Wheel { dx, dy } => {
            forward_if_remote(
                router,
                clients,
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

            forward_if_remote(
                router,
                clients,
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

/// Send an event to the machine holding the cursor — unless that machine is
/// the one being physically touched, in which case its own OS already
/// delivered it and we are not suppressing there.
fn forward_if_remote(
    router: &CursorRouter,
    clients: &HashMap<MachineId, Client>,
    event: InputEvent,
    input_owner: MachineId,
) {
    let active = router.active();
    if active == input_owner {
        return;
    }
    if active == router.host() {
        return;
    }
    let Some(client) = clients.get(&active) else {
        return;
    };
    // Remap on the way out. The wire carries the host's view; each client gets
    // the translation appropriate to its own platform.
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
    match transition {
        Transition::Blocked => {}

        Transition::Stay(located) => {
            // The machine being touched moves its own cursor natively — we are
            // not suppressing there, so injecting would double the motion.
            // This is also what keeps a machine usable if the link drops.
            if located.machine == input_owner || located.machine == router.host() {
                return;
            }
            send_move(clients, located);
        }

        Transition::Switch { from, to } => {
            tracing::debug!(%from, to = %to.machine, "cursor crossed");

            // Whatever we are leaving must not be left holding keys down.
            if from != router.host() {
                if let Some(client) = clients.get(&from) {
                    let _ = client.tx.send(Frame::ReleaseAll);
                    let _ = client.tx.send(Frame::Leave);
                }
            } else {
                let _ = backend.inject.release_all();
            }

            if to.machine == router.host() {
                backend.capture.set_swallow(false);
                let _ = backend.pointer.warp(to.local);
                let _ = backend.pointer.set_visible(true);
            } else {
                // Suppress local delivery *before* announcing the switch, so no
                // stray event lands on the host in between. Only if the host is
                // the one being touched, though: if somebody else is driving,
                // this machine's own keyboard must stay live so its user can
                // take control back.
                let host_is_driving = input_owner == router.host();
                backend.capture.set_swallow(host_is_driving);
                let _ = backend.pointer.set_visible(!host_is_driving);
                if let Some(client) = clients.get(&to.machine) {
                    let _ = client.tx.send(Frame::Enter {
                        monitor: to.monitor,
                        x: to.local.x,
                        y: to.local.y,
                    });
                }
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
                if let Some(client) = clients.get(&from) {
                    let _ = client.tx.send(Frame::ReleaseAll);
                    let _ = client.tx.send(Frame::Leave);
                }
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
