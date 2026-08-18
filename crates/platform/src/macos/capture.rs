//! Capturing the physical keyboard and mouse with a CGEventTap.
//!
//! The tap is created at `kCGHIDEventTap` with `kCGEventTapOptionDefault`,
//! which is the only combination that can both see every event and *suppress*
//! it — returning NULL from the callback drops the event. Suppression is what
//! stops keystrokes landing on the host while the cursor is on a client.
//!
//! A tap requires an active CFRunLoop, and CFRunLoopRun never returns, so it
//! lives on its own thread. Nothing about it is async.

// The CGEvent* constants keep Apple's C names. They contain uppercase letters,
// so they resolve as constants in patterns rather than as catch-all bindings —
// the lint is about the naming convention, not about the match arms silently
// swallowing every event.
#![allow(non_upper_case_globals)]

use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use tether_proto::MouseButton;
use tokio::sync::mpsc::UnboundedSender;

use super::ffi::*;
use super::inject::{hold_parked, modifiers_from_flags, TETHER_EVENT_MARK};
use super::keycodes::vk_to_hid;
use crate::traits::{InputCapture, LocalEvent, PlatformError, Result};

struct CaptureShared {
    sink: Mutex<Option<UnboundedSender<LocalEvent>>>,
    swallow: AtomicBool,
    injected: std::sync::atomic::AtomicU64,
    tap: AtomicPtr<c_void>,
    runloop: AtomicPtr<c_void>,
}

impl CaptureShared {
    fn emit(&self, event: LocalEvent) {
        let guard = match self.sink.lock() {
            Ok(g) => g,
            // A poisoned lock here would mean losing every subsequent event;
            // there is nothing useful to do but carry on without emitting.
            Err(_) => return,
        };
        if let Some(sink) = guard.as_ref() {
            let _ = sink.send(event);
        }
    }
}

pub struct MacCapture {
    shared: Arc<CaptureShared>,
    thread: Option<JoinHandle<()>>,
}

impl MacCapture {
    pub fn new() -> Self {
        Self {
            shared: Arc::new(CaptureShared {
                sink: Mutex::new(None),
                swallow: AtomicBool::new(false),
                injected: std::sync::atomic::AtomicU64::new(0),
                tap: AtomicPtr::new(ptr::null_mut()),
                runloop: AtomicPtr::new(ptr::null_mut()),
            }),
            thread: None,
        }
    }
}

impl Default for MacCapture {
    fn default() -> Self {
        Self::new()
    }
}

impl InputCapture for MacCapture {
    fn start(&mut self, sink: UnboundedSender<LocalEvent>) -> Result<()> {
        if self.thread.is_some() {
            return Ok(());
        }
        *self
            .shared
            .sink
            .lock()
            .map_err(|_| PlatformError::backend("capture sink poisoned"))? = Some(sink);

        let shared = Arc::clone(&self.shared);
        // The tap either installs immediately or not at all, so the thread
        // reports back once and we surface a real error instead of silently
        // running with a dead tap.
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<std::result::Result<(), String>>();

        let handle = std::thread::Builder::new()
            .name("tether-eventtap".into())
            .spawn(move || run_tap(shared, ready_tx))
            .map_err(|e| PlatformError::backend(format!("could not spawn tap thread: {e}")))?;

        match ready_rx.recv() {
            Ok(Ok(())) => {
                self.thread = Some(handle);
                Ok(())
            }
            Ok(Err(message)) => Err(PlatformError::PermissionDenied(message)),
            Err(_) => Err(PlatformError::backend("tap thread exited during startup")),
        }
    }

    fn stop(&mut self) {
        // Belt and braces: never leave the mouse detached from the cursor with
        // nothing running that would reattach it. Every ordinary path unpins
        // before it gets here, but a panic or an early return on the way out
        // would otherwise leave the user with a mouse that does nothing.
        self.shared.swallow.store(false, Ordering::SeqCst);
        let _ = super::inject::set_pinned(false);

        let runloop = self.shared.runloop.swap(ptr::null_mut(), Ordering::SeqCst);
        if !runloop.is_null() {
            unsafe { CFRunLoopStop(runloop) };
        }
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
        if let Ok(mut sink) = self.shared.sink.lock() {
            *sink = None;
        }
    }

    fn set_swallow(&self, swallow: bool) {
        self.shared.swallow.store(swallow, Ordering::SeqCst);
    }

    fn injected_filtered(&self) -> u64 {
        self.shared.injected.load(Ordering::Relaxed)
    }
}

