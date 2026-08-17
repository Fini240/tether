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

/// How much of a device movement the operating system declined to apply.
///
/// Zero while the pointer is free: the OS moved it by exactly what the device
/// reported. At the outer edge of a desktop the OS clamps and the remainder is
/// the user still pushing, which is what has to cross to the next machine.
///
/// Clamped to the sign and size of the movement itself, because the OS also
/// moves the pointer in ways the device never asked for — the sideways slide
/// crossing between misaligned screens is motion it *invented*, and read
/// naively that comes out as a large push in the opposite direction.
fn unapplied(delta: i32, applied: i32) -> i32 {
    if delta == 0 {
        return 0;
    }
    let left = delta - applied;
    if left.signum() != delta.signum() {
        return 0;
    }
    left.clamp(-delta.abs(), delta.abs())
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

    /// Adopt the operating system's idea of where the pointer is.
    ///
    /// While the machine holding the pointer is also the one being touched, we
    /// do not inject anything there — its own OS moves the cursor and this
    /// router only follows along by adding up deltas. Those two can disagree:
    /// pointer acceleration, and above all a multi-monitor desktop whose
    /// screens are not aligned, where the OS slides the pointer sideways as it
    /// crosses a seam. Once they disagree the router believes the pointer is
    /// somewhere it is not, and refuses to cross an edge the pointer is
    /// visibly sitting on.
    ///
    /// Called with the OS position before each move, so the two can never
    /// drift further apart than a single event.
    pub fn resync_from_local(&mut self, local: Point) {
        let host = self.host;
        self.resync_on(host, local);
    }

    /// Adopt a position reported by `machine`, in that machine's own space.
    ///
    /// Only meaningful from the machine whose own cursor is moving — that is,
    /// the one being physically touched while it also holds the pointer.
    /// Anywhere else the cursor is either pinned or under our control, and its
    /// position says nothing.
    ///
    /// Adopts the position and nothing else. It deliberately does *not* move
    /// ownership to `machine`: a position report is a correction, not a
    /// routing decision, and letting one hand the pointer back would undo a
    /// crossing. Events captured in the moment before the host started
    /// suppressing are still in flight when the crossing is applied, and every
    /// one of them carries a position on the host — enough, when it reassigned
    /// ownership, to drag the cursor back off the client and take the keyboard
    /// with it.
    pub fn resync_on(&mut self, machine: MachineId, local: Point) {
        if let Some(placement) = self.layout.get(machine) {
            let global = local + placement.origin;
            // Ignore a position outside that machine's own screens: it would
            // mean the report and the layout disagree, and trusting it would
            // teleport the pointer somewhere nobody asked for.
            if placement
                .monitors
                .iter()
                .any(|m| placement.global_rect_of(m).contains(global))
            {
                self.position = global;
            }
        }
    }

    /// Feed one movement as the machine holding the pointer reported it: where
    /// its own OS has since put the cursor, and how far the physical device
    /// actually moved.
    ///
    /// The two are different measurements and both are needed. The position is
    /// the truth while the pointer is loose on that desktop — it includes the
    /// sideways slide the OS performs crossing between screens of different
    /// heights, which no amount of adding up device movement predicts. But the
    /// position stops at the outer edge of that desktop, and pushing past that
    /// edge is exactly how the cursor reaches the next machine. What the OS
    /// refused to apply is the push that crosses.
    ///
    /// Adding both — adopting the position *and* then applying the whole delta
    /// on top — moves the cursor twice per event. The router then runs one
    /// event's worth of motion ahead of the pointer the user can see and hands
    /// over while that pointer is still short of the edge, which reads as the
    /// cursor sliding into the edge on its own after control has already gone.
    pub fn move_reported(&mut self, machine: MachineId, at: Point, dx: i32, dy: i32) -> Transition {
        if self.active != machine {
            // The pointer is not on the reporting machine, so its cursor is
            // either pinned or under our control and its position says
            // nothing. The device movement is still real.
            return self.move_by(dx, dy);
        }
        let before = self.position;
        self.resync_on(machine, at);
        let applied = self.position - before;
        self.move_by(unapplied(dx, applied.x), unapplied(dy, applied.y))
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

#[cfg(test)]
mod resync_tests {
    use super::*;
    use crate::layout::Placement;
    use tether_proto::{MonitorId, MonitorInfo, Platform, Rect};

    /// A desktop whose two screens sit at different heights, with a machine
    /// below the shorter one — the arrangement that exposed the drift.
    fn ragged() -> CursorRouter {
        let monitors = |rects: &[(i32, i32, i32, i32)]| -> Vec<MonitorInfo> {
            rects
                .iter()
                .enumerate()
                .map(|(i, (x, y, w, h))| MonitorInfo {
                    id: MonitorId(i as u32),
                    name: format!("m{i}"),
                    bounds: Rect::new(*x, *y, *w, *h),
                    scale: 1.0,
                    primary: i == 0,
                })
                .collect()
        };

        let mut layout = Layout::new();
        layout.machines.push(Placement {
            machine: MachineId(1),
            name: "pc".into(),
            platform: Platform::Windows,
            origin: Point::new(0, 0),
            monitors: monitors(&[(0, 200, 1920, 1080), (1920, 0, 2560, 1440)]),
        });
        layout.machines.push(Placement {
            machine: MachineId(2),
            name: "mac".into(),
            platform: Platform::MacOS,
            origin: Point::new(0, 1280),
            monitors: monitors(&[(0, 0, 1710, 1112)]),
        });
        CursorRouter::new(layout, MachineId(1))
    }

    #[test]
    fn a_reported_position_outside_that_machine_is_ignored() {
        // A machine reporting a position its own screens do not contain means
        // the report and the layout disagree. Believing it would teleport the
        // pointer somewhere nobody asked for; the previous position is a far
        // better guess than a contradictory one.
        let mut router = ragged();
        router.resync_from_local(Point::new(800, 1270));
        router.resync_on(MachineId(1), Point::new(900, 9999));
        assert_eq!(router.position(), Point::new(800, 1270));
    }

    #[test]
    fn resync_adopts_the_systems_position() {
        let mut router = ragged();
        router.resync_from_local(Point::new(2500, 700));
        assert_eq!(router.position(), Point::new(2500, 700));
    }

    #[test]
    fn resync_recovers_after_the_os_moves_the_pointer_itself() {
        // Crossing between two screens of different heights, the OS slides the
        // pointer to a row the next screen actually has. The router did not
        // see that happen; without adopting the new position it goes on
        // believing the pointer is somewhere with no screen under it, and
        // refuses to cross an edge the pointer is visibly sitting on.
        let mut router = ragged();

        // The router last saw the pointer high on the right-hand screen.
        router.resync_from_local(Point::new(2600, 300));
        // The OS has since carried it onto the left-hand screen, near the
        // bottom — a move this router never added up.
        router.resync_from_local(Point::new(800, 1270));
        assert_eq!(router.position(), Point::new(800, 1270));
        assert_eq!(router.active(), MachineId(1));

        // Pushing down from where the pointer really is reaches the machine
        // below. Working from the stale position it would instead be pushing
        // off the bottom of the right-hand screen into empty canvas.
        match router.move_by(0, 40) {
            Transition::Switch { to, .. } => assert_eq!(to.machine, MachineId(2)),
            other => panic!("did not cross after the OS moved the pointer: {other:?}"),
        }
    }

    /// Two 1920x1080 machines side by side, host on the left — the same
    /// fixture as the routing tests, rebuilt here for the reported-move ones.
    fn side_by_side() -> CursorRouter {
        use crate::layout::Layout;
        let placement = |id: u64| Placement {
            machine: MachineId(id),
            name: format!("m{id}"),
            platform: Platform::Windows,
            origin: Point::new(0, 0),
            monitors: vec![MonitorInfo {
                id: MonitorId(0),
                name: "mon".into(),
                bounds: Rect::new(0, 0, 1920, 1080),
                scale: 1.0,
                primary: true,
            }],
        };
        let mut layout = Layout::new();
        layout.auto_place(placement(1));
        layout.auto_place(placement(2));
        CursorRouter::new(layout, MachineId(1))
    }

    #[test]
    fn a_move_the_os_already_applied_is_not_applied_again() {
        // The pointer moved 40px and the OS put it there. The router must land
        // on the same pixel, not 40 further on. Running ahead is what handed
        // control over while the visible cursor was still short of the edge,
        // leaving it to slide into the edge on its own afterwards.
        let mut router = side_by_side();
        let transition = router.move_reported(MachineId(1), Point::new(1000, 540), 40, 0);
        assert!(matches!(transition, Transition::Stay(_)));
        assert_eq!(router.position(), Point::new(1000, 540));
    }

    #[test]
    fn pushing_past_a_clamped_edge_is_what_crosses() {
        let mut router = side_by_side();
        // Walk to the last column the host's desktop has.
        router.move_reported(MachineId(1), Point::new(1919, 540), 959, 0);
        assert_eq!(router.position(), Point::new(1919, 540));
        assert_eq!(router.active(), MachineId(1));

        // Still pushing right. The OS has nowhere left to put the pointer, so
        // it reports the same position; the device movement is the only
        // evidence the user is still asking to go that way.
        match router.move_reported(MachineId(1), Point::new(1919, 540), 40, 0) {
            Transition::Switch { to, .. } => {
                assert_eq!(to.machine, MachineId(2));
                assert_eq!(to.local, Point::new(39, 540));
            }
            other => panic!("expected a crossing, got {other:?}"),
        }
    }

    #[test]
    fn a_slide_between_misaligned_screens_is_not_read_as_a_push() {
        // Crossing the host's own seam, the OS drops the pointer 100px to a
        // row the next screen actually has. That is motion the OS invented,
        // not motion it refused — treating the difference as leftover push
        // would fling the cursor back up and out of the desktop.
        let mut router = ragged();
        router.move_reported(MachineId(1), Point::new(1930, 100), 0, 0);

        let transition = router.move_reported(MachineId(1), Point::new(1900, 200), -20, 0);
        assert!(matches!(transition, Transition::Stay(_)), "{transition:?}");
        assert_eq!(router.position(), Point::new(1900, 200));
        assert_eq!(router.active(), MachineId(1));
    }

    #[test]
    fn a_report_from_the_host_cannot_take_the_pointer_off_a_client() {
        // Events captured in the instant before suppression started are still
        // in flight when the crossing lands, and each carries a position on
        // the host. Believing one hands the pointer back, and the keyboard
        // goes with it — the host then delivers keystrokes to itself while the
        // user watches an idle cursor on the client.
        let mut router = side_by_side();
        router.move_reported(MachineId(1), Point::new(1919, 540), 959, 0);
        router.move_reported(MachineId(1), Point::new(1919, 540), 40, 0);
        assert_eq!(router.active(), MachineId(2));

        router.move_reported(MachineId(1), Point::new(1919, 540), 5, 0);
        assert_eq!(router.active(), MachineId(2));
        assert_eq!(router.position(), Point::new(1964, 540));

        // Even applied directly, a resync is a correction and never a handover.
        router.resync_on(MachineId(1), Point::new(1919, 540));
        assert_eq!(router.active(), MachineId(2));
    }
}
