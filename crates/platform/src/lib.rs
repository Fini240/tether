//! The OS boundary.
//!
//! Everything that talks to a window server, an event tap, or a pasteboard
//! lives behind the traits in [`traits`]. The rest of the codebase is written
//! against those traits and does not know which platform it is on.
//!
//! ## Implementation status
//!
//! | | capture | inject | monitors | clipboard | lock |
//! |---|---|---|---|---|---|
//! | macOS   | ✅ CGEventTap | ✅ CGEvent | ✅ | ✅ | ✅ |
//! | Windows | ✅ WH_*_LL hooks | ✅ SendInput | ✅ | ✅ | ✅ |
//! | headless | ✅ synthetic | ✅ logs | ✅ fake | ✅ in-memory | ✅ no-op |
//!
//! Linux is out of scope. Wayland has no legacy input path at all, and
//! supporting X11 alone would have meant shipping something that broke on the
//! default session of every current distribution.
//!
//! Stubs return [`PlatformError::Unsupported`] with the name of the API that
//! should be called, rather than silently doing nothing. A KVM that pretends to
//! have delivered a keystroke is worse than one that refuses to start.
//!
//! The `headless` backend is not a mock for tests only — it is how you run a
//! host and a client on one machine to watch the routing work end to end.

pub mod clipboard;
pub mod headless;
pub mod traits;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "windows")]
pub mod windows;

pub use traits::{
    ClipboardAccess, InputCapture, InputInject, LocalEvent, Monitors, PlatformError, Pointer,
    Result, ScreenLock,
};

/// Which backend to construct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    /// The real OS for this build.
    Native,
    /// Synthetic screens, logged injection, in-memory clipboard.
    Headless,
}

/// Everything the daemon needs from the OS, in one bundle.
pub struct Backend {
    pub kind: BackendKind,
    pub capture: Box<dyn InputCapture>,
    pub inject: Box<dyn InputInject>,
    pub pointer: Box<dyn Pointer>,
    pub monitors: Box<dyn Monitors>,
    pub clipboard: Box<dyn ClipboardAccess>,
    pub lock: Box<dyn ScreenLock>,
}

impl Backend {
    pub fn new(kind: BackendKind) -> Result<Backend> {
        match kind {
            BackendKind::Headless => Ok(headless::backend()),
            BackendKind::Native => native(),
        }
    }
}

#[cfg(target_os = "macos")]
fn native() -> Result<Backend> {
    macos::backend()
}

#[cfg(target_os = "windows")]
fn native() -> Result<Backend> {
    windows::backend()
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn native() -> Result<Backend> {
    Err(traits::PlatformError::unsupported(
        "this operating system. Tether targets macOS and Windows; \
         run with --backend headless to exercise the rest of the stack",
    ))
}

/// Whether this process holds the OS permissions needed to capture input.
///
/// Checked before a host starts, because the failure mode otherwise is an event
/// tap that installs successfully and then never fires — which looks like a
/// network problem and is not.
pub fn check_capture_permission() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        macos::check_accessibility_permission()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(())
    }
}
