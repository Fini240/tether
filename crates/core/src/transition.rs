//! The cursor router: turns raw pointer deltas into "who owns the cursor now".
//!
//! The host keeps one authoritative global cursor position. Every mouse delta
//! it captures moves that position on the canvas; where it lands decides which
//! machine receives the event. Clients never reason about position at all —
//! they are told absolute local coordinates and obey.
//!
//! Deltas rather than the OS's reported absolute position: once the cursor is
//! on a remote machine the host's own pointer is pinned in place, so its
//! absolute position stops being meaningful. Deltas keep working.

use tether_proto::{Point, Rect};

use crate::layout::{Layout, Located, MachineId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    /// Cursor moved within the machine that already owned it.
    Stay(Located),
    /// Cursor crossed onto a different machine.
    Switch { from: MachineId, to: Located },
    /// Movement was refused: the canvas edge, a gap between screens, or an
    /// active cursor lock. The cursor stays where it was.
    Blocked,
}

pub struct CursorRouter {
    layout: Layout,
    /// The machine with the physical keyboard and mouse.
    host: MachineId,
    /// Authoritative cursor position on the global canvas.
    position: Point,
    /// Machine currently receiving input.
    active: MachineId,
    /// While set, the cursor cannot leave `active`.
    locked: bool,
}

