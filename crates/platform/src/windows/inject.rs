//! Synthesising input on Windows with `SendInput`.

use std::collections::HashSet;
use std::sync::Mutex;

use tether_proto::{InputEvent, KeyCode, Modifiers, MouseButton, Point};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN,
};

use super::keycodes::{hid_to_scancode, is_extended};

/// Side-button identifiers. Defined here because windows-sys does not export
/// them from a feature we otherwise need.
const XBUTTON1: i32 = 1;
const XBUTTON2: i32 = 2;
use crate::traits::{InputInject, PlatformError, Result};

/// Written into `dwExtraInfo` on everything we synthesise, and checked by our
/// own hooks so they can tell our injections from a real keypress.
///
/// `LLKHF_INJECTED` would tell us an event was injected by *something*, which
/// is not the same question — another tool's injected input is still input we
/// should react to. This identifies ours specifically.
pub const TETHER_EVENT_MARK: usize = 0x7E74_E12D;

struct State {
    /// Scan codes currently held, so `release_all` can let go of them.
    keys: HashSet<u16>,
    buttons: HashSet<MouseButton>,
}

pub struct WindowsInject {
    state: Mutex<State>,
}

impl WindowsInject {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State {
                keys: HashSet::new(),
                buttons: HashSet::new(),
            }),
        }
    }

    pub(super) fn send(inputs: &[INPUT]) -> Result<()> {
        let sent = unsafe {
            SendInput(
                inputs.len() as u32,
                inputs.as_ptr(),
                std::mem::size_of::<INPUT>() as i32,
            )
        };
        if sent as usize != inputs.len() {
            // The usual cause is UIPI: a normal-integrity process cannot inject
            // into an elevated window, and Windows reports that by silently
            // accepting fewer events rather than by failing loudly.
            return Err(PlatformError::backend(
                "SendInput was blocked. The foreground window is probably \
                 running elevated; Tether has to run elevated too in order to \
                 drive it. The secure desktop (UAC prompts, Ctrl+Alt+Del, the \
                 lock screen) accepts no injected input at any privilege level.",
            ));
        }
        Ok(())
    }

    /// Absolute mouse positions are in a 0..65535 space spanning the *virtual*
    /// desktop, which is not the pixel space monitors are reported in. Getting
    /// this conversion wrong is the classic multi-monitor bug where the cursor
    /// lands on the wrong screen or bunches up in a corner.
    pub(super) fn to_absolute(x: i32, y: i32) -> (i32, i32) {
        let left = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
        let top = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
        let width = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) }.max(1);
        let height = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) }.max(1);

        // The -1 matters: without it the rightmost column and bottom row are
        // unreachable, so a cursor can never quite touch the far screen edge —
        // and touching the edge is how you cross to another machine.
        let nx = ((x - left) as i64 * 65535 / (width - 1).max(1) as i64) as i32;
        let ny = ((y - top) as i64 * 65535 / (height - 1).max(1) as i64) as i32;
        (nx.clamp(0, 65535), ny.clamp(0, 65535))
    }

    pub(super) fn mouse_input(flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32, data: i32) -> INPUT {
        // mouseData is unsigned in the API but carries a signed wheel delta;
        // the cast is the documented way to pass a negative scroll.
        let data = data as u32;
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    mouseData: data,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: TETHER_EVENT_MARK,
                },
            },
        }
    }

    fn key_input(scancode: u16, pressed: bool) -> INPUT {
        let mut flags = KEYEVENTF_SCANCODE;
        if is_extended(scancode) {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }
        if !pressed {
            flags |= KEYEVENTF_KEYUP;
        }

        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: 0,
                    wScan: scancode & 0xFF,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: TETHER_EVENT_MARK,
                },
            },
        }
    }

    fn button_flags(button: MouseButton, pressed: bool) -> (MOUSE_EVENT_FLAGS, i32) {
        match button {
            MouseButton::Left if pressed => (MOUSEEVENTF_LEFTDOWN, 0),
            MouseButton::Left => (MOUSEEVENTF_LEFTUP, 0),
            MouseButton::Right if pressed => (MOUSEEVENTF_RIGHTDOWN, 0),
            MouseButton::Right => (MOUSEEVENTF_RIGHTUP, 0),
            MouseButton::Middle if pressed => (MOUSEEVENTF_MIDDLEDOWN, 0),
            MouseButton::Middle => (MOUSEEVENTF_MIDDLEUP, 0),
            MouseButton::Back if pressed => (MOUSEEVENTF_XDOWN, XBUTTON1),
            MouseButton::Back => (MOUSEEVENTF_XUP, XBUTTON1),
            MouseButton::Forward if pressed => (MOUSEEVENTF_XDOWN, XBUTTON2),
            MouseButton::Forward => (MOUSEEVENTF_XUP, XBUTTON2),
            MouseButton::Other(_) if pressed => (MOUSEEVENTF_XDOWN, XBUTTON1),
            MouseButton::Other(_) => (MOUSEEVENTF_XUP, XBUTTON1),
        }
    }
}

