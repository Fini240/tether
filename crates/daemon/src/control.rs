//! The seam between a running daemon and a user interface.
//!
//! The daemon publishes a [`Status`] snapshot and accepts [`Command`]s. That is
//! the whole contract — the GUI never reaches into the session loop, and the
//! session loop knows nothing about a GUI existing.
//!
//! A mutex around a snapshot rather than a stream of events: a UI redrawing at
//! 60 Hz wants "what is true now", not a backlog to fold, and the snapshot is
//! small enough that copying it per frame is free next to the rendering.

use std::sync::{Arc, Mutex};

use tether_core::config::Role;
use tether_core::layout::{Layout, MachineId};
use tether_proto::Platform;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

/// A peer as the running session sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct PeerInfo {
    pub machine: MachineId,
    pub name: String,
    pub platform: Platform,
    pub address: String,
    pub connected: bool,
}

/// What the daemon is doing right now.
#[derive(Debug, Clone, Default)]
pub struct Status {
    pub running: bool,
    pub role: Option<Role>,
    /// Address the host is listening on, once bound.
    pub listening: Option<String>,
    pub peers: Vec<PeerInfo>,
    /// Machine whose keyboard and mouse are driving.
    pub input_owner: Option<MachineId>,
    /// Machine the pointer is currently on.
    pub cursor_on: Option<MachineId>,
    /// Where the pointer is on the shared canvas. Drawn in the window, because
    /// "the pointer will not cross" and "the pointer is not where you think"
    /// look identical until you can see it.
    pub cursor_position: Option<tether_proto::Point>,
    pub cursor_locked: bool,
    /// Set when the session stopped because something went wrong.
    pub error: Option<String>,
    /// The live arrangement, which is not always the saved one — a client that
    /// has just joined is on the canvas before anyone has saved.
    pub layout: Layout,
    pub this_machine: Option<MachineId>,
    /// Human-readable line for the status bar.
    pub detail: String,
}

impl Status {
    pub fn name_of(&self, machine: MachineId) -> String {
        self.layout
            .get(machine)
            .map(|p| p.name.clone())
            .or_else(|| {
                self.peers
                    .iter()
                    .find(|p| p.machine == machine)
                    .map(|p| p.name.clone())
            })
            .unwrap_or_else(|| machine.to_string())
    }
}

/// Asked of a running daemon.
#[derive(Debug, Clone)]
pub enum Command {
    /// Replace the arrangement, e.g. after dragging a screen in the UI.
    SetLayout(Layout),
    /// Move the pointer to a machine.
    JumpTo(MachineId),
    ToggleCursorLock,
    /// Stop the session and return from `run`.
    Stop,
}

/// Publishes the snapshot. Separate from the command receiver so a session
/// loop can hold both across a `select!` — updating status while awaiting a
/// command needs two independent borrows.
#[derive(Clone)]
pub struct StatusHandle(Arc<Mutex<Status>>);

impl StatusHandle {
    /// Update the published snapshot.
    ///
    /// A poisoned lock is recovered rather than propagated: the status is a
    /// plain snapshot with no invariants to violate, and killing a working
    /// input session because a UI thread panicked mid-read would be absurd.
    pub fn update(&self, edit: impl FnOnce(&mut Status)) {
        let mut guard = self.0.lock().unwrap_or_else(|e| e.into_inner());
        edit(&mut guard);
    }
}

/// The daemon's half: publish status, receive commands.
pub struct DaemonControl {
    pub status: StatusHandle,
    pub commands: UnboundedReceiver<Command>,
}

/// The UI's half: read status, send commands.
#[derive(Clone)]
pub struct UiControl {
    status: Arc<Mutex<Status>>,
    commands: UnboundedSender<Command>,
}

impl UiControl {
    pub fn status(&self) -> Status {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Returns false if the daemon has gone away.
    pub fn send(&self, command: Command) -> bool {
        self.commands.send(command).is_ok()
    }
}

/// Create a linked pair.
pub fn channel() -> (DaemonControl, UiControl) {
    let status = Arc::new(Mutex::new(Status::default()));
    let (tx, rx) = mpsc::unbounded_channel();

    (
        DaemonControl {
            status: StatusHandle(Arc::clone(&status)),
            commands: rx,
        },
        UiControl {
            status,
            commands: tx,
        },
    )
}
