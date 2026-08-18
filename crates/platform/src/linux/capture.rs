//! Capturing the physical keyboard and mouse by reading `/dev/input` directly.
//!
//! One thread per device, each blocking on `poll` with a short timeout so it
//! can notice a change of state and a shutdown between events.
//!
//! Suppression is `EVIOCGRAB`, which takes a device for this process alone.
//! That is a stronger thing than the other two backends manage and it comes
//! with a bonus: a grabbed mouse is invisible to the display server, so the
//! local cursor stops dead without any of the cursor-pinning machinery macOS
//! needs. The same call does both jobs.
//!
//! Two consequences worth knowing:
//!
//! * A grab that leaks is a dead keyboard. Every exit path ungrabs, and the
//!   kernel drops the grab if the process dies anyway — but a *hung* process
//!   still holding one is the worst failure this backend has, which is why the
//!   poll timeout is short and there is no lock held across it.
//! * There is no cursor position to report. The display server owns that and
//!   will not say, so movement is always a delta. The router copes — it is the
//!   same path a suppressed machine already takes — at the cost of not knowing
//!   about a sideways slide between mismatched screens.

use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use evdev::{Device, InputEventKind, Key, RelativeAxisType};
use tether_proto::{Modifiers, MouseButton};
use tokio::sync::mpsc::UnboundedSender;

use super::inject::{VIRTUAL_KEYBOARD, VIRTUAL_POINTER};
use super::keycodes::key_to_hid;
use crate::traits::{InputCapture, LocalEvent, PlatformError, Result};

/// How long a reader waits before looking at the swallow flag again.
///
/// The upper bound on how long suppression takes to come into effect, so it
/// wants to be short. It is also how often an idle machine wakes up per input
/// device, so it does not want to be tiny.
const POLL_TIMEOUT_MS: i32 = 20;

struct Shared {
    sink: Mutex<Option<UnboundedSender<LocalEvent>>>,
    swallow: AtomicBool,
    running: AtomicBool,
    modifiers: Mutex<Modifiers>,
}

impl Shared {
    fn emit(&self, event: LocalEvent) {
        let Ok(guard) = self.sink.lock() else { return };
        if let Some(sink) = guard.as_ref() {
            let _ = sink.send(event);
        }
    }

    fn modifiers(&self) -> Modifiers {
        self.modifiers.lock().map(|m| *m).unwrap_or(Modifiers::NONE)
    }

    fn set_modifier(&self, bit: Modifiers, down: bool) {
        if let Ok(mut modifiers) = self.modifiers.lock() {
            modifiers.set(bit, down);
        }
    }
}

pub struct LinuxCapture {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
}

impl LinuxCapture {
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Shared {
                sink: Mutex::new(None),
                swallow: AtomicBool::new(false),
                running: AtomicBool::new(false),
                modifiers: Mutex::new(Modifiers::NONE),
            }),
            threads: Vec::new(),
        }
    }
}

impl Default for LinuxCapture {
    fn default() -> Self {
        Self::new()
    }
}

impl InputCapture for LinuxCapture {
    fn start(&mut self, sink: UnboundedSender<LocalEvent>) -> Result<()> {
        if !self.threads.is_empty() {
            return Ok(());
        }
        *self
            .shared
            .sink
            .lock()
            .map_err(|_| PlatformError::backend("capture sink poisoned"))? = Some(sink);

        let devices = interesting_devices()?;
        if devices.is_empty() {
            return Err(PlatformError::PermissionDenied(
                "no readable keyboard or mouse in /dev/input. Add yourself to the \
                 group that owns those nodes and log back in — on most \
                 distributions:\n    sudo usermod -aG input $USER"
                    .into(),
            ));
        }

        self.shared.running.store(true, Ordering::SeqCst);
        for device in devices {
            let shared = Arc::clone(&self.shared);
            let name = device.name().unwrap_or("input device").to_string();
            let handle = std::thread::Builder::new()
                .name(format!("tether-evdev-{name}"))
                .spawn(move || read_device(device, shared))
                .map_err(|e| {
                    PlatformError::backend(format!("could not spawn a reader thread: {e}"))
                })?;
            self.threads.push(handle);
        }
        Ok(())
    }

    fn stop(&mut self) {
        self.shared.running.store(false, Ordering::SeqCst);
        self.shared.swallow.store(false, Ordering::SeqCst);
        for handle in self.threads.drain(..) {
            let _ = handle.join();
        }
        if let Ok(mut sink) = self.shared.sink.lock() {
            *sink = None;
        }
    }

    fn set_swallow(&self, swallow: bool) {
        self.shared.swallow.store(swallow, Ordering::SeqCst);
    }

    fn injected_filtered(&self) -> Option<u64> {
        // Nothing to count. Our injections come out of uinput nodes this side
        // never opens, so there is no event here to recognise and drop — the
        // separation is structural rather than a filter. `None` says exactly
        // that; a zero would read as "the filter caught nothing", which is the
        // one thing that would mean it was broken.
        None
    }
}

impl Drop for LinuxCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Every device worth reading: real keyboards and real pointers.
///
/// Ours are skipped by name. Without that a client injecting a keystroke reads
/// it straight back as somebody typing here, claims control, and the two
/// machines fight over who is driving — the same failure the other backends
/// avoid by marking their events, arrived at from the other end.
fn interesting_devices() -> Result<Vec<Device>> {
    let mut devices = Vec::new();
    for (path, device) in evdev::enumerate() {
        let name = device.name().unwrap_or_default().to_string();
        if name == VIRTUAL_KEYBOARD || name == VIRTUAL_POINTER {
            tracing::debug!(%name, "skipping our own virtual device");
            continue;
        }
        if !is_keyboard(&device) && !is_pointer(&device) {
            continue;
        }
        tracing::debug!(path = %path.display(), %name, "capturing");
        devices.push(device);
    }
    Ok(devices)
}

