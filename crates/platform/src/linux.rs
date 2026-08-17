//! Linux backend — **not implemented**.
//!
//! X11 first, Wayland later, per the project's scope decision. Every entry
//! point returns `Unsupported` naming the API it needs.
//!
//! ## X11
//!
//! * **capture** — the XRecord extension (`XRecordCreateContext` on a second
//!   display connection) to observe events without grabbing. XRecord *cannot*
//!   suppress, so swallowing needs an `XGrabKeyboard` / `XGrabPointer` while
//!   the cursor is remote — the standard approach, and the reason the mouse
//!   pointer visibly freezes on the host during a switch.
//! * **inject** — `XTestFakeKeyEvent` / `XTestFakeButtonEvent` /
//!   `XTestFakeMotionEvent` from the XTEST extension.
//! * **monitors** — XRandR (`XRRGetScreenResources` + `XRRGetCrtcInfo`).
//!   Xinerama is the fallback for ancient servers.
//! * **keycodes** — HID usage → X keycode is `evdev_code + 8` on any modern
//!   server, but confirm against `XkbGetKeyboard` rather than assuming.
//! * **lock** — no standard call. Try `loginctl lock-session`, then
//!   `xdg-screensaver lock`, then the desktop-specific binaries.
//!
//! ## Wayland (later milestone)
//!
//! There is no legacy injection path at all — this is not an oversight in the
//! protocol, it is the security model. The route is:
//!
//! * `xdg-desktop-portal`'s `RemoteDesktop` interface to get a session, then
//!   **libei** (`ei_*`) for both input capture and injection.
//! * `libeis` on the compositor side; GNOME 45+ and KDE Plasma 6+ ship it.
//!   Older compositors and most wlroots-based ones cannot be supported.
//! * The portal shows a permission dialog per session and the grant does not
//!   persist across reboots, so the daemon needs a re-authorise flow rather
//!   than a one-time setup step.
//!
//! Until then, a Wayland session is detected at startup and refused with an
//! explanation rather than silently doing nothing. Note that an *XWayland*
//! client cannot be driven either — XTEST inside XWayland only reaches
//! XWayland windows, so it looks like it half-works, which is worse.

use crate::traits::{PlatformError, Result};
use crate::Backend;

/// Whether this session is Wayland. Worth checking before blaming the network.
pub fn is_wayland_session() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var("XDG_SESSION_TYPE")
            .map(|v| v.eq_ignore_ascii_case("wayland"))
            .unwrap_or(false)
}

pub fn backend() -> Result<Backend> {
    if is_wayland_session() {
        return Err(PlatformError::unsupported(
            "Wayland sessions (needs the RemoteDesktop portal + libei). \
             Log into an X11 session, or run with --backend headless",
        ));
    }
    Err(PlatformError::unsupported(
        "the X11 backend (needs XRecord + XTEST); \
         run with --backend headless to exercise the rest of the stack",
    ))
}
