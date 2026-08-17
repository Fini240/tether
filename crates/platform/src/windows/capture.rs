//! Capturing the physical keyboard and mouse with low-level hooks.
//!
//! `WH_KEYBOARD_LL` and `WH_MOUSE_LL` rather than Raw Input, because only a
//! low-level hook can *suppress* an event — returning non-zero from the hook
//! swallows it, which is what stops keystrokes landing here while the pointer
//! is on another machine. Raw Input gives cleaner mouse deltas but cannot
//! suppress, so it does not fit the trait.
//!
//! Hooks are delivered as thread messages, so the thread that installs them
//! must run a `GetMessage` loop and must not block. It gets a thread of its own.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;

use tether_proto::{Modifiers, MouseButton};
use tokio::sync::mpsc::UnboundedSender;
use windows_sys::Win32::Foundation::{LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use super::inject::TETHER_EVENT_MARK;
use super::keycodes::scancode_to_hid;
use crate::traits::{InputCapture, LocalEvent, PlatformError, Result};

struct CaptureShared {
    sink: Mutex<Option<UnboundedSender<LocalEvent>>>,
    swallow: AtomicBool,
    injected: AtomicU64,
    thread_id: AtomicU32,
    /// Set while the hook is re-centring the cursor itself, so the move that
    /// causes is not read back as the user moving the mouse.
    pinning: AtomicBool,
    /// Where the pointer was last seen, for turning absolute hook positions
    /// into deltas.
    last: Mutex<POINT>,
    /// Where the pointer is pinned while another machine has it.
    anchor: Mutex<POINT>,
    modifiers: Mutex<Modifiers>,
}

impl CaptureShared {
    fn emit(&self, event: LocalEvent) {
        let Ok(guard) = self.sink.lock() else { return };
        if let Some(sink) = guard.as_ref() {
            let _ = sink.send(event);
        }
    }
}

/// The hook procedures are plain `extern "system"` functions with no user
/// pointer, so the shared state has to be reachable globally.
static SHARED: OnceLock<Arc<CaptureShared>> = OnceLock::new();

pub struct WindowsCapture {
    shared: Arc<CaptureShared>,
    thread: Option<JoinHandle<()>>,
}

impl WindowsCapture {
    pub fn new() -> Self {
        let shared = SHARED
            .get_or_init(|| {
                Arc::new(CaptureShared {
                    sink: Mutex::new(None),
                    swallow: AtomicBool::new(false),
                    injected: AtomicU64::new(0),
                    thread_id: AtomicU32::new(0),
                    pinning: AtomicBool::new(false),
                    last: Mutex::new(POINT { x: 0, y: 0 }),
                    anchor: Mutex::new(POINT { x: 0, y: 0 }),
                    modifiers: Mutex::new(Modifiers::NONE),
                })
            })
            .clone();

        Self {
            shared,
            thread: None,
        }
    }
}

impl Default for WindowsCapture {
    fn default() -> Self {
        Self::new()
    }
}

impl InputCapture for WindowsCapture {
    fn start(&mut self, sink: UnboundedSender<LocalEvent>) -> Result<()> {
        if self.thread.is_some() {
            return Ok(());
        }
        *self
            .shared
            .sink
            .lock()
            .map_err(|_| PlatformError::backend("capture sink poisoned"))? = Some(sink);

        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<std::result::Result<(), String>>();

        let handle = std::thread::Builder::new()
            .name("tether-hooks".into())
            .spawn(move || run_hooks(ready_tx))
            .map_err(|e| PlatformError::backend(format!("could not spawn hook thread: {e}")))?;

        match ready_rx.recv() {
            Ok(Ok(())) => {
                self.thread = Some(handle);
                Ok(())
            }
            Ok(Err(message)) => Err(PlatformError::backend(message)),
            Err(_) => Err(PlatformError::backend("hook thread exited during startup")),
        }
    }

    fn stop(&mut self) {
        let thread_id = self.shared.thread_id.swap(0, Ordering::SeqCst);
        if thread_id != 0 {
            // The hook thread is parked in GetMessage; a posted WM_QUIT is the
            // only way to get it out.
            unsafe { PostThreadMessageW(thread_id, WM_QUIT, 0, 0) };
        }
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
        if let Ok(mut sink) = self.shared.sink.lock() {
            *sink = None;
        }
    }

    fn set_swallow(&self, swallow: bool) {
        let was = self.shared.swallow.swap(swallow, Ordering::SeqCst);
        if swallow && !was {
            // Pin the pointer where it stands. While suppressed the cursor does
            // not move, so every hook event would otherwise report the same
            // position and produce a delta of zero after the first one.
            if let Ok(last) = self.shared.last.lock() {
                if let Ok(mut anchor) = self.shared.anchor.lock() {
                    *anchor = *last;
                }
            }
        }
    }

    fn injected_filtered(&self) -> u64 {
        self.shared.injected.load(Ordering::Relaxed)
    }
}

impl Drop for WindowsCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_hooks(ready: std::sync::mpsc::Sender<std::result::Result<(), String>>) {
    unsafe {
        let module = GetModuleHandleW(std::ptr::null());

        let keyboard = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), module, 0);
        let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), module, 0);

        if keyboard.is_null() || mouse.is_null() {
            if !keyboard.is_null() {
                UnhookWindowsHookEx(keyboard);
            }
            if !mouse.is_null() {
                UnhookWindowsHookEx(mouse);
            }
            let _ = ready.send(Err(
                "Windows refused the input hooks. This usually means another \
                 program holds them, or Tether is running at a lower integrity \
                 level than the foreground application."
                    .to_string(),
            ));
            return;
        }

        if let Some(shared) = SHARED.get() {
            shared
                .thread_id
                .store(GetCurrentThreadId(), Ordering::SeqCst);
        }
        let _ = ready.send(Ok(()));

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        UnhookWindowsHookEx(keyboard);
        UnhookWindowsHookEx(mouse);
    }
}

