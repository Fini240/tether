//! Synthesising input on macOS with `CGEventPost`.

use std::collections::HashSet;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use tether_proto::{InputEvent, KeyCode, Modifiers, MouseButton, Point};

use super::ffi::*;
use super::keycodes::hid_to_vk;
use crate::traits::{InputInject, PlatformError, Result};

/// Stamped into `kCGEventSourceUserData` on everything we synthesise.
/// Arbitrary; just needs to be a value nothing else plausibly writes.
pub const TETHER_EVENT_MARK: i64 = 0x7E7_4E12_D1CE;

/// Where [`warp`] last put the cursor, waiting to be picked up by the injector.
///
/// A warp moves the very cursor the injector computes its deltas against, but
/// it is a free function on the `Pointer` side of the backend and cannot reach
/// the injector's state. Unshared, the injector goes on believing the pointer
/// is wherever it last drove it — the far side of the screen, or the origin on
/// the first crossing of a session — and the first motion after the pointer
/// enters this machine carries a delta the size of the whole jump. Applications
/// that read the delta fields rather than the position (games, 3D viewports)
/// lurch across the scene once per crossing.
static WARPED: Mutex<Option<CGPoint>> = Mutex::new(None);

/// Everything the injector must remember between events.
///
/// A synthesised event carries no context, so we supply it: a mouse-up needs a
/// position, a drag needs to know a button is held, and a key event needs the
/// current modifier flags or the receiving app sees an unmodified keystroke.
struct State {
    cursor: CGPoint,
    /// CoreGraphics button numbers currently down.
    buttons: HashSet<u32>,
    /// Virtual keycodes currently down, for `release_all`.
    keys: HashSet<u16>,
    flags: u64,
}

pub struct MacInject {
    state: Mutex<State>,
}

// Only raw C calls and a Mutex; no CF object is retained across threads.
unsafe impl Send for MacInject {}
unsafe impl Sync for MacInject {}

impl MacInject {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State {
                cursor: CGPoint { x: 0.0, y: 0.0 },
                buttons: HashSet::new(),
                keys: HashSet::new(),
                flags: 0,
            }),
        }
    }

    fn post(&self, event: CGEventRef, flags: u64) {
        if event.is_null() {
            return;
        }
        unsafe {
            CGEventSetFlags(event, flags);
            // Label it as ours. Our own event tap reads this back and drops
            // the event instead of reporting it as somebody touching this
            // machine's keyboard — without which, a machine being driven
            // remotely would instantly claim input back and the two ends
            // would fight over who is driving.
            CGEventSetIntegerValueField(event, kCGEventSourceUserData, TETHER_EVENT_MARK);
            // Post at the HID level so the event reaches every application,
            // including ones that install their own session-level taps.
            CGEventPost(kCGHIDEventTap, event);
            CFRelease(event);
        }
    }

    /// Motion while a button is held has to be a *dragged* event; a plain
    /// `MouseMoved` during a drag makes text selection and window dragging stop
    /// tracking the pointer.
    fn motion_type(&self, buttons: &HashSet<u32>) -> u32 {
        if buttons.contains(&kCGMouseButtonLeft) {
            kCGEventLeftMouseDragged
        } else if buttons.contains(&kCGMouseButtonRight) {
            kCGEventRightMouseDragged
        } else if !buttons.is_empty() {
            kCGEventOtherMouseDragged
        } else {
            kCGEventMouseMoved
        }
    }

    fn move_to(&self, state: &mut State, to: CGPoint, delta: Option<(i32, i32)>) {
        state.cursor = to;
        let kind = self.motion_type(&state.buttons);
        let button = state
            .buttons
            .iter()
            .copied()
            .next()
            .unwrap_or(kCGMouseButtonLeft);

        unsafe {
            let event = CGEventCreateMouseEvent(ptr::null_mut(), kind, to, button);
            if event.is_null() {
                return;
            }
            if let Some((dx, dy)) = delta {
                // Games and 3D viewports read the delta fields rather than
                // differencing positions; without these they see no movement.
                CGEventSetIntegerValueField(event, kCGMouseEventDeltaX, dx as i64);
                CGEventSetIntegerValueField(event, kCGMouseEventDeltaY, dy as i64);
            }
            self.post(event, state.flags);
        }
    }

    fn press_button(&self, state: &mut State, button: MouseButton, pressed: bool) {
        let cg_button = match button {
            MouseButton::Left => kCGMouseButtonLeft,
            MouseButton::Right => kCGMouseButtonRight,
            MouseButton::Middle => kCGMouseButtonCenter,
            MouseButton::Back => 3,
            MouseButton::Forward => 4,
            MouseButton::Other(n) => n as u32,
        };

        let kind = match (cg_button, pressed) {
            (0, true) => kCGEventLeftMouseDown,
            (0, false) => kCGEventLeftMouseUp,
            (1, true) => kCGEventRightMouseDown,
            (1, false) => kCGEventRightMouseUp,
            (_, true) => kCGEventOtherMouseDown,
            (_, false) => kCGEventOtherMouseUp,
        };

        if pressed {
            state.buttons.insert(cg_button);
        } else {
            state.buttons.remove(&cg_button);
        }

        unsafe {
            let event = CGEventCreateMouseEvent(ptr::null_mut(), kind, state.cursor, cg_button);
            if event.is_null() {
                return;
            }
            CGEventSetIntegerValueField(event, kCGMouseEventButtonNumber, cg_button as i64);
            // TODO(double-click): set kCGMouseEventClickState from a click
            // counter so double- and triple-clicks register. Single clicks are
            // correct without it.
            self.post(event, state.flags);
        }
    }

    fn scroll(&self, state: &State, dx: f32, dy: f32) {
        unsafe {
            let event = CGEventCreateScrollWheelEvent(
                ptr::null_mut(),
                kCGScrollEventUnitLine,
                2,
                dy.round() as i32,
                dx.round() as i32,
            );
            self.post(event, state.flags);
        }
    }

    fn key(&self, state: &mut State, key: KeyCode, pressed: bool, modifiers: Modifiers) {
        let Some(vk) = hid_to_vk(key) else {
            tracing::warn!(?key, "no macOS virtual keycode for this HID usage; dropped");
            return;
        };

        state.flags = flags_from_modifiers(modifiers);
        if pressed {
            state.keys.insert(vk);
        } else {
            state.keys.remove(&vk);
        }

        unsafe {
            let event = CGEventCreateKeyboardEvent(ptr::null_mut(), vk, pressed);
            if event.is_null() {
                return;
            }
            // A modifier posted as a key event does not update the system's
            // modifier state; macOS only honours it as a `flagsChanged`. Apps
            // that watch modifiers directly (Xcode, Photoshop) depend on this.
            if key.is_modifier() {
                CGEventSetType(event, kCGEventFlagsChanged);
            }
            self.post(event, state.flags);
        }
    }
}

