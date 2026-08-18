//! Linux backend, built on evdev and uinput.
//!
//! ## Why not X11 or Wayland
//!
//! Because there is no "Linux desktop" to target, only several. X11 has XTEST
//! and XInput2 and works beautifully until the machine boots a Wayland session,
//! which most distributions now do by default. Wayland has no portable
//! equivalent at all: capturing input globally is deliberately not something a
//! client may do, and the ways in are compositor-specific.
//!
//! evdev and uinput sit below all of that. The kernel is the same on every
//! desktop, so one backend covers X11, Wayland, and a machine with no display
//! server at all.
//!
//! ## Permissions
//!
//! The trade for that is a permission. Reading `/dev/input/event*` and writing
//! `/dev/uinput` are privileged, and the usual grant is group membership:
//!
//! ```text
//! sudo usermod -aG input $USER      # then log out and back in
//! ```
//!
//! On distributions where `/dev/uinput` is root-only, it also needs a rule:
//!
//! ```text
//! echo 'KERNEL=="uinput", GROUP="input", MODE="0660"' \
//!     | sudo tee /etc/udev/rules.d/99-tether.rules
//! sudo udevadm control --reload-rules && sudo modprobe uinput
//! ```
//!
//! Unlike macOS, none of this is tied to the binary's signature, so it survives
//! a rebuild.
//!
//! ## What this backend cannot do
//!
//! * **Report where the cursor is.** That belongs to the display server and
//!   there is no way to ask it from here. Movement is always a delta, which is
//!   the path a suppressed machine already takes on every platform; the cost is
//!   that a sideways slide between two screens of different heights is not
//!   noticed, so the pointer can enter the next screen at the wrong height.
//! * **Know how the screens are arranged.** `/sys/class/drm` gives sizes, not
//!   positions. One screen is exact; several are laid out left to right and may
//!   need correcting once in the arrangement editor.
//! * **Lock the screen** on anything that is not systemd-logind or a desktop
//!   with a `loginctl`-compatible session.

pub mod capture;
pub mod inject;
pub mod keycodes;
pub mod screens;

use tether_proto::Point;

use crate::clipboard::SystemClipboard;
use crate::traits::{Monitors, PlatformError, Pointer, Result, ScreenLock};
use crate::{Backend, BackendKind};

pub fn backend() -> Result<Backend> {
    let monitors = screens::LinuxMonitors;
    // The injector needs the desktop extent up front: an absolute pointing
    // device declares its coordinate range when it is created.
    let desktop = monitors.enumerate().map(|screens| {
        let width = screens
            .iter()
            .map(|m| m.bounds.x + m.bounds.width)
            .max()
            .unwrap_or(1);
        let height = screens
            .iter()
            .map(|m| m.bounds.y + m.bounds.height)
            .max()
            .unwrap_or(1);
        (width, height)
    })?;

    Ok(Backend {
        kind: BackendKind::Native,
        capture: Box::new(capture::LinuxCapture::new()),
        inject: Box::new(inject::LinuxInject::new(desktop)?),
        pointer: Box::new(LinuxPointer),
        monitors: Box::new(screens::LinuxMonitors),
        clipboard: Box::new(SystemClipboard::new(0)?),
        lock: Box::new(LinuxScreenLock),
    })
}

/// Fails with an actionable message if this process cannot do input at all.
///
/// Checked before a session starts, because the alternative is a machine that
/// connects, reports itself healthy, and silently ignores every key.
pub fn check_input_permission() -> Result<()> {
    let readable = evdev::enumerate().next().is_some();
    if !readable {
        return Err(PlatformError::PermissionDenied(
            "cannot read any device in /dev/input. Add yourself to the group that \
             owns those nodes and log back in:\n    sudo usermod -aG input $USER"
                .into(),
        ));
    }

    match std::fs::OpenOptions::new().write(true).open("/dev/uinput") {
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Err(PlatformError::backend(
            "/dev/uinput does not exist, so this machine cannot be driven. \
                 Load the module:\n    sudo modprobe uinput",
        )),
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
            Err(PlatformError::PermissionDenied(
                "cannot write /dev/uinput, so this machine cannot be driven. \
                 Grant it with a udev rule:\n    echo 'KERNEL==\"uinput\", \
                 GROUP=\"input\", MODE=\"0660\"' | sudo tee \
                 /etc/udev/rules.d/99-tether.rules\n    sudo udevadm control \
                 --reload-rules"
                    .into(),
            ))
        }
        Err(err) => Err(PlatformError::backend(format!(
            "cannot open /dev/uinput: {err}"
        ))),
    }
}

/// There is no cursor to move from down here.
///
/// Every method is a no-op that reports success rather than an error, and that
/// is deliberate: the daemon calls these on every crossing, and a backend that
/// failed each time would fill the log with warnings about something it was
/// never going to be able to do. Grabbing the mouse — which the capture side
/// does when the pointer leaves — already freezes the local cursor, which is
/// the only one of these three jobs that actually matters.
pub struct LinuxPointer;

impl Pointer for LinuxPointer {
    fn position(&self) -> Result<Point> {
        Err(PlatformError::unsupported(
            "reading the cursor position on Linux (the display server owns it)",
        ))
    }

    fn warp(&self, _to: Point) -> Result<()> {
        // A client's `Enter` warps the pointer to the edge it was crossed at.
        // Here the injector does that instead, on the first absolute position
        // that follows, so there is nothing to do and nothing wrong.
        Ok(())
    }

    fn set_visible(&self, _visible: bool) -> Result<()> {
        Ok(())
    }

    fn set_pinned(&self, _pinned: bool) -> Result<()> {
        // Grabbing the devices is the pin, and capture does it. Saying "yes,
        // done" here is accurate rather than a shrug.
        Ok(())
    }
}

pub struct LinuxScreenLock;

impl ScreenLock for LinuxScreenLock {
    fn lock(&self) -> Result<()> {
        // `loginctl lock-session` is the one that goes through logind and lets
        // the desktop's own locker respond, which is what the user has already
        // configured. The alternatives are all specific to one screensaver.
        let status = std::process::Command::new("loginctl")
            .arg("lock-session")
            .status();
        match status {
            Ok(status) if status.success() => return Ok(()),
            Ok(status) => tracing::warn!(?status, "loginctl lock-session failed"),
            Err(err) => tracing::warn!(%err, "could not run loginctl"),
        }

        Err(PlatformError::unsupported(
            "locking the screen on this system. `loginctl lock-session` is the \
             only portable way in, and it did not work here — turn \
             lock_screen_on_leave off in the config.",
        ))
    }
}
