//! Synthesising input on macOS with `CGEventPost`.

use std::collections::HashSet;
use std::ptr;
use std::sync::Mutex;

use tether_proto::{InputEvent, KeyCode, Modifiers, MouseButton, Point};

use super::ffi::*;
use super::keycodes::hid_to_vk;
use crate::traits::{InputInject, PlatformError, Result};

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
        let err = CGWarpMouseCursorPosition(CGPoint {
            x: to.x as f64,
            y: to.y as f64,
        });
        if err != 0 {
            return Err(PlatformError::backend(format!(
                "CGWarpMouseCursorPosition failed: {err}"
            )));
        }
        // Without this the hardware mouse stays decoupled for about a quarter
        // of a second and the first flick after a screen switch is swallowed.
        CGAssociateMouseAndMouseCursorPosition(true);
    }
    Ok(())
}

pub fn set_cursor_visible(visible: bool) -> Result<()> {
    unsafe {
        let display = CGMainDisplayID();
        let err = if visible {
            CGDisplayShowCursor(display)
        } else {
            CGDisplayHideCursor(display)
        };
        if err != 0 {
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
