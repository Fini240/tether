//! Windows backend.
//!
//! ## Two limits worth knowing before debugging
//!
//! * **UIPI.** A normal-integrity process cannot inject input into a window
//!   owned by an elevated one. If Tether drives everything except Task Manager
//!   or your elevated terminal, that is why — run it elevated too.
//! * **The secure desktop.** UAC prompts, Ctrl+Alt+Del and the lock screen
//!   accept no injected input at any privilege level. That is by design in
//!   Windows and cannot be worked around.
//!
//! ## No permission prompt
//!
//! Unlike macOS there is no Accessibility grant to obtain: low-level hooks work
//! for any process at the same integrity level. `check_capture_permission`
//! therefore succeeds without asking anything.

pub mod capture;
pub mod inject;
pub mod keycodes;
pub mod screens;

use tether_proto::Point;
use windows_sys::Win32::System::Shutdown::LockWorkStation;
use windows_sys::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

use crate::clipboard::SystemClipboard;
use crate::traits::{PlatformError, Pointer, Result, ScreenLock};
use crate::{Backend, BackendKind};

pub fn backend() -> Result<Backend> {
    // Must happen before anything asks about a monitor or a cursor position.
    // Without it Windows lies to a non-aware process about both, reporting
    // virtualised coordinates scaled to 96 DPI — so on a 150% display every
    // position is out by a third and the cursor lands in the wrong place.
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    Ok(Backend {
        kind: BackendKind::Native,
        capture: Box::new(capture::WindowsCapture::new()),
        inject: Box::new(inject::WindowsInject::new()),
        pointer: Box::new(WindowsPointer),
        monitors: Box::new(screens::WindowsMonitors),
        clipboard: Box::new(SystemClipboard::new(0)?),
        lock: Box::new(WindowsScreenLock),
    })
}

/// Nothing to ask for on Windows.
pub fn check_capture_permission() -> Result<()> {
    Ok(())
}

pub struct WindowsPointer;

impl Pointer for WindowsPointer {
    fn position(&self) -> Result<Point> {
        inject::cursor_position()
    }

    fn warp(&self, to: Point) -> Result<()> {
        inject::warp(to)
    }

    fn set_visible(&self, _visible: bool) -> Result<()> {
        // See inject::set_cursor_visible — there is no honest way to hide the
        // system cursor from a background process, so this does nothing rather
        // than pretending.
        Ok(())
    }
}

pub struct WindowsScreenLock;

impl ScreenLock for WindowsScreenLock {
    fn lock(&self) -> Result<()> {
        let ok = unsafe { LockWorkStation() };
        if ok == 0 {
            return Err(PlatformError::backend(
                "LockWorkStation failed (it is refused for a process running \
                 in a non-interactive session)",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::traits::LocalEvent;
    use std::time::Duration;

    /// Warping must not come back as the user moving the mouse.
    ///
    /// The distinction is invisible in the type system — both `SetCursorPos`
    /// and `SendInput` move the pointer, and both wake the hook — so it is
    /// worth pinning down. Skipped where the hooks cannot install, which is
    /// any session without an interactive desktop.
    #[test]
    fn a_warp_is_not_reported_as_input() {
        let mut backend = match super::backend() {
            Ok(backend) => backend,
            Err(_) => return,
        };

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        if backend.capture.start(tx).is_err() {
            return; // no interactive desktop; nothing to assert about
        }

        let Ok(origin) = backend.pointer.position() else {
            backend.capture.stop();
            return;
        };

        for step in 1..=4 {
            let _ = backend.pointer.warp(tether_proto::Point::new(
                origin.x + step * 17,
                origin.y + step * 11,
            ));
            std::thread::sleep(Duration::from_millis(40));
        }
        std::thread::sleep(Duration::from_millis(150));

        let reported: Vec<LocalEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        let _ = backend.pointer.warp(origin);
        backend.capture.stop();

        let moves = reported
            .iter()
            .filter(|e| matches!(e, LocalEvent::MouseDelta { .. }))
            .count();
        assert_eq!(
            moves, 0,
            "warping the cursor was reported back as {moves} mouse movements; \
             every crossing would fling the pointer by however far it jumped"
        );
    }
}
