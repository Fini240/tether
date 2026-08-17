//! The OS-facing traits every backend implements.

use tether_proto::{
    ClipboardContents, InputEvent, KeyCode, Modifiers, MonitorInfo, MouseButton, Point,
};
use tokio::sync::mpsc::UnboundedSender;

pub type Result<T> = std::result::Result<T, PlatformError>;

#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    /// Named so the message can say *which* API is missing, e.g.
    /// `Unsupported("input capture (needs SetWindowsHookEx)")`.
    #[error("not implemented on this platform: {0}")]
    Unsupported(String),

    /// The OS refused for permission reasons and the user must intervene. The
    /// string is shown verbatim, so it should say what to click.
    #[error("permission denied: {0}")]
    PermissionDenied(String),

    #[error("{0}")]
    Backend(String),
}

impl PlatformError {
    pub fn unsupported(what: impl Into<String>) -> Self {
        PlatformError::Unsupported(what.into())
    }
    pub fn backend(what: impl Into<String>) -> Self {
        PlatformError::Backend(what.into())
    }
}

/// An input event as captured on the host, before any routing.
///
/// Motion is a *delta*, not a position: once the cursor is on a remote machine
/// the host's own pointer is pinned, so its absolute position stops tracking
/// what the user is doing.
#[derive(Debug, Clone, PartialEq)]
pub enum LocalEvent {
    MouseDelta {
        dx: i32,
        dy: i32,
    },
    Button {
        button: MouseButton,
        pressed: bool,
    },
    Wheel {
        dx: f32,
        dy: f32,
    },
    Key {
        key: KeyCode,
        pressed: bool,
        modifiers: Modifiers,
        repeat: bool,
    },
}

/// Captures the physical keyboard and mouse. Host side only.
pub trait InputCapture: Send {
    /// Begin capturing. Events are pushed to `sink` from a backend-owned
    /// thread; the channel is unbounded because dropping input events silently
    /// produces stuck modifiers.
    fn start(&mut self, sink: UnboundedSender<LocalEvent>) -> Result<()>;

    fn stop(&mut self);

    /// While true, captured events are consumed instead of reaching local
    /// applications. Set when the cursor is on a remote machine — otherwise
    /// every keystroke lands on both machines at once.
    fn set_swallow(&self, swallow: bool);
}

/// Injects input into this machine. Client side (and host, for local replay).
pub trait InputInject: Send + Sync {
    fn inject(&self, event: &InputEvent) -> Result<()>;

    /// Release every key and button this backend believes is held.
    ///
    /// Called when the cursor leaves and whenever a connection drops. Without
    /// it, a Shift held while switching machines stays down forever on the one
    /// you left, and the only cure is pressing and releasing it manually.
    fn release_all(&self) -> Result<()>;
}

pub trait Pointer: Send + Sync {
    fn position(&self) -> Result<Point>;

    /// Move the cursor without generating a motion event.
    fn warp(&self, to: Point) -> Result<()>;

    /// Hide the cursor while another machine owns it, show it on return.
    fn set_visible(&self, visible: bool) -> Result<()>;
}

pub trait Monitors: Send + Sync {
    /// Every attached display, in this machine's local coordinate space.
    fn enumerate(&self) -> Result<Vec<MonitorInfo>>;
}

pub trait ClipboardAccess: Send {
    fn read(&mut self) -> Result<ClipboardContents>;
    fn write(&mut self, contents: &ClipboardContents) -> Result<()>;

    /// Read the clipboard and return it only if it changed since the last call.
    ///
    /// Polled rather than event-driven: no platform offers a portable change
    /// notification, and the two that offer something (NSPasteboard's
    /// `changeCount`, Windows' clipboard-format listener) disagree on
    /// semantics. Comparing contents is boring and correct.
    fn poll_change(&mut self) -> Result<Option<ClipboardContents>>;
}

pub trait ScreenLock: Send + Sync {
    fn lock(&self) -> Result<()>;
}
