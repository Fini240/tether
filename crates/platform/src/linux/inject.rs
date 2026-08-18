//! Synthesising input through `uinput`.
//!
//! Two virtual devices rather than one, because the kernel classifies a device
//! by what it can emit and userspace acts on that classification. A single
//! device advertising both a full keyboard and an absolute pointer is read by
//! libinput as a tablet-with-buttons and handled accordingly — the pointer
//! works and the keyboard quietly does not.
//!
//! The pointer is absolute rather than relative. The protocol hands a client
//! the position the host decided on, and turning that back into a relative
//! movement means knowing where this machine's cursor currently is — which is
//! the display server's secret, and the one thing evdev cannot ask for. An
//! absolute device sidesteps the question: the coordinate *is* the message.
//!
//! There is no marking of our own events the way the other two backends do,
//! and none is needed: a uinput device is a distinct device node, and the
//! capture side simply never opens the ones we created.

use std::collections::HashSet;
use std::sync::Mutex;

use evdev::uinput::{VirtualDevice, VirtualDeviceBuilder};
use evdev::{
    AbsInfo, AbsoluteAxisType, AttributeSet, EventType, InputEvent, Key, RelativeAxisType,
    UinputAbsSetup,
};
use tether_proto::{InputEvent as Event, MouseButton, Point};

use super::keycodes::hid_to_key;
use crate::traits::{InputInject, PlatformError, Result};

/// The absolute pointer's coordinate range.
///
/// Fixed rather than the desktop's pixel size: the range is baked into the
/// device when it is created, and a screen that changes resolution afterwards
/// would leave a device describing a desktop that no longer exists. A fixed
/// range with the mapping done here survives that — only the divisor changes.
const ABS_MAX: i32 = 65535;

/// The name the virtual devices report. Deliberately recognisable: it is what
/// shows up in `libinput list-devices`, and what the capture side skips.
pub const VIRTUAL_KEYBOARD: &str = "Tether virtual keyboard";
pub const VIRTUAL_POINTER: &str = "Tether virtual pointer";

pub struct LinuxInject {
    state: Mutex<State>,
}

struct State {
    keyboard: VirtualDevice,
    pointer: VirtualDevice,
    /// The desktop rectangle absolute coordinates are scaled against.
    desktop: (i32, i32),
    keys: HashSet<u16>,
    buttons: HashSet<Key>,
}

// Only file descriptors and a Mutex; nothing here is tied to a thread.
unsafe impl Send for LinuxInject {}
unsafe impl Sync for LinuxInject {}

impl LinuxInject {
    /// `desktop` is the full extent of this machine's screens, used to scale
    /// absolute positions into the device's fixed range.
    pub fn new(desktop: (i32, i32)) -> Result<Self> {
        let keyboard = build_keyboard()?;
        let pointer = build_pointer()?;
        Ok(Self {
            state: Mutex::new(State {
                keyboard,
                pointer,
                desktop: (desktop.0.max(1), desktop.1.max(1)),
                keys: HashSet::new(),
                buttons: HashSet::new(),
            }),
        })
    }
}

fn uinput_error(what: &str, err: std::io::Error) -> PlatformError {
    if err.kind() == std::io::ErrorKind::PermissionDenied {
        return PlatformError::PermissionDenied(format!(
            "cannot create a virtual input device ({what}): permission denied on \
             /dev/uinput. Add yourself to a group that owns it and log back in — \
             on most distributions:\n    sudo usermod -aG input $USER\nand if \
             /dev/uinput is root-only, a udev rule to widen it:\n    echo \
             'KERNEL==\"uinput\", GROUP=\"input\", MODE=\"0660\"' | sudo tee \
             /etc/udev/rules.d/99-tether.rules"
        ));
    }
    if err.kind() == std::io::ErrorKind::NotFound {
        return PlatformError::backend(format!(
            "cannot create a virtual input device ({what}): /dev/uinput does not \
             exist. Load the module with `sudo modprobe uinput`, and add it to \
             /etc/modules-load.d to have it there after a reboot."
        ));
    }
    PlatformError::backend(format!(
        "cannot create a virtual input device ({what}): {err}"
    ))
}

fn build_keyboard() -> Result<VirtualDevice> {
    let mut keys = AttributeSet::<Key>::new();
    // Every code the mapping table can produce. Advertising the full 0..=255
    // range instead would have the desktop treat this as a device with keys
    // that do not exist, which shows up as odd entries in keyboard settings.
    for usage in 0u16..=0xE7 {
        if let Some(code) = hid_to_key(tether_proto::KeyCode(usage)) {
            keys.insert(Key::new(code));
        }
    }

    VirtualDeviceBuilder::new()
        .map_err(|e| uinput_error("keyboard", e))?
        .name(VIRTUAL_KEYBOARD)
        .with_keys(&keys)
        .map_err(|e| uinput_error("keyboard keys", e))?
        .build()
        .map_err(|e| uinput_error("keyboard", e))
}

fn build_pointer() -> Result<VirtualDevice> {
    let mut buttons = AttributeSet::<Key>::new();
    for button in [
        Key::BTN_LEFT,
        Key::BTN_RIGHT,
        Key::BTN_MIDDLE,
        Key::BTN_SIDE,
        Key::BTN_EXTRA,
    ] {
        buttons.insert(button);
    }

    let mut wheels = AttributeSet::<RelativeAxisType>::new();
    wheels.insert(RelativeAxisType::REL_WHEEL);
    wheels.insert(RelativeAxisType::REL_HWHEEL);

    let axis = AbsInfo::new(0, 0, ABS_MAX, 0, 0, 1);

    VirtualDeviceBuilder::new()
        .map_err(|e| uinput_error("pointer", e))?
        .name(VIRTUAL_POINTER)
        .with_keys(&buttons)
        .map_err(|e| uinput_error("pointer buttons", e))?
        .with_relative_axes(&wheels)
        .map_err(|e| uinput_error("pointer wheels", e))?
        .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisType::ABS_X, axis))
        .map_err(|e| uinput_error("pointer x axis", e))?
        .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisType::ABS_Y, axis))
        .map_err(|e| uinput_error("pointer y axis", e))?
        .build()
        .map_err(|e| uinput_error("pointer", e))
}

