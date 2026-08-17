//! A backend with no operating system behind it.
//!
//! Two jobs:
//!
//! 1. Let the routing, protocol and reconnection logic be exercised without a
//!    second physical computer — run a host and a client on one machine, both
//!    headless, and watch the frames flow.
//! 2. Give CI something to run.
//!
//! Injected events are recorded rather than delivered, so a test can assert on
//! exactly what a client would have received.

use std::sync::{Arc, Mutex};

use tether_proto::{
    ClipboardContents, ClipboardStamp, InputEvent, MonitorId, MonitorInfo, Point, Rect,
};
use tokio::sync::mpsc::UnboundedSender;

use crate::traits::{
    ClipboardAccess, InputCapture, InputInject, LocalEvent, Monitors, PlatformError, Pointer,
    Result, ScreenLock,
};
use crate::{Backend, BackendKind};

/// Build a full headless backend with a default 1920x1080 screen.
pub fn backend() -> Backend {
    backend_with(vec![fake_monitor(0, 0, 0, 1920, 1080, true)]).0
}

/// Build a headless backend over the given monitors, returning a handle that
/// can feed synthetic input and inspect what was injected.
pub fn backend_with(monitors: Vec<MonitorInfo>) -> (Backend, HeadlessHandle) {
    let handle = HeadlessHandle::default();
    let backend = Backend {
        kind: BackendKind::Headless,
        capture: Box::new(HeadlessCapture {
            handle: handle.clone(),
        }),
        inject: Box::new(HeadlessInject {
            handle: handle.clone(),
        }),
        pointer: Box::new(HeadlessPointer {
            handle: handle.clone(),
        }),
        monitors: Box::new(HeadlessMonitors { monitors }),
        clipboard: Box::new(HeadlessClipboard::default()),
        lock: Box::new(HeadlessLock {
            handle: handle.clone(),
        }),
    };
    (backend, handle)
}

pub fn fake_monitor(id: u32, x: i32, y: i32, w: i32, h: i32, primary: bool) -> MonitorInfo {
    MonitorInfo {
        id: MonitorId(id),
        name: format!("Headless {id}"),
        bounds: Rect::new(x, y, w, h),
        scale: 1.0,
        primary,
    }
}

struct Shared {
    sink: Option<UnboundedSender<LocalEvent>>,
    swallowing: bool,
    injected: Vec<InputEvent>,
    cursor: Point,
    cursor_visible: bool,
    locks: u32,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            sink: None,
            swallowing: false,
            injected: Vec::new(),
            cursor: Point::new(0, 0),
            cursor_visible: true,
            locks: 0,
        }
    }
}

/// Drives and observes a headless backend.
#[derive(Clone, Default)]
pub struct HeadlessHandle {
    shared: Arc<Mutex<Shared>>,
}

impl HeadlessHandle {
    /// Feed a synthetic event as though the user had produced it.
    /// Returns false if capture has not been started.
    pub fn emit(&self, event: LocalEvent) -> bool {
        let shared = self.shared.lock().expect("headless mutex poisoned");
        match &shared.sink {
            Some(sink) => sink.send(event).is_ok(),
            None => false,
        }
    }

    /// Every event injected into this backend, oldest first.
    pub fn injected(&self) -> Vec<InputEvent> {
        self.shared
            .lock()
            .expect("headless mutex poisoned")
            .injected
            .clone()
    }

    pub fn clear_injected(&self) {
        self.shared
            .lock()
            .expect("headless mutex poisoned")
            .injected
            .clear();
    }

    pub fn is_swallowing(&self) -> bool {
        self.shared
            .lock()
            .expect("headless mutex poisoned")
            .swallowing
    }

    pub fn cursor(&self) -> Point {
        self.shared.lock().expect("headless mutex poisoned").cursor
    }

    pub fn cursor_visible(&self) -> bool {
        self.shared
            .lock()
            .expect("headless mutex poisoned")
            .cursor_visible
    }

    /// How many times the screen lock has been requested.
    pub fn lock_count(&self) -> u32 {
        self.shared.lock().expect("headless mutex poisoned").locks
    }
}

struct HeadlessCapture {
    handle: HeadlessHandle,
}

impl InputCapture for HeadlessCapture {
    fn start(&mut self, sink: UnboundedSender<LocalEvent>) -> Result<()> {
        self.handle
            .shared
            .lock()
            .expect("headless mutex poisoned")
            .sink = Some(sink);
        Ok(())
    }

    fn stop(&mut self) {
        self.handle
            .shared
            .lock()
            .expect("headless mutex poisoned")
            .sink = None;
    }