impl Drop for MacCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_tap(
    shared: Arc<CaptureShared>,
    ready: std::sync::mpsc::Sender<std::result::Result<(), String>>,
) {
    let mask = event_mask(kCGEventMouseMoved)
        | event_mask(kCGEventLeftMouseDown)
        | event_mask(kCGEventLeftMouseUp)
        | event_mask(kCGEventRightMouseDown)
        | event_mask(kCGEventRightMouseUp)
        | event_mask(kCGEventOtherMouseDown)
        | event_mask(kCGEventOtherMouseUp)
        | event_mask(kCGEventLeftMouseDragged)
        | event_mask(kCGEventRightMouseDragged)
        | event_mask(kCGEventOtherMouseDragged)
        | event_mask(kCGEventScrollWheel)
        | event_mask(kCGEventKeyDown)
        | event_mask(kCGEventKeyUp)
        | event_mask(kCGEventFlagsChanged);

    // The callback borrows this for the lifetime of the run loop. Ownership
    // comes back at the end of the function, after CFRunLoopRun returns.
    let user_info = Arc::into_raw(Arc::clone(&shared)) as *mut c_void;

    unsafe {
        let tap = CGEventTapCreate(
            kCGHIDEventTap,
            kCGHeadInsertEventTap,
            kCGEventTapOptionDefault,
            mask,
            tap_callback,
            user_info,
        );

        if tap.is_null() {
            drop(Arc::from_raw(user_info as *const CaptureShared));
            let _ = ready.send(Err(
                "macOS refused the event tap. Grant this binary Accessibility \
                 access in System Settings → Privacy & Security → Accessibility, \
                 then start it again."
                    .to_string(),
            ));
            return;
        }

        let source = CFMachPortCreateRunLoopSource(ptr::null(), tap, 0);
        let runloop = CFRunLoopGetCurrent();
        shared.tap.store(tap, Ordering::SeqCst);
        shared.runloop.store(runloop, Ordering::SeqCst);

        CFRunLoopAddSource(runloop, source, kCFRunLoopCommonModes);
        CGEventTapEnable(tap, true);
        let _ = ready.send(Ok(()));

        CFRunLoopRun();

        CGEventTapEnable(tap, false);
        CFRelease(source);
        CFRelease(tap);
        shared.tap.store(ptr::null_mut(), Ordering::SeqCst);
        drop(Arc::from_raw(user_info as *const CaptureShared));
    }
}