impl Default for MacInject {
    fn default() -> Self {
        Self::new()
    }
}

impl InputInject for MacInject {
    fn inject(&self, event: &InputEvent) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| PlatformError::backend("inject state poisoned"))?;

        // A warp since the last event moved the cursor out from under us.
        if let Ok(mut warped) = WARPED.lock() {
            if let Some(to) = warped.take() {
                state.cursor = to;
            }
        }

        match event {
            InputEvent::MouseMove { x, y } => {
                let to = CGPoint {
                    x: *x as f64,
                    y: *y as f64,
                };
                let delta = (
                    (to.x - state.cursor.x) as i32,
                    (to.y - state.cursor.y) as i32,
                );
                self.move_to(&mut state, to, Some(delta));
            }
            InputEvent::MouseMoveRelative { dx, dy } => {
                let to = CGPoint {
                    x: state.cursor.x + *dx as f64,
                    y: state.cursor.y + *dy as f64,
                };
                self.move_to(&mut state, to, Some((*dx, *dy)));
            }
            InputEvent::MouseButton { button, pressed } => {
                self.press_button(&mut state, *button, *pressed)
            }
            InputEvent::MouseWheel { dx, dy } => self.scroll(&state, *dx, *dy),
            InputEvent::Key {
                key,
                pressed,
                modifiers,
                ..
            } => self.key(&mut state, *key, *pressed, *modifiers),
        }
        Ok(())
    }

    fn release_all(&self) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| PlatformError::backend("inject state poisoned"))?;

        let keys: Vec<u16> = state.keys.drain().collect();
        let buttons: Vec<u32> = state.buttons.iter().copied().collect();
        state.flags = 0;

        for vk in keys {
            unsafe {
                let event = CGEventCreateKeyboardEvent(ptr::null_mut(), vk, false);
                if !event.is_null() {
                    if super::keycodes::vk_to_hid(vk).is_some_and(KeyCode::is_modifier) {
                        CGEventSetType(event, kCGEventFlagsChanged);
                    }
                    self.post(event, 0);
                }
            }
        }
        for cg_button in buttons {
            let button = match cg_button {
                0 => MouseButton::Left,
                1 => MouseButton::Right,
                2 => MouseButton::Middle,
                n => MouseButton::Other(n as u8),
            };
            self.press_button(&mut state, button, false);
        }
        Ok(())
    }
}