/// Suppress by returning 1; pass through by deferring to the next hook.
unsafe fn finish(shared: &CaptureShared, code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if shared.swallow.load(Ordering::SeqCst) {
        1
    } else {
        CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
    }
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(shared) = SHARED.get() else {
        return CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam);
    };
    if code < 0 {
        return CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam);
    }

    let info = &*(lparam as *const KBDLLHOOKSTRUCT);

    // Ours. Let it through untouched and do not report it as user input, or a
    // machine being driven remotely would read the incoming keystrokes as
    // somebody at its own keyboard and grab control back.
    if info.dwExtraInfo == TETHER_EVENT_MARK {
        shared.injected.fetch_add(1, Ordering::Relaxed);
        return CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam);
    }

    let pressed = matches!(wparam as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
    let extended = info.flags & LLKHF_EXTENDED != 0;

    let Some(key) = scancode_to_hid(info.scanCode as u16, extended) else {
        return finish(shared, code, wparam, lparam);
    };

    // Windows does not attach modifier state to each event the way macOS does,
    // so it is tracked here from the modifier keys themselves.
    let modifiers = {
        let mut guard = match shared.modifiers.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(bit) = key.modifier_bit() {
            guard.set(bit, pressed);
        }
        if key == tether_proto::KeyCode::CAPS_LOCK && pressed {
            let on = guard.contains(Modifiers::CAPS_LOCK);
            guard.set(Modifiers::CAPS_LOCK, !on);
        }
        *guard
    };

    shared.emit(LocalEvent::Key {
        key,
        pressed,
        modifiers,
        // The hook gives no repeat flag; the host treats a repeat the same as
        // a press, so reporting false is accurate enough to be honest.
        repeat: false,
    });

    finish(shared, code, wparam, lparam)
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(shared) = SHARED.get() else {
        return CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam);
    };
    if code < 0 {
        return CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam);
    }

    let info = &*(lparam as *const MSLLHOOKSTRUCT);

    // Our own re-centring, coming back round. Record where it put the pointer
    // and say nothing.
    if shared.pinning.load(Ordering::SeqCst) {
        if wparam as u32 == WM_MOUSEMOVE {
            if let Ok(mut last) = shared.last.lock() {
                *last = info.pt;
            }
        }
        return CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam);
    }

    if info.dwExtraInfo == TETHER_EVENT_MARK {
        shared.injected.fetch_add(1, Ordering::Relaxed);

        // Still record where the pointer ended up. Deltas here are computed as
        // `position - last`, so skipping this leaves `last` stale for as long
        // as this machine is being driven from elsewhere — and the first real
        // movement afterwards produces one enormous delta measured from a
        // position the cursor left minutes ago, flinging the pointer across
        // the canvas instead of walking it to the next screen.
        if wparam as u32 == WM_MOUSEMOVE {
            if let Ok(mut last) = shared.last.lock() {
                *last = info.pt;
            }
        }
        return CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam);
    }

    match wparam as u32 {
        WM_MOUSEMOVE => {
            let swallowing = shared.swallow.load(Ordering::SeqCst);

            let delta = {
                let mut last = match shared.last.lock() {
                    Ok(guard) => guard,
                    Err(poisoned) => poisoned.into_inner(),
                };
                let reference = if swallowing {
                    match shared.anchor.lock() {
                        Ok(anchor) => *anchor,
                        Err(poisoned) => *poisoned.into_inner(),
                    }
                } else {
                    *last
                };
                let delta = (info.pt.x - reference.x, info.pt.y - reference.y);
                *last = info.pt;
                delta
            };

            if swallowing {
                // Suppressing stops the cursor moving, so without putting it
                // back on the anchor every event would measure from a stale
                // point and motion would drift to a halt.
                //
                // SetCursorPos re-enters this hook, and it carries no
                // dwExtraInfo — the mark check above cannot see it, whatever
                // an earlier comment here claimed. The flag is what keeps that
                // re-entry from being reported as a hand on the mouse.
                let anchor = match shared.anchor.lock() {
                    Ok(anchor) => *anchor,
                    Err(poisoned) => *poisoned.into_inner(),
                };
                if let Ok(mut last) = shared.last.lock() {
                    *last = anchor;
                }
                shared.pinning.store(true, Ordering::SeqCst);
                SetCursorPos(anchor.x, anchor.y);
                shared.pinning.store(false, Ordering::SeqCst);
            }

            if delta != (0, 0) {
                if swallowing {
                    shared.emit(LocalEvent::MouseDelta {
                        dx: delta.0,
                        dy: delta.1,
                    });
                } else {
                    // `pt` is where the OS is about to put the pointer, seam
                    // adjustments and all. Far better than differencing, and
                    // it cannot race anything.
                    shared.emit(LocalEvent::MouseMoved {
                        x: info.pt.x,
                        y: info.pt.y,
                        dx: delta.0,
                        dy: delta.1,
                    });
                }
            }
        }

        WM_LBUTTONDOWN => shared.emit(button(MouseButton::Left, true)),
        WM_LBUTTONUP => shared.emit(button(MouseButton::Left, false)),
        WM_RBUTTONDOWN => shared.emit(button(MouseButton::Right, true)),
        WM_RBUTTONUP => shared.emit(button(MouseButton::Right, false)),
        WM_MBUTTONDOWN => shared.emit(button(MouseButton::Middle, true)),
        WM_MBUTTONUP => shared.emit(button(MouseButton::Middle, false)),

        WM_XBUTTONDOWN | WM_XBUTTONUP => {
            // Which side button is in the high word of mouseData.
            let which = (info.mouseData >> 16) as u16;
            let btn = if which == 1 {
                MouseButton::Back
            } else {
                MouseButton::Forward
            };
            shared.emit(button(btn, wparam as u32 == WM_XBUTTONDOWN));
        }

        WM_MOUSEWHEEL => {
            let notches = (info.mouseData >> 16) as i16 as f32 / 120.0;
            shared.emit(LocalEvent::Wheel {
                dx: 0.0,
                dy: notches,
            });
        }
        WM_MOUSEHWHEEL => {
            let notches = (info.mouseData >> 16) as i16 as f32 / 120.0;
            shared.emit(LocalEvent::Wheel {
                dx: notches,
                dy: 0.0,
            });
        }

        _ => {}
    }

    finish(shared, code, wparam, lparam)
}

fn button(button: MouseButton, pressed: bool) -> LocalEvent {
    LocalEvent::Button { button, pressed }
}