extern "C" fn tap_callback(
    _proxy: *mut c_void,
    event_type: u32,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef {
    if user_info.is_null() {
        return event;
    }
    // Borrowed, not owned — `run_tap` holds the only strong reference.
    let shared = unsafe { &*(user_info as *const CaptureShared) };

    // macOS disables a tap whose callback ran long. Re-arm it rather than
    // going quietly deaf.
    if event_type == kCGEventTapDisabledByTimeout || event_type == kCGEventTapDisabledByUserInput {
        let tap = shared.tap.load(Ordering::SeqCst);
        if !tap.is_null() {
            tracing::warn!("event tap was disabled by the system; re-enabling");
            unsafe { CGEventTapEnable(tap, true) };
        }
        // A tap that will not come back is the one case where a pinned cursor
        // becomes a trap. Nothing is being captured any more, so the pointer
        // on the other machine has stopped moving — and if the mouse here is
        // still detached from its cursor, this machine has no working pointer
        // either and no way for the user to fix it. Give the mouse back.
        if tap.is_null() || !unsafe { CGEventTapIsEnabled(tap) } {
            tracing::error!("the event tap will not re-enable; releasing the cursor");
            let _ = super::inject::set_pinned(false);
            let _ = super::inject::set_cursor_visible(true);
            shared.swallow.store(false, Ordering::SeqCst);
        }
        return event;
    }

    // Our own injection? Let it through to the OS untouched, but do not report
    // it as user input. This is what stops a remotely-driven machine from
    // seeing the incoming events as a local touch and claiming control back.
    if unsafe { CGEventGetIntegerValueField(event, kCGEventSourceUserData) } == TETHER_EVENT_MARK {
        shared.injected.fetch_add(1, Ordering::Relaxed);
        return event;
    }

    let flags = unsafe { CGEventGetFlags(event) };
    let modifiers = modifiers_from_flags(flags);

    let local = match event_type {
        kCGEventMouseMoved
        | kCGEventLeftMouseDragged
        | kCGEventRightMouseDragged
        | kCGEventOtherMouseDragged => {
            let dx = unsafe { CGEventGetIntegerValueField(event, kCGMouseEventDeltaX) } as i32;
            let dy = unsafe { CGEventGetIntegerValueField(event, kCGMouseEventDeltaY) } as i32;
            if dx == 0 && dy == 0 {
                return pass_through(shared, event);
            }
            if shared.swallow.load(Ordering::SeqCst) {
                // Suppressed: the cursor is pinned, so its position says
                // nothing about what the user is doing. Only the device
                // movement is real.
                //
                // The position is still worth a glance: if it has moved, the
                // pin is not holding and the cursor is quietly sliding around
                // this machine while the user works on another. `hold_parked`
                // puts it back and does nothing at all in the normal case.
                hold_parked(unsafe { CGEventGetLocation(event) });
                Some(LocalEvent::MouseDelta { dx, dy })
            } else {
                let at = unsafe { CGEventGetLocation(event) };
                Some(LocalEvent::MouseMoved {
                    x: at.x as i32,
                    y: at.y as i32,
                    dx,
                    dy,
                })
            }
        }

        kCGEventLeftMouseDown
        | kCGEventLeftMouseUp
        | kCGEventRightMouseDown
        | kCGEventRightMouseUp
        | kCGEventOtherMouseDown
        | kCGEventOtherMouseUp => {
            let number = unsafe { CGEventGetIntegerValueField(event, kCGMouseEventButtonNumber) };
            let button = match number {
                0 => MouseButton::Left,
                1 => MouseButton::Right,
                2 => MouseButton::Middle,
                3 => MouseButton::Back,
                4 => MouseButton::Forward,
                n => MouseButton::Other(n as u8),
            };
            let pressed = matches!(
                event_type,
                kCGEventLeftMouseDown | kCGEventRightMouseDown | kCGEventOtherMouseDown
            );
            Some(LocalEvent::Button { button, pressed })
        }

        kCGEventScrollWheel => {
            let dy = unsafe { CGEventGetDoubleValueField(event, kCGScrollWheelEventDeltaAxis1) };
            let dx = unsafe { CGEventGetDoubleValueField(event, kCGScrollWheelEventDeltaAxis2) };
            Some(LocalEvent::Wheel {
                dx: dx as f32,
                dy: dy as f32,
            })
        }

        kCGEventKeyDown | kCGEventKeyUp => {
            let vk = unsafe { CGEventGetIntegerValueField(event, kCGKeyboardEventKeycode) } as u16;
            let repeat =
                unsafe { CGEventGetIntegerValueField(event, kCGKeyboardEventAutorepeat) } != 0;
            match vk_to_hid(vk) {
                Some(key) => Some(LocalEvent::Key {
                    key,
                    pressed: event_type == kCGEventKeyDown,
                    modifiers,
                    repeat,
                }),
                None => {
                    tracing::trace!(vk, "unmapped macOS keycode; passing through locally");
                    return pass_through(shared, event);
                }
            }
        }

        kCGEventFlagsChanged => {
            let vk = unsafe { CGEventGetIntegerValueField(event, kCGKeyboardEventKeycode) } as u16;
            match vk_to_hid(vk) {
                // A flagsChanged says nothing about up vs down directly. Infer
                // it: if this key's role is set in the new flags, it just went
                // down. Correct unless both the left and right key of a pair
                // are held, where the release of the first looks like a
                // no-op — harmless, since the modifier really is still down.
                Some(key) => key.modifier_bit().map(|bit| LocalEvent::Key {
                    key,
                    pressed: modifiers.contains(bit),
                    modifiers,
                    repeat: false,
                }),
                None => None,
            }
        }

        _ => None,
    };

    if let Some(local) = local {
        // Anything that reaches here is, as far as this machine can tell,
        // somebody's hand — it is unmarked, so it is not ours. When it is big
        // enough to take control away from whoever is driving, say where it
        // came from: pid 0 is real hardware, our own pid means an injection
        // that lost its mark, and anything else is another program moving the
        // pointer. Guessing between those three from the outside is hopeless.
        if claims_control(&local) {
            let pid = unsafe { CGEventGetIntegerValueField(event, kCGEventSourceUnixProcessID) };
            tracing::debug!(
                pid,
                ours = pid == std::process::id() as i64,
                hardware = pid == 0,
                ?local,
                "unmarked input that is enough to claim control"
            );
        }
        shared.emit(local);
    }
    pass_through(shared, event)
}

/// Would this be enough for the daemon to hand control to this machine?
///
/// Mirrors `is_deliberate` in the daemon. Kept in step by hand: this one only
/// decides whether a line is worth logging, so drift costs a confusing debug
/// session rather than wrong behaviour.
fn claims_control(local: &LocalEvent) -> bool {
    match local {
        LocalEvent::Key { pressed, .. } | LocalEvent::Button { pressed, .. } => *pressed,
        LocalEvent::MouseDelta { dx, dy } | LocalEvent::MouseMoved { dx, dy, .. } => {
            dx.abs() + dy.abs() >= 3
        }
        LocalEvent::Wheel { dx, dy } => dx.abs() + dy.abs() >= 1.0,
    }
}

/// Returning NULL drops the event; returning it delivers it locally.
fn pass_through(shared: &CaptureShared, event: CGEventRef) -> CGEventRef {
    if shared.swallow.load(Ordering::SeqCst) {
        ptr::null_mut()
    } else {
        event
    }
}