/// Move the cursor without generating an event, then re-associate the hardware
/// mouse with it.
pub fn warp(to: Point) -> Result<()> {
    unsafe {
        let at = CGPoint {
            x: to.x as f64,
            y: to.y as f64,
        };
        let err = CGWarpMouseCursorPosition(at);
        if err == 0 {
            if let Ok(mut warped) = WARPED.lock() {
                *warped = Some(at);
            }
        }
        if err != 0 {
            return Err(PlatformError::backend(format!(
                "CGWarpMouseCursorPosition failed: {err}"
            )));
        }
        if is_pinned() {
            // Deliberately apart: the pointer is on another machine. Re-couple
            // here and the physical mouse takes the local cursor straight back
            // — which is the whole bug this pinning exists to stop. Move the
            // spot we hold it at instead, so the watchdog does not immediately
            // drag it back to where it used to be.
            if let Ok(mut pin) = PIN.lock() {
                pin.parked = Some(at);
            }
        } else {
            // Without this the hardware mouse stays decoupled for about a
            // quarter of a second and the first flick after a screen switch is
            // swallowed.
            CGAssociateMouseAndMouseCursorPosition(true);
        }
    }
    Ok(())
}

/// Where CoreGraphics currently has the cursor.
///
/// `CGEventCreate` with no source fills in the current location, which is
/// cheaper than creating an event source to ask. It has to be that call and
/// not `CGEventCreateMouseEvent(NULL, kCGEventNull, ..)`: a null event is not
/// a mouse event, so that returns NULL and the read fails every single time.
pub fn position() -> Result<CGPoint> {
    unsafe {
        let event = CGEventCreate(ptr::null_mut());
        if event.is_null() {
            return Err(PlatformError::backend("could not read the cursor position"));
        }
        let at = CGEventGetLocation(event);
        CFRelease(event);
        Ok(at)
    }
}

/// Where the cursor is held while another machine has the pointer.
struct Pin {
    pinned: bool,
    /// The position it was pinned at, and the position the watchdog puts it
    /// back to if something moves it anyway.
    parked: Option<CGPoint>,
    /// When the watchdog last had to intervene, so a pin that is failing
    /// outright cannot turn every mouse event into two round trips to the
    /// window server from inside the event tap's callback — which is how a tap
    /// gets itself disabled for being slow.
    held_at: Option<std::time::Instant>,
}

static PIN: Mutex<Pin> = Mutex::new(Pin {
    pinned: false,
    parked: None,
    held_at: None,
});

/// How often the watchdog will drag a drifting cursor back. Fast enough that a
/// failed pin still looks like a cursor that shivers rather than one that
/// leaves, slow enough to be a rounding error in the tap's time budget.
const HOLD_INTERVAL: std::time::Duration = std::time::Duration::from_millis(16);

/// The same flag as `PIN.pinned`, readable without taking the lock — the event
/// tap consults it on every mouse event and must not block behind anything.
static PINNED: AtomicBool = AtomicBool::new(false);

pub fn is_pinned() -> bool {
    PINNED.load(Ordering::SeqCst)
}

/// Sever the physical mouse from this machine's cursor, or reconnect it.
///
/// Swallowing the motion events at the tap stops applications from seeing the
/// movement, but the cursor sprite is drawn from the HID stream underneath
/// that: suppression alone leaves the arrow gliding around the screen the
/// whole time the user is working on the other machine. Disassociating is what
/// actually holds it still, and — unlike a listen-only tap — the deltas keep
/// arriving, so the remote machine still gets driven.
///
/// Unlike hide/show this is a switch rather than a counter, so a repeated call
/// in the same direction is free; it is skipped anyway to save the round trip
/// to the window server on every mouse event.
pub fn set_pinned(pinned: bool) -> Result<()> {
    let mut pin = PIN
        .lock()
        .map_err(|_| PlatformError::backend("pin state poisoned"))?;
    if pin.pinned == pinned {
        return Ok(());
    }

    // Read the resting place before detaching, not after: once the mouse is
    // loose the position can only get less trustworthy.
    let parked = if pinned { position().ok() } else { None };

    let err = unsafe { CGAssociateMouseAndMouseCursorPosition(!pinned) };
    if err != 0 {
        // Leave the flag alone. Believing we pinned when we did not would stop
        // `warp` from ever re-associating, and the mouse would feel dead for a
        // quarter second after every crossing.
        return Err(PlatformError::backend(format!(
            "could not {} the mouse: CGAssociateMouseAndMouseCursorPosition failed: {err}",
            if pinned { "detach" } else { "reattach" }
        )));
    }

    pin.pinned = pinned;
    pin.parked = parked;
    pin.held_at = None;
    PINNED.store(pinned, Ordering::SeqCst);
    tracing::debug!(pinned, "cursor pin changed");
    Ok(())
}