fn cg_button(button: MouseButton) -> Key {
    match button {
        MouseButton::Left => Key::BTN_LEFT,
        MouseButton::Right => Key::BTN_RIGHT,
        MouseButton::Middle => Key::BTN_MIDDLE,
        MouseButton::Back => Key::BTN_SIDE,
        MouseButton::Forward => Key::BTN_EXTRA,
        MouseButton::Other(_) => Key::BTN_LEFT,
    }
}

impl State {
    /// Scale a position in this machine's pixels into the device's range.
    fn to_abs(&self, at: Point) -> (i32, i32) {
        let (width, height) = self.desktop;
        let x = (at.x.clamp(0, width - 1) as i64 * ABS_MAX as i64) / (width - 1).max(1) as i64;
        let y = (at.y.clamp(0, height - 1) as i64 * ABS_MAX as i64) / (height - 1).max(1) as i64;
        (x as i32, y as i32)
    }

    fn emit_pointer(&mut self, events: &[InputEvent]) -> Result<()> {
        self.pointer
            .emit(events)
            .map_err(|e| PlatformError::backend(format!("could not emit pointer input: {e}")))
    }

    fn emit_keyboard(&mut self, events: &[InputEvent]) -> Result<()> {
        self.keyboard
            .emit(events)
            .map_err(|e| PlatformError::backend(format!("could not emit key input: {e}")))
    }
}

impl InputInject for LinuxInject {
    fn inject(&self, event: &Event) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| PlatformError::backend("inject state poisoned"))?;

        match event {
            Event::MouseMove { x, y } => {
                let (ax, ay) = state.to_abs(Point::new(*x, *y));
                state.emit_pointer(&[
                    InputEvent::new(EventType::ABSOLUTE, AbsoluteAxisType::ABS_X.0, ax),
                    InputEvent::new(EventType::ABSOLUTE, AbsoluteAxisType::ABS_Y.0, ay),
                ])
            }

            Event::MouseMoveRelative { dx, dy } => {
                // The device is absolute, so relative motion has nowhere to go
                // — there is no cursor position here to add it to. Dropped
                // rather than faked: this variant exists for pointer-locked
                // games, and a fake is worse than nothing in one of those.
                tracing::debug!(dx, dy, "relative motion is not supported on this backend");
                Ok(())
            }

            Event::MouseButton { button, pressed } => {
                let key = cg_button(*button);
                if *pressed {
                    state.buttons.insert(key);
                } else {
                    state.buttons.remove(&key);
                }
                let value = i32::from(*pressed);
                state.emit_pointer(&[InputEvent::new(EventType::KEY, key.code(), value)])
            }

            Event::MouseWheel { dx, dy } => {
                // Detents, which is what REL_WHEEL counts. Rounding away from
                // zero so a small trackpad flick still moves something rather
                // than being truncated into no scroll at all.
                let vertical = round_away(*dy);
                let horizontal = round_away(*dx);
                let mut events = Vec::new();
                if vertical != 0 {
                    events.push(InputEvent::new(
                        EventType::RELATIVE,
                        RelativeAxisType::REL_WHEEL.0,
                        vertical,
                    ));
                }
                if horizontal != 0 {
                    events.push(InputEvent::new(
                        EventType::RELATIVE,
                        RelativeAxisType::REL_HWHEEL.0,
                        horizontal,
                    ));
                }
                if events.is_empty() {
                    return Ok(());
                }
                state.emit_pointer(&events)
            }

            Event::Key { key, pressed, .. } => {
                let Some(code) = hid_to_key(*key) else {
                    tracing::trace!(?key, "no Linux code for this key; dropped");
                    return Ok(());
                };
                if *pressed {
                    state.keys.insert(code);
                } else {
                    state.keys.remove(&code);
                }
                let value = i32::from(*pressed);
                state.emit_keyboard(&[InputEvent::new(EventType::KEY, code, value)])
            }
        }
    }

    fn release_all(&self) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| PlatformError::backend("inject state poisoned"))?;

        let keys: Vec<u16> = state.keys.drain().collect();
        if !keys.is_empty() {
            let events: Vec<InputEvent> = keys
                .into_iter()
                .map(|code| InputEvent::new(EventType::KEY, code, 0))
                .collect();
            state.emit_keyboard(&events)?;
        }

        let buttons: Vec<Key> = state.buttons.drain().collect();
        if !buttons.is_empty() {
            let events: Vec<InputEvent> = buttons
                .into_iter()
                .map(|key| InputEvent::new(EventType::KEY, key.code(), 0))
                .collect();
            state.emit_pointer(&events)?;
        }
        Ok(())
    }
}

/// Round to a whole detent without ever rounding a real scroll down to none.
fn round_away(value: f32) -> i32 {
    if value == 0.0 {
        0
    } else if value > 0.0 {
        value.max(1.0).round() as i32
    } else {
        value.min(-1.0).round() as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_small_flick_still_scrolls() {
        // Truncation would turn a slow trackpad scroll into nothing at all.
        assert_eq!(round_away(0.2), 1);
        assert_eq!(round_away(-0.2), -1);
        assert_eq!(round_away(0.0), 0);
        assert_eq!(round_away(3.4), 3);
        assert_eq!(round_away(-3.4), -3);
    }
}