impl CursorRouter {
    /// Starts with the cursor on the host, at its primary monitor's centre.
    pub fn new(layout: Layout, host: MachineId) -> Self {
        let position = layout.center_of(host).unwrap_or(Point::new(0, 0));
        Self {
            layout,
            host,
            position,
            active: host,
            locked: false,
        }
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    pub fn host(&self) -> MachineId {
        self.host
    }

    pub fn active(&self) -> MachineId {
        self.active
    }

    pub fn position(&self) -> Point {
        self.position
    }

    pub fn is_locked(&self) -> bool {
        self.locked
    }

    pub fn set_locked(&mut self, locked: bool) {
        self.locked = locked;
    }

    pub fn toggle_lock(&mut self) -> bool {
        self.locked = !self.locked;
        self.locked
    }

    /// Replace the layout, e.g. after a client's monitors change.
    ///
    /// If the cursor's current position is no longer covered by any monitor it
    /// is recalled to the host, because the alternative is a pointer stranded
    /// on a screen that no longer exists.
    pub fn set_layout(&mut self, layout: Layout) {
        self.layout = layout;
        if self.layout.locate(self.position).is_none() {
            self.recall_to_host();
        }
    }

    /// Force the cursor back to the host's primary monitor.
    pub fn recall_to_host(&mut self) -> Option<Located> {
        let center = self.layout.center_of(self.host)?;
        self.position = center;
        self.active = self.host;
        self.layout.locate(center)
    }

    /// Warp straight to a machine's primary monitor centre — the "jump to this
    /// screen" hotkey. Ignores the cursor lock: an explicit hotkey is an
    /// explicit intent, and being unable to escape a lock you forgot you set is
    /// worse than the lock leaking.
    pub fn jump_to(&mut self, machine: MachineId) -> Transition {
        let Some(center) = self.layout.center_of(machine) else {
            return Transition::Blocked;
        };
        let Some(located) = self.layout.locate(center) else {
            return Transition::Blocked;
        };
        let from = self.active;
        self.position = center;
        self.active = located.machine;
        if from == located.machine {
            Transition::Stay(located)
        } else {
            Transition::Switch { from, to: located }
        }
    }

    /// Feed one pointer delta and find out where the cursor ended up.
    pub fn move_by(&mut self, dx: i32, dy: i32) -> Transition {
        let target = Point::new(self.position.x + dx, self.position.y + dy);

        // Fast path: still inside the machine that already owns the cursor.
        if let Some(located) = self.layout.locate(target) {
            if located.machine == self.active {
                self.position = target;
                return Transition::Stay(located);
            }
            if self.locked {
                return self.slide_within_active(dx, dy);
            }
            let from = self.active;
            self.position = target;
            self.active = located.machine;
            return Transition::Switch { from, to: located };
        }

        // The full delta landed in a gap or off the canvas. Try each axis
        // alone before giving up: dragging diagonally along a screen edge
        // should glide, not stick.
        self.slide(dx, dy)
    }

    /// Retry the move one axis at a time, then clamp.
    fn slide(&mut self, dx: i32, dy: i32) -> Transition {
        for (adx, ady) in [(dx, 0), (0, dy)] {
            if adx == 0 && ady == 0 {
                continue;
            }
            let candidate = Point::new(self.position.x + adx, self.position.y + ady);
            let Some(located) = self.layout.locate(candidate) else {
                continue;
            };
            if located.machine != self.active {
                if self.locked {
                    continue;
                }
                let from = self.active;
                self.position = candidate;
                self.active = located.machine;
                return Transition::Switch { from, to: located };
            }
            self.position = candidate;
            return Transition::Stay(located);
        }
        Transition::Blocked
    }

    /// Same as `slide`, but never leaves the active machine. Used when the
    /// cursor is locked and the requested move would have crossed.
    fn slide_within_active(&mut self, dx: i32, dy: i32) -> Transition {
        for (adx, ady) in [(dx, 0), (0, dy)] {
            if adx == 0 && ady == 0 {
                continue;
            }
            let candidate = Point::new(self.position.x + adx, self.position.y + ady);
            if let Some(located) = self.layout.locate(candidate) {
                if located.machine == self.active {
                    self.position = candidate;
                    return Transition::Stay(located);
                }
            }
        }
        Transition::Blocked
    }

    /// Global bounds of the machine currently holding the cursor.
    pub fn active_bounds(&self) -> Option<Rect> {
        self.layout.get(self.active).map(|p| p.global_bounds())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Placement;
    use tether_proto::{MonitorId, MonitorInfo, Platform, Rect};

    fn mon(id: u32, x: i32, y: i32, w: i32, h: i32) -> MonitorInfo {
        MonitorInfo {
            id: MonitorId(id),
            name: format!("mon{id}"),
            bounds: Rect::new(x, y, w, h),
            scale: 1.0,
            primary: id == 0,
        }
    }

    fn placement(id: u64, monitors: Vec<MonitorInfo>) -> Placement {
        Placement {
            machine: MachineId(id),
            name: format!("m{id}"),
            platform: Platform::Linux,
            origin: Point::new(0, 0),
            monitors,
        }
    }

    /// Two 1920x1080 machines side by side, host on the left.
    fn pair() -> CursorRouter {
        let mut layout = Layout::new();
        layout.auto_place(placement(1, vec![mon(0, 0, 0, 1920, 1080)]));
        layout.auto_place(placement(2, vec![mon(0, 0, 0, 1920, 1080)]));
        CursorRouter::new(layout, MachineId(1))
    }

    #[test]
    fn starts_centred_on_the_host() {
        let r = pair();
        assert_eq!(r.active(), MachineId(1));
        assert_eq!(r.position(), Point::new(960, 540));
    }

    #[test]
    fn moving_inside_the_host_stays_put() {
        let mut r = pair();
        assert!(matches!(r.move_by(10, 10), Transition::Stay(_)));
        assert_eq!(r.active(), MachineId(1));
    }

    #[test]
    fn crossing_the_right_edge_switches_machines() {
        let mut r = pair();
        match r.move_by(1000, 0) {
            Transition::Switch { from, to } => {
                assert_eq!(from, MachineId(1));
                assert_eq!(to.machine, MachineId(2));
                // 960 + 1000 = 1960 global -> 40 into machine 2.
                assert_eq!(to.local, Point::new(40, 540));
            }
            other => panic!("expected a switch, got {other:?}"),
        }
    }

    #[test]
    fn crossing_back_returns_to_the_host() {
        let mut r = pair();
        r.move_by(1000, 0);
        assert_eq!(r.active(), MachineId(2));
        match r.move_by(-100, 0) {
            Transition::Switch { to, .. } => assert_eq!(to.machine, MachineId(1)),
            other => panic!("expected a switch back, got {other:?}"),
        }
    }

    #[test]
    fn the_far_edge_of_the_canvas_blocks() {
        let mut r = pair();
        assert_eq!(r.move_by(-2000, 0), Transition::Blocked);
        assert_eq!(r.position(), Point::new(960, 540));
    }

    #[test]
    fn a_diagonal_move_at_the_edge_slides_instead_of_sticking() {
        let mut r = pair();
        r.move_by(0, 500); // y = 1040, near the bottom
        // Down-right past the bottom edge: y is refused, x still applies.
        match r.move_by(50, 100) {
            Transition::Stay(l) => assert_eq!(l.local, Point::new(1010, 1040)),
            other => panic!("expected a slide, got {other:?}"),
        }
    }

    #[test]
    fn a_lock_prevents_crossing_but_not_moving() {
        let mut r = pair();
        r.set_locked(true);
        assert_eq!(r.move_by(1000, 0), Transition::Blocked);
        assert_eq!(r.active(), MachineId(1));
        assert!(matches!(r.move_by(-100, 0), Transition::Stay(_)));
    }

    #[test]
    fn a_lock_still_slides_along_the_blocked_edge() {
        let mut r = pair();
        r.set_locked(true);
        // Would cross to the right; the vertical component must still apply.
        match r.move_by(1000, -100) {
            Transition::Stay(l) => assert_eq!(l.local, Point::new(960, 440)),
            other => panic!("expected a slide, got {other:?}"),
        }
    }

    #[test]
    fn jump_to_overrides_the_lock() {
        let mut r = pair();
        r.set_locked(true);
        match r.jump_to(MachineId(2)) {
            Transition::Switch { to, .. } => assert_eq!(to.machine, MachineId(2)),
            other => panic!("expected a jump, got {other:?}"),
        }
    }

    #[test]
    fn jump_to_an_unknown_machine_is_blocked() {
        let mut r = pair();
        assert_eq!(r.jump_to(MachineId(99)), Transition::Blocked);
        assert_eq!(r.active(), MachineId(1));
    }

    #[test]
    fn a_multi_monitor_host_crosses_only_at_its_outer_edge() {
        let mut layout = Layout::new();
        // Host has two stacked-side-by-side displays: 0..1920 and 1920..3840.
        layout.auto_place(placement(
            1,
            vec![mon(0, 0, 0, 1920, 1080), mon(1, 1920, 0, 1920, 1080)],
        ));
        layout.auto_place(placement(2, vec![mon(0, 0, 0, 1920, 1080)]));
        let mut r = CursorRouter::new(layout, MachineId(1));

        // Centre of the host's primary, moving right onto its *second* display.
        match r.move_by(1500, 0) {
            Transition::Stay(l) => {
                assert_eq!(l.machine, MachineId(1));
                assert_eq!(l.monitor, MonitorId(1));
            }
            other => panic!("expected to stay on the host, got {other:?}"),
        }
        // Only now does another 1500 leave the host entirely.
        match r.move_by(1500, 0) {
            Transition::Switch { to, .. } => assert_eq!(to.machine, MachineId(2)),
            other => panic!("expected a switch, got {other:?}"),
        }
    }

    #[test]
    fn a_vanishing_client_recalls_the_cursor_to_the_host() {
        let mut r = pair();
        r.move_by(1000, 0);
        assert_eq!(r.active(), MachineId(2));

        let mut layout = r.layout().clone();
        layout.remove(MachineId(2));
        r.set_layout(layout);

        assert_eq!(r.active(), MachineId(1));
        assert_eq!(r.position(), Point::new(960, 540));
    }
}