impl Default for WindowsInject {
    fn default() -> Self {
        Self::new()
    }
}

impl InputInject for WindowsInject {
    fn inject(&self, event: &InputEvent) -> Result<()> {
        match event {
            InputEvent::MouseMove { x, y } => {
                let (nx, ny) = Self::to_absolute(*x, *y);
                Self::send(&[Self::mouse_input(
                    MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                    nx,
                    ny,
                    0,
                )])
            }

            InputEvent::MouseMoveRelative { dx, dy } => {
                Self::send(&[Self::mouse_input(MOUSEEVENTF_MOVE, *dx, *dy, 0)])
            }

            InputEvent::MouseButton { button, pressed } => {
                let (flags, data) = Self::button_flags(*button, *pressed);
                if let Ok(mut state) = self.state.lock() {
                    if *pressed {
                        state.buttons.insert(*button);
                    } else {
                        state.buttons.remove(button);
                    }
                }
                Self::send(&[Self::mouse_input(flags, 0, 0, data)])
            }

            InputEvent::MouseWheel { dx, dy } => {
                // WHEEL_DELTA (120) is one notch.
                let mut inputs = Vec::new();
                if *dy != 0.0 {
                    inputs.push(Self::mouse_input(
                        MOUSEEVENTF_WHEEL,
                        0,
                        0,
                        (dy * 120.0).round() as i32,
                    ));
                }
                if *dx != 0.0 {
                    inputs.push(Self::mouse_input(
                        MOUSEEVENTF_HWHEEL,
                        0,
                        0,
                        (dx * 120.0).round() as i32,
                    ));
                }
                if inputs.is_empty() {
                    return Ok(());
                }
                Self::send(&inputs)
            }

            InputEvent::Key { key, pressed, .. } => {
                let Some(scancode) = hid_to_scancode(*key) else {
                    tracing::warn!(?key, "no Windows scan code for this HID usage; dropped");
                    return Ok(());
                };
                if let Ok(mut state) = self.state.lock() {
                    if *pressed {
                        state.keys.insert(scancode);
                    } else {
                        state.keys.remove(&scancode);
                    }
                }
                Self::send(&[Self::key_input(scancode, *pressed)])
            }
        }
    }

    fn release_all(&self) -> Result<()> {
        let (keys, buttons) = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| PlatformError::backend("inject state poisoned"))?;
            (
                state.keys.drain().collect::<Vec<_>>(),
                state.buttons.drain().collect::<Vec<_>>(),
            )
        };

        let mut inputs: Vec<INPUT> = keys
            .into_iter()
            .map(|scancode| Self::key_input(scancode, false))
            .collect();

        for button in buttons {
            let (flags, data) = Self::button_flags(button, false);
            inputs.push(Self::mouse_input(flags, 0, 0, data));
        }

        if inputs.is_empty() {
            return Ok(());
        }
        Self::send(&inputs)
    }
}

/// Convert modifier state to nothing — Windows tracks modifiers from the key
/// events themselves, unlike macOS where flags ride on every event.
pub fn modifiers_noop(_: Modifiers) {}

/// Move the cursor without it being mistaken for the user moving it.
///
/// Through `SendInput`, not `SetCursorPos`. Both move the pointer and both
/// wake the low-level hook — but only `SendInput` carries `dwExtraInfo`, which
/// is how the hook recognises our own work. A `SetCursorPos` arrives looking
/// exactly like a hand on the mouse, and the hook reports it as a movement of
/// however far the pointer just jumped.
///
/// That mattered every time the pointer came back from another machine: the
/// warp placing it at the entry point was read straight back as the user
/// flinging the mouse across the desk.
pub fn warp(to: Point) -> Result<()> {
    let (nx, ny) = WindowsInject::to_absolute(to.x, to.y);
    WindowsInject::send(&[WindowsInject::mouse_input(
        MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
        nx,
        ny,
        0,
    )])
}

pub fn cursor_position() -> Result<Point> {
    let mut point = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
    let ok = unsafe { GetCursorPos(&mut point) };
    if ok == 0 {
        return Err(PlatformError::backend("GetCursorPos failed"));
    }
    Ok(Point::new(point.x, point.y))
}

/// Windows has no per-process cursor hiding that works from a background
/// process, so this is a no-op rather than a lie.
///
/// `ShowCursor` is per-thread and only affects windows this process owns, and
/// `SetSystemCursor` would replace the cursor system-wide and permanently.
/// While a remote machine has the pointer, this one parks it in a corner
/// instead — see the note in `mod.rs`.
pub fn set_cursor_visible(_visible: bool) -> Result<()> {
    Ok(())
}

pub fn key_is_supported(key: KeyCode) -> bool {
    hid_to_scancode(key).is_some()
}