/// Put the cursor back if it drifted while pinned.
///
/// Called from the event tap for movement it is swallowing. Disassociating the
/// mouse is supposed to make this impossible, and normally it does — but the
/// association is global session state that any process can switch back on,
/// and a cursor that quietly resumes sliding is precisely the symptom that is
/// hard to notice and maddening to live with. Cheap to check, so check.
pub fn hold_parked(at: CGPoint) {
    if !is_pinned() {
        return;
    }
    // Decide under the lock, act outside it: this runs on the event tap's
    // thread, and a tap whose callback waits on anything is a tap macOS
    // switches off for being slow.
    let parked = {
        let Ok(mut pin) = PIN.lock() else { return };
        let Some(parked) = pin.parked else { return };
        // Sub-pixel wobble is not drift. A whole pixel of movement is.
        if (at.x - parked.x).abs() < 1.0 && (at.y - parked.y).abs() < 1.0 {
            return;
        }
        if pin
            .held_at
            .is_some_and(|held| held.elapsed() < HOLD_INTERVAL)
        {
            return;
        }
        pin.held_at = Some(std::time::Instant::now());
        parked
    };
    tracing::debug!("the cursor moved while pinned; putting it back");
    unsafe {
        // Re-assert the detachment first: if the cursor moved, something put
        // the mouse back together, and warping without fixing that just means
        // doing it again on the next event.
        CGAssociateMouseAndMouseCursorPosition(false);
        CGWarpMouseCursorPosition(parked);
    }
}

/// Ask the window server to honour our cursor hiding even though we are not
/// the foreground application.
///
/// `CGDisplayHideCursor` is documented to work only for the frontmost app, and
/// a daemon is never that: the call returns success and the arrow stays on
/// screen, which is why a machine whose pointer had left still showed a cursor
/// sitting there. The `SetsCursorInBackground` connection property is the
/// long-standing way around it — private, unchanged for well over a decade,
/// and used by every comparable tool.
///
/// Looked up with `dlsym` rather than linked. Linking a private symbol means a
/// macOS that drops it stops this program from launching at all; looked up, it
/// merely means the cursor stays visible while the pointer is away — which is
/// cosmetic, since by then it is also pinned in place.
///
/// One documented hole remains, straight from Apple: the Dock keeps cursor
/// control whenever it would be the active target, and blocks this. A cursor
/// resting over the Dock may stay visible. It will not move.
fn allow_hiding_from_the_background() {
    static SYMBOLS: OnceLock<Option<(CGSDefaultConnectionFn, CGSSetConnectionPropertyFn)>> =
        OnceLock::new();

    let Some((connection, set_property)) = SYMBOLS.get_or_init(|| unsafe {
        // Reference something in CoreGraphics before searching for symbols
        // inside it. `RTLD_DEFAULT` searches the images already loaded, and a
        // framework nothing has called yet need not be one of them — the
        // lookup then fails, and this being a `OnceLock`, it would stay failed
        // for the life of the process.
        let _ = CGMainDisplayID();

        let default = libc::dlsym(libc::RTLD_DEFAULT, c"_CGSDefaultConnection".as_ptr());
        let set = libc::dlsym(libc::RTLD_DEFAULT, c"CGSSetConnectionProperty".as_ptr());
        if default.is_null() || set.is_null() {
            tracing::debug!("no CGS connection properties; the cursor may stay visible");
            return None;
        }
        Some((
            std::mem::transmute::<*mut std::ffi::c_void, CGSDefaultConnectionFn>(default),
            std::mem::transmute::<*mut std::ffi::c_void, CGSSetConnectionPropertyFn>(set),
        ))
    }) else {
        return;
    };

    unsafe {
        let key = CFStringCreateWithCString(
            ptr::null(),
            c"SetsCursorInBackground".as_ptr(),
            kCFStringEncodingUTF8,
        );
        if key.is_null() {
            return;
        }
        let id = connection();
        let err = set_property(id, id, key, kCFBooleanTrue);
        CFRelease(key);
        if err != 0 {
            tracing::debug!(err, "the window server refused SetsCursorInBackground");
        }
    }
}

