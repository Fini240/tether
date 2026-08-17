//! Windows backend — **not implemented**.
//!
//! Every entry point returns `Unsupported` naming the Win32 API it needs, so a
//! run on Windows fails loudly at startup instead of appearing to work.
//!
//! What each piece requires:
//!
//! * **capture** — `SetWindowsHookEx` with `WH_MOUSE_LL` and `WH_KEYBOARD_LL`,
//!   on a thread running a `GetMessage` loop (the hooks are delivered as
//!   messages, so the thread cannot block). Return a non-zero value from the
//!   hook proc to swallow an event. Raw Input (`WM_INPUT`) is the alternative
//!   and gives better mouse deltas, but cannot suppress, so the low-level hook
//!   is the one that matches the trait.
//! * **inject** — `SendInput` with `INPUT_MOUSE` / `INPUT_KEYBOARD`. Use
//!   `MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK` and the 0..65535
//!   normalised coordinate space, which is *not* the pixel space
//!   `EnumDisplayMonitors` reports — converting between them is where this
//!   usually goes wrong on multi-monitor setups.
//! * **monitors** — `EnumDisplayMonitors` + `GetMonitorInfoW`, plus
//!   `SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2)` at startup or every
//!   coordinate arrives pre-scaled and wrong.
//! * **lock** — `LockWorkStation`.
//! * **keycodes** — HID usage ⇄ scancode, then `MapVirtualKeyW`. Send scancodes
//!   rather than virtual keys so the client's own keyboard layout applies.
//!
//! Two things that will bite regardless of implementation:
//!
//! * UIPI: a normal-integrity process cannot inject into an elevated window.
//!   The daemon has to run as a service (or elevated) to drive Task Manager,
//!   UAC prompts, and similar.
//! * The secure desktop (UAC, Ctrl+Alt+Del, the lock screen) accepts no
//!   injected input at all, at any privilege level. That is by design in
//!   Windows and cannot be worked around.

use crate::traits::{PlatformError, Result};
use crate::Backend;

pub fn backend() -> Result<Backend> {
    Err(PlatformError::unsupported(
        "the Windows backend (needs SetWindowsHookEx + SendInput); \
         run with --backend headless to exercise the rest of the stack",
    ))
}