    fn set_swallow(&self, swallow: bool) {
        self.handle
            .shared
            .lock()
            .expect("headless mutex poisoned")
            .swallowing = swallow;
    }
}

struct HeadlessInject {
    handle: HeadlessHandle,
}

impl InputInject for HeadlessInject {
    fn inject(&self, event: &InputEvent) -> Result<()> {
        tracing::debug!(?event, "headless inject");
        let mut shared = self.handle.shared.lock().expect("headless mutex poisoned");
        if let InputEvent::MouseMove { x, y } = event {
            shared.cursor = Point::new(*x, *y);
        }
        shared.injected.push(event.clone());
        Ok(())
    }

    fn release_all(&self) -> Result<()> {
        tracing::debug!("headless release_all");
        Ok(())
    }
}

struct HeadlessPointer {
    handle: HeadlessHandle,
}

impl Pointer for HeadlessPointer {
    fn position(&self) -> Result<Point> {
        Ok(self.handle.cursor())
    }

    fn warp(&self, to: Point) -> Result<()> {
        self.handle
            .shared
            .lock()
            .expect("headless mutex poisoned")
            .cursor = to;
        Ok(())
    }

    fn set_visible(&self, visible: bool) -> Result<()> {
        self.handle
            .shared
            .lock()
            .expect("headless mutex poisoned")
            .cursor_visible = visible;
        Ok(())
    }
}

struct HeadlessMonitors {
    monitors: Vec<MonitorInfo>,
}

impl Monitors for HeadlessMonitors {
    fn enumerate(&self) -> Result<Vec<MonitorInfo>> {
        if self.monitors.is_empty() {
            return Err(PlatformError::backend("headless backend has no monitors"));
        }
        Ok(self.monitors.clone())
    }
}

#[derive(Default)]
struct HeadlessClipboard {
    contents: Option<ClipboardContents>,
    /// What `poll_change` last reported, so it only fires on a real change.
    reported: Option<ClipboardContents>,
}

impl ClipboardAccess for HeadlessClipboard {
    fn read(&mut self) -> Result<ClipboardContents> {
        Ok(self
            .contents
            .clone()
            .unwrap_or_else(|| ClipboardContents::empty(ClipboardStamp { owner: 0, seq: 0 })))
    }

    fn write(&mut self, contents: &ClipboardContents) -> Result<()> {
        self.contents = Some(contents.clone());
        self.reported = Some(contents.clone());
        Ok(())
    }

    fn poll_change(&mut self) -> Result<Option<ClipboardContents>> {
        let current = self.read()?;
        if current.is_empty() || self.reported.as_ref() == Some(&current) {
            return Ok(None);
        }
        self.reported = Some(current.clone());
        Ok(Some(current))
    }
}

struct HeadlessLock {
    handle: HeadlessHandle,
}

impl ScreenLock for HeadlessLock {
    fn lock(&self) -> Result<()> {
        tracing::info!("headless screen lock");
        self.handle
            .shared
            .lock()
            .expect("headless mutex poisoned")
            .locks += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injected_events_are_recorded() {
        let (backend, handle) = backend_with(vec![fake_monitor(0, 0, 0, 800, 600, true)]);
        backend
            .inject
            .inject(&InputEvent::MouseMove { x: 10, y: 20 })
            .unwrap();
        assert_eq!(
            handle.injected(),
            vec![InputEvent::MouseMove { x: 10, y: 20 }]
        );
        assert_eq!(handle.cursor(), Point::new(10, 20));
    }

    #[test]
    fn emit_reaches_a_started_capture() {
        let (mut backend, handle) = backend_with(vec![fake_monitor(0, 0, 0, 800, 600, true)]);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        assert!(!handle.emit(LocalEvent::MouseDelta { dx: 1, dy: 1 }));
        backend.capture.start(tx).unwrap();
        assert!(handle.emit(LocalEvent::MouseDelta { dx: 1, dy: 1 }));

        assert_eq!(
            rx.try_recv().unwrap(),
            LocalEvent::MouseDelta { dx: 1, dy: 1 }
        );
    }

    #[test]
    fn clipboard_round_trips() {
        let (mut backend, _) = backend_with(vec![fake_monitor(0, 0, 0, 800, 600, true)]);
        let mut contents = ClipboardContents::empty(ClipboardStamp { owner: 1, seq: 1 });
        contents.text = Some("hello".into());
        backend.clipboard.write(&contents).unwrap();
        assert_eq!(
            backend.clipboard.read().unwrap().text.as_deref(),
            Some("hello")
        );
    }
}
