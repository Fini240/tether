//! macOS backend.
//!
//! ## Permissions
//!
//! Capture needs **Accessibility**; some macOS versions additionally prompt for
//! **Input Monitoring**. Both are granted per *binary*, keyed on the code
//! signature. Two consequences worth knowing before debugging a silent failure:
//!
//! * A `cargo build` that changes the binary invalidates an ad-hoc signature,
//!   and the grant has to be removed and re-added. Sign with a stable identity
//!   to avoid re-approving on every rebuild.
//! * Injection (`CGEventPost`) needs Accessibility too, so a *client* also has
//!   to be approved — it just fails silently rather than erroring.

pub mod capture;
pub mod ffi;
pub mod inject;
pub mod keycodes;
pub mod screens;

use tether_proto::Point;

use crate::clipboard::SystemClipboard;
use crate::traits::{PlatformError, Pointer, Result, ScreenLock};
use crate::{Backend, BackendKind};

pub fn backend() -> Result<Backend> {
    Ok(Backend {
        kind: BackendKind::Native,
        capture: Box::new(capture::MacCapture::new()),
        inject: Box::new(inject::MacInject::new()),
        pointer: Box::new(MacPointer),
        monitors: Box::new(screens::MacMonitors),
        clipboard: Box::new(SystemClipboard::new(0)?),
        lock: Box::new(MacScreenLock),
    })
}

/// Fails with an actionable message if the process is not trusted for
/// Accessibility.
pub fn check_accessibility_permission() -> Result<()> {
    if unsafe { ffi::AXIsProcessTrusted() } {
        return Ok(());
    }
    Err(PlatformError::PermissionDenied(
        "this binary is not trusted for Accessibility. Open System Settings → \
         Privacy & Security → Accessibility, add it, and restart it. The grant \
         is tied to the binary's signature, so it must be re-added after an \
         unsigned rebuild."
            .into(),
    ))
}

pub struct MacPointer;

impl Pointer for MacPointer {
    fn position(&self) -> Result<Point> {
        // Read the cursor from a null event, which CoreGraphics fills in with
        // the current location. Cheaper than creating an event source.
        unsafe {
            let event = ffi::CGEventCreateMouseEvent(
                std::ptr::null_mut(),
                ffi::kCGEventNull,
                ffi::CGPoint { x: 0.0, y: 0.0 },
                ffi::kCGMouseButtonLeft,
            );
            if event.is_null() {
                return Err(PlatformError::backend("could not read the cursor position"));
            }
            let location = ffi::CGEventGetLocation(event);
            ffi::CFRelease(event);
            Ok(Point::new(location.x as i32, location.y as i32))
        }
    }

    fn warp(&self, to: Point) -> Result<()> {
        inject::warp(to)
    }

    fn set_visible(&self, visible: bool) -> Result<()> {
        inject::set_cursor_visible(visible)
    }
}

pub struct MacScreenLock;

impl ScreenLock for MacScreenLock {
    fn lock(&self) -> Result<()> {
        // CGSession -suspend drops straight to the login window, which is a
        // real lock. `pmset displaysleepnow` only sleeps the display and locks
        // solely if the user has "require password after sleep" set, so it is
        // the fallback rather than the first choice.
        const CGSESSION: &str =
            "/System/Library/CoreServices/Menu Extras/User.menu/Contents/Resources/CGSession";

        if std::path::Path::new(CGSESSION).exists() {
            let status = std::process::Command::new(CGSESSION)
                .arg("-suspend")
                .status();
            match status {
                Ok(s) if s.success() => return Ok(()),
                Ok(s) => tracing::warn!(?s, "CGSession -suspend failed; falling back to pmset"),
                Err(e) => tracing::warn!(%e, "could not run CGSession; falling back to pmset"),
            }
        }

        std::process::Command::new("/usr/bin/pmset")
            .arg("displaysleepnow")
            .status()
            .map_err(|e| PlatformError::backend(format!("could not lock the screen: {e}")))?;
        Ok(())
    }
}