/// Has letter keys, so it is a keyboard rather than a lid switch or a button
/// pretending to be one.
fn is_keyboard(device: &Device) -> bool {
    device
        .supported_keys()
        .is_some_and(|keys| keys.contains(Key::KEY_A) && keys.contains(Key::KEY_Z))
}

/// Reports relative motion and has a left button: a mouse or a trackpad, not a
/// dial or a volume wheel.
fn is_pointer(device: &Device) -> bool {
    let moves = device
        .supported_relative_axes()
        .is_some_and(|axes| axes.contains(RelativeAxisType::REL_X));
    let clicks = device
        .supported_keys()
        .is_some_and(|keys| keys.contains(Key::BTN_LEFT));
    moves && clicks
}

/// Read one device until asked to stop, grabbing and releasing it as the
/// swallow flag changes.
fn read_device(mut device: Device, shared: Arc<Shared>) {
    let fd = device.as_raw_fd();
    let mut grabbed = false;
    // Motion is accumulated across a packet and emitted at the SYN_REPORT that
    // ends it. A mouse reports x and y as two events, and forwarding them
    // separately doubles the event rate and halves the accuracy of every
    // deliberate-movement threshold downstream.
    let (mut dx, mut dy) = (0i32, 0i32);
    let (mut wheel_x, mut wheel_y) = (0f32, 0f32);

    while shared.running.load(Ordering::SeqCst) {
        let want = shared.swallow.load(Ordering::SeqCst);
        if want != grabbed {
            let result = if want { device.grab() } else { device.ungrab() };
            match result {
                Ok(()) => grabbed = want,
                Err(err) => {
                    // Losing a grab means this device's input reaches local
                    // apps while the pointer is elsewhere. Worth saying, and
                    // not worth dying over.
                    tracing::warn!(%err, grab = want, "could not change the grab on a device");
                }
            }
        }

        if !wait_readable(fd) {
            continue;
        }

        let events = match device.fetch_events() {
            Ok(events) => events,
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(err) => {
                tracing::warn!(%err, "a device stopped reading; dropping it");
                break;
            }
        };

        for event in events {
            match event.kind() {
                InputEventKind::Key(key) => {
                    on_key(&shared, key, event.value());
                }
                InputEventKind::RelAxis(axis) => match axis {
                    RelativeAxisType::REL_X => dx += event.value(),
                    RelativeAxisType::REL_Y => dy += event.value(),
                    RelativeAxisType::REL_WHEEL => wheel_y += event.value() as f32,
                    RelativeAxisType::REL_HWHEEL => wheel_x += event.value() as f32,
                    _ => {}
                },
                InputEventKind::Synchronization(_) => {
                    if dx != 0 || dy != 0 {
                        shared.emit(LocalEvent::MouseDelta { dx, dy });
                        dx = 0;
                        dy = 0;
                    }
                    if wheel_x != 0.0 || wheel_y != 0.0 {
                        shared.emit(LocalEvent::Wheel {
                            dx: wheel_x,
                            dy: wheel_y,
                        });
                        wheel_x = 0.0;
                        wheel_y = 0.0;
                    }
                }
                _ => {}
            }
        }
    }

    if grabbed {
        // The one thing that must not be skipped: a device left grabbed by a
        // thread that has stopped reading it is a keyboard that does nothing.
        let _ = device.ungrab();
    }
}

/// Wait for the device to have something to say, or the timeout.
///
/// `poll` rather than a blocking read so the loop above keeps its promise to
/// notice a grab change and a shutdown promptly, on a device nobody is
/// touching.
fn wait_readable(fd: std::os::unix::io::RawFd) -> bool {
    let mut poll_fd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // Safety: one initialised pollfd, count matching, timeout in ms.
    let ready = unsafe { libc::poll(&mut poll_fd, 1, POLL_TIMEOUT_MS) };
    ready > 0 && poll_fd.revents & libc::POLLIN != 0
}

/// A key or button event, split by which of the two it is.
fn on_key(shared: &Shared, key: Key, value: i32) {
    // 0 up, 1 down, 2 autorepeat.
    let pressed = value != 0;
    let repeat = value == 2;

    if let Some(button) = mouse_button(key) {
        if !repeat {
            shared.emit(LocalEvent::Button { button, pressed });
        }
        return;
    }

    let Some(code) = key_to_hid(key.code()) else {
        tracing::trace!(code = key.code(), "unmapped Linux keycode; not forwarded");
        return;
    };

    // Modifier state is tracked here because Linux does not carry it on the
    // event the way the other two platforms do — there is no flags field, only
    // the individual key transitions, so the running total is ours to keep.
    if let Some(bit) = code.modifier_bit() {
        shared.set_modifier(bit, pressed);
    }
    if key == Key::KEY_CAPSLOCK && pressed && !repeat {
        let was = shared.modifiers().contains(Modifiers::CAPS_LOCK);
        shared.set_modifier(Modifiers::CAPS_LOCK, !was);
    }

    shared.emit(LocalEvent::Key {
        key: code,
        pressed,
        modifiers: shared.modifiers(),
        repeat,
    });
}

fn mouse_button(key: Key) -> Option<MouseButton> {
    Some(match key {
        Key::BTN_LEFT => MouseButton::Left,
        Key::BTN_RIGHT => MouseButton::Right,
        Key::BTN_MIDDLE => MouseButton::Middle,
        Key::BTN_SIDE => MouseButton::Back,
        Key::BTN_EXTRA => MouseButton::Forward,
        _ => return None,
    })
}