/// Whether the cursor is currently hidden by us.
///
/// `CGDisplayHideCursor` and `CGDisplayShowCursor` are a counter, not a
/// switch: hiding twice takes two shows to undo. Callers reasonably treat
/// visibility as state and set it whenever they recompute it, so this has to
/// only ever act on a change — otherwise a session's worth of "still hidden"
/// runs the count into the hundreds and the cursor never comes back.
static HIDDEN: AtomicBool = AtomicBool::new(false);

pub fn set_cursor_visible(visible: bool) -> Result<()> {
    if HIDDEN.swap(!visible, Ordering::SeqCst) == !visible {
        return Ok(());
    }
    // Ask for the background exemption every time rather than once at startup:
    // it is a property of a window-server connection, and nothing promises the
    // one we hold is the one we held an hour ago.
    allow_hiding_from_the_background();
    unsafe {
        let display = CGMainDisplayID();
        let err = if visible {
            CGDisplayShowCursor(display)
        } else {
            CGDisplayHideCursor(display)
        };
        if err != 0 {
            // Put the flag back, or a failed hide is remembered as a hide and
            // the show that should undo it is skipped.
            HIDDEN.store(visible, Ordering::SeqCst);
            return Err(PlatformError::backend(format!(
                "cursor visibility change failed: {err}"
            )));
        }
    }
    Ok(())
}

pub fn modifiers_from_flags(flags: u64) -> Modifiers {
    let mut mods = Modifiers::NONE;
    mods.set(Modifiers::SHIFT, flags & kCGEventFlagMaskShift != 0);
    mods.set(Modifiers::CONTROL, flags & kCGEventFlagMaskControl != 0);
    mods.set(Modifiers::ALT, flags & kCGEventFlagMaskAlternate != 0);
    mods.set(Modifiers::META, flags & kCGEventFlagMaskCommand != 0);
    mods.set(
        Modifiers::CAPS_LOCK,
        flags & kCGEventFlagMaskAlphaShift != 0,
    );
    mods
}

pub fn flags_from_modifiers(mods: Modifiers) -> u64 {
    let mut flags = 0u64;
    if mods.contains(Modifiers::SHIFT) {
        flags |= kCGEventFlagMaskShift;
    }
    if mods.contains(Modifiers::CONTROL) {
        flags |= kCGEventFlagMaskControl;
    }
    if mods.contains(Modifiers::ALT) {
        flags |= kCGEventFlagMaskAlternate;
    }
    if mods.contains(Modifiers::META) {
        flags |= kCGEventFlagMaskCommand;
    }
    if mods.contains(Modifiers::CAPS_LOCK) {
        flags |= kCGEventFlagMaskAlphaShift;
    }
    flags
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scroll event has to carry the deltas it was handed, on the axes it
    /// was handed them for.
    ///
    /// `CGEventCreateScrollWheelEvent` is variadic from the *second* wheel
    /// onwards, and Apple's arm64 convention passes fixed arguments in
    /// registers and variadic ones on the stack. Declaring `wheel1` on the
    /// wrong side of that boundary still compiles and still returns a
    /// perfectly valid scroll event — it just has the vertical delta sitting
    /// on the horizontal axis, and a vertical axis holding whatever was left
    /// in the register. Which is why scrolling a Mac from another machine
    /// appeared to do nothing whatsoever.
    #[test]
    fn a_scroll_event_carries_the_deltas_it_was_given() {
        for (vertical, horizontal) in [(3i32, 0i32), (-3, 0), (1, 0), (0, 2), (0, -5)] {
            unsafe {
                let event = CGEventCreateScrollWheelEvent(
                    ptr::null_mut(),
                    kCGScrollEventUnitLine,
                    2,
                    vertical,
                    horizontal,
                );
                assert!(!event.is_null(), "no event for ({vertical}, {horizontal})");
                let axis1 = CGEventGetIntegerValueField(event, kCGScrollWheelEventDeltaAxis1);
                let axis2 = CGEventGetIntegerValueField(event, kCGScrollWheelEventDeltaAxis2);
                CFRelease(event);

                assert_eq!(
                    axis1, vertical as i64,
                    "vertical delta of ({vertical}, {horizontal}) landed wrong"
                );
                assert_eq!(
                    axis2, horizontal as i64,
                    "horizontal delta of ({vertical}, {horizontal}) landed wrong"
                );
            }
        }
    }

    #[test]
    fn modifier_flags_round_trip() {
        for mods in [
            Modifiers::NONE,
            Modifiers::META,
            Modifiers::SHIFT | Modifiers::CONTROL,
            Modifiers::ALT | Modifiers::META | Modifiers::CAPS_LOCK,
        ] {
            assert_eq!(modifiers_from_flags(flags_from_modifiers(mods)), mods);
        }
    }
}
