//! Capturing the physical keyboard and mouse with low-level hooks.
//!
//! `WH_KEYBOARD_LL` and `WH_MOUSE_LL` rather than Raw Input alone, because only
//! a low-level hook can *suppress* an event — returning non-zero from the hook
//! swallows it, which is what stops keystrokes landing here while the pointer
//! is on another machine. Raw Input cannot suppress, so it cannot replace the
//! hooks.
//!
//! It is still needed alongside them, for one reason: **a hook reports where
//! the cursor ended up, and Windows clamps that to the virtual desktop.** Push
//! the mouse against the outer edge of the desktop and `MSLLHOOKSTRUCT.pt`
//! stops changing while the user is plainly still pushing — so the movement
//! that should carry the pointer onto the next machine differences out to
//! nothing, and the cursor slides into the edge and sticks there. Raw Input
//! reports the device's own movement, which no clamp can touch. The hooks own
//! position, buttons and suppression; Raw Input owns motion.
//!
//! Hooks are delivered as thread messages, so the thread that installs them
//! must run a `GetMessage` loop and must not block. It gets a thread of its
//! own, and that thread also owns the hidden window Raw Input is delivered to.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;

use tether_proto::{Modifiers, MouseButton, Point};
use tokio::sync::mpsc::UnboundedSender;
use windows_sys::Win32::Foundation::{HMODULE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::{
    GetRawInputData, RegisterRawInputDevices, HRAWINPUT, MOUSE_MOVE_ABSOLUTE, RAWINPUT,
    RAWINPUTDEVICE, RAWINPUTHEADER, RIDEV_INPUTSINK, RIDEV_REMOVE, RID_INPUT, RIM_TYPEMOUSE,
};
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
    /// Set while Raw Input is registered and delivering relative movement. The
    /// hook then leaves motion alone and only keeps `last` up to date; clear,
    /// and it reports movement by differencing positions again — which works
    /// everywhere except against the outer edge of the desktop.
    raw: AtomicBool,
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
                    raw: AtomicBool::new(false),
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
            // Park the pointer in the middle of the primary display and pin it
            // there. While suppressed the cursor does not move, so every hook
            // event would otherwise report the same position and produce a
            // delta of zero after the first one.
            //
            // The middle, specifically — not wherever it happened to stop.
            // Suppression begins the instant the pointer crosses, which is to
            // say hard against the outer edge of the desktop, and Windows
            // clamps every position it reports to that desktop. Pinned on the
            // edge, a push further out lands on the same pixel it started
            // from: the delta is zero in exactly the direction the user is
            // pushing, and the pointer on the other machine cannot be moved
            // away from the edge it arrived at. From the middle there is a
            // half-screen of room in every direction.
            //
            // Through `inject::warp` rather than `SetCursorPos`, so the hook
            // recognises the move as ours and does not read the jump back as
            // the user flinging the mouse across the desk.
            let park = park_point();
            let _ = super::inject::warp(Point::new(park.x, park.y));
            if let Ok(mut anchor) = self.shared.anchor.lock() {
                *anchor = park;
            }
            if let Ok(mut last) = self.shared.last.lock() {
                *last = park;
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

/// Centre of the primary display, in the desktop's own coordinates.
///
/// The primary display starts at the origin on Windows however the other
/// monitors are arranged around it, so its centre is always a real point with
/// room on every side.
fn park_point() -> POINT {
    unsafe {
        POINT {
            x: GetSystemMetrics(SM_CXSCREEN) / 2,
            y: GetSystemMetrics(SM_CYSCREEN) / 2,
        }
    }
}

/// A window for Raw Input to be delivered to. Never shown, never painted.
///
/// `RIDEV_INPUTSINK` needs somewhere to send `WM_INPUT`, and it has to be a
/// window on the thread running the message loop.
unsafe fn create_input_window(module: HMODULE) -> Option<HWND> {
    let name: Vec<u16> = "TetherRawInput\0".encode_utf16().collect();

    let mut class: WNDCLASSW = std::mem::zeroed();
    class.lpfnWndProc = Some(window_proc);
    class.hInstance = module;
    class.lpszClassName = name.as_ptr();
    // Fails with ERROR_CLASS_ALREADY_EXISTS if capture is restarted in the
    // same process, which is not a problem: the existing class is the one we
    // registered and creating a window from it works either way.
    RegisterClassW(&class);

    let window = CreateWindowExW(
        0,
        name.as_ptr(),
        name.as_ptr(),
        WS_POPUP,
        0,
        0,
        0,
        0,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        module,
        std::ptr::null_mut(),
    );
    if window.is_null() {
        None
    } else {
        Some(window)
    }
}

/// Ask for the mouse's own movement, delivered even when another application
/// is in the foreground — which is every moment that matters here.
unsafe fn register_raw_mouse(window: HWND) -> bool {
    let device = RAWINPUTDEVICE {
        usUsagePage: 0x01, // generic desktop controls
        usUsage: 0x02,     // mouse
        dwFlags: RIDEV_INPUTSINK,
        hwndTarget: window,
    };
    RegisterRawInputDevices(&device, 1, std::mem::size_of::<RAWINPUTDEVICE>() as u32) != 0
}

/// Give the registration back before the window it points at goes away.
///
/// Stopping and starting a session in the same process is an ordinary thing to
/// do from the window, and a registration left pointing at a destroyed window
/// is what makes the second start silently deliver no movement at all.
/// `RIDEV_REMOVE` requires a null target.
unsafe fn unregister_raw_mouse() {
    let device = RAWINPUTDEVICE {
        usUsagePage: 0x01,
        usUsage: 0x02,
        dwFlags: RIDEV_REMOVE,
        hwndTarget: std::ptr::null_mut(),
    };
    RegisterRawInputDevices(&device, 1, std::mem::size_of::<RAWINPUTDEVICE>() as u32);
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_INPUT {
        if let Some(shared) = SHARED.get() {
            raw_mouse_moved(shared, lparam as HRAWINPUT);
        }
        // Still hand it on: an application that handles WM_INPUT must call
        // DefWindowProc so the system can release the buffer behind it.
    }
    DefWindowProcW(window, message, wparam, lparam)
}

/// One packet of movement straight from the mouse.
unsafe fn raw_mouse_moved(shared: &CaptureShared, handle: HRAWINPUT) {
    if !shared.raw.load(Ordering::SeqCst) {
        return;
    }

    let mut data: RAWINPUT = std::mem::zeroed();
    let mut size = std::mem::size_of::<RAWINPUT>() as u32;
    let read = GetRawInputData(
        handle,
        RID_INPUT,
        &mut data as *mut RAWINPUT as *mut c_void,
        &mut size,
        std::mem::size_of::<RAWINPUTHEADER>() as u32,
    );
    if read == u32::MAX || data.header.dwType != RIM_TYPEMOUSE {
        return;
    }
    let mouse = &data.data.mouse;

    // Our own injection, come back round. Dropped rather than reported: a
    // machine being driven remotely that read these as a hand on its mouse
    // would grab control straight back. The hook sees the same event and is
    // the one that counts it.
    if mouse.ulExtraInformation as usize == TETHER_EVENT_MARK {
        return;
    }

    if mouse.usFlags & MOUSE_MOVE_ABSOLUTE != 0 {
        // A drawing tablet, a remote-desktop session, or a virtual mouse:
        // `lLastX`/`lLastY` are a position rather than movement, and this
        // device has no relative movement to give. Hand motion back to the
        // hook, which at least works away from the desktop edge.
        if shared.raw.swap(false, Ordering::SeqCst) {
            tracing::info!(
                "the mouse reports absolute positions; falling back to hook \
                 positions for movement"
            );
        }
        return;
    }

    let (dx, dy) = (mouse.lLastX, mouse.lLastY);
    if dx == 0 && dy == 0 {
        return;
    }

    if shared.swallow.load(Ordering::SeqCst) {
        // Suppressed: the cursor is pinned, so its position says nothing.
        shared.emit(LocalEvent::MouseDelta { dx, dy });
        return;
    }

    // Not suppressed, so the cursor is loose on this desktop and where the OS
    // has put it is worth having — it carries the sideways slide the OS
    // performs crossing between screens of different heights. Read here rather
    // than differenced from the hook, because this runs on the same thread the
    // hook does and only ever after the OS has moved the cursor.
    let mut at = POINT { x: 0, y: 0 };
    if GetCursorPos(&mut at) == 0 {
        shared.emit(LocalEvent::MouseDelta { dx, dy });
        return;
    }
    if let Ok(mut last) = shared.last.lock() {
        *last = at;
    }
    shared.emit(LocalEvent::MouseMoved {
        x: at.x,
        y: at.y,
        dx,
        dy,
    });
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

        // Start from where the pointer actually is. Left at the origin, the
        // first movement differences against a corner of the desktop.
        if let Some(shared) = SHARED.get() {
            let mut at = POINT { x: 0, y: 0 };
            if GetCursorPos(&mut at) != 0 {
                if let Ok(mut last) = shared.last.lock() {
                    *last = at;
                }
            }
        }

        let window = create_input_window(module);
        let raw = window.map(|w| register_raw_mouse(w)).unwrap_or(false);
        if let Some(shared) = SHARED.get() {
            shared.raw.store(raw, Ordering::SeqCst);
            shared
                .thread_id
                .store(GetCurrentThreadId(), Ordering::SeqCst);
        }
        if !raw {
            tracing::warn!(
                "could not register the mouse for raw input; movement will be \
                 read from hook positions, which Windows clamps to the desktop \
                 — pushing the pointer past an outer edge may not cross"
            );
        }
        let _ = ready.send(Ok(()));

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        if let Some(shared) = SHARED.get() {
            shared.raw.store(false, Ordering::SeqCst);
        }
        if let Some(window) = window {
            if raw {
                unregister_raw_mouse();
            }
            DestroyWindow(window);
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
            let raw = shared.raw.load(Ordering::SeqCst);

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

            if swallowing && !raw {
                // Suppressing stops the cursor moving, so without putting it
                // back on the anchor every event would measure from a stale
                // point and motion would drift to a halt.
                //
                // SetCursorPos re-enters this hook, and it carries no
                // dwExtraInfo — the mark check above cannot see it, whatever
                // an earlier comment here claimed. The flag is what keeps that
                // re-entry from being reported as a hand on the mouse.
                //
                // Only worth doing for the fallback, which measures movement
                // by differencing positions and so needs a fixed point to
                // measure from. Raw Input needs none of it, and this is a
                // synchronous round trip through the hook on the one thread
                // both hooks share: doing it per event while the host is away
                // is time the keyboard hook does not get, and a hook that runs
                // past `LowLevelHooksTimeout` is skipped entirely for the next
                // event.
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

            // Raw Input reports the device rather than the cursor, so when it
            // is running it is the authority on movement and this branch only
            // keeps `last` up to date. Differencing positions is the fallback
            // for the machines where it could not be registered.
            if !raw && delta != (0, 0) {
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
