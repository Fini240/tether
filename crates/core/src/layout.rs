//! The virtual canvas.
//!
//! Every monitor of every machine is placed on one shared coordinate plane. A
//! machine contributes its monitors at whatever offsets its own OS reports,
//! shifted by a per-machine `origin` that the user sets by dragging screens
//! around in the arrangement UI.
//!
//! Edge switching is not modelled as "which machine is to the left of which".
//! It falls out of the geometry: move the pointer, look up which monitor now
//! contains it, and if that monitor belongs to a different machine, the cursor
//! has crossed. This is why multi-monitor setups work without special cases —
//! a machine with three displays is just three rectangles.

use serde::{Deserialize, Serialize};
use tether_proto::{MonitorId, MonitorInfo, Platform, Point, Rect};

/// Stable identifier for a machine, derived from its TLS certificate
/// fingerprint so it survives IP changes and reinstalls of the config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MachineId(pub u64);

impl std::fmt::Display for MachineId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

/// One machine's monitors, positioned on the global canvas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Placement {
    pub machine: MachineId,
    pub name: String,
    pub platform: Platform,
    /// Where this machine's local origin lands on the global canvas.
    pub origin: Point,
    /// Monitors in the machine's own local coordinates.
    pub monitors: Vec<MonitorInfo>,
}

impl Placement {
    /// Bounding box of all this machine's monitors, in local coordinates.
    pub fn local_bounds(&self) -> Rect {
        self.monitors
            .iter()
            .fold(Rect::new(0, 0, 0, 0), |acc, m| acc.union(&m.bounds))
    }

    /// Bounding box on the global canvas.
    pub fn global_bounds(&self) -> Rect {
        self.local_bounds().translate(self.origin)
    }

    pub fn global_rect_of(&self, monitor: &MonitorInfo) -> Rect {
        monitor.bounds.translate(self.origin)
    }

    pub fn primary_monitor(&self) -> Option<&MonitorInfo> {
        self.monitors
            .iter()
            .find(|m| m.primary)
            .or_else(|| self.monitors.first())
    }
}

/// A resolved position: which machine, which monitor, and where in that
/// machine's own coordinate space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Located {
    pub machine: MachineId,
    pub monitor: MonitorId,
    pub local: Point,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    pub machines: Vec<Placement>,
}

impl Layout {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, machine: MachineId) -> Option<&Placement> {
        self.machines.iter().find(|p| p.machine == machine)
    }

    pub fn get_mut(&mut self, machine: MachineId) -> Option<&mut Placement> {
        self.machines.iter_mut().find(|p| p.machine == machine)
    }

    pub fn contains(&self, machine: MachineId) -> bool {
        self.get(machine).is_some()
    }

    /// Insert a machine, or update the monitors of one already placed. An
    /// update keeps the existing `origin`: a client reconnecting after a
    /// resolution change must not jump to a different spot on the canvas.
    pub fn upsert(&mut self, placement: Placement) {
        match self.get_mut(placement.machine) {
            Some(existing) => {
                existing.name = placement.name;
                existing.platform = placement.platform;
                existing.monitors = placement.monitors;
            }
            None => self.machines.push(placement),
        }
    }

    pub fn remove(&mut self, machine: MachineId) {
        self.machines.retain(|p| p.machine != machine);
    }

    /// Bounding box of the entire canvas.
    pub fn bounds(&self) -> Rect {
        self.machines
            .iter()
            .fold(Rect::new(0, 0, 0, 0), |acc, p| acc.union(&p.global_bounds()))
    }

    /// Which monitor covers this global point, if any.
    ///
    /// Overlapping placements resolve to the first match in machine order,
    /// which is stable but arbitrary — the arrangement UI should prevent
    /// overlaps rather than relying on this.
    pub fn locate(&self, global: Point) -> Option<Located> {
        for placement in &self.machines {
            for monitor in &placement.monitors {
                if placement.global_rect_of(monitor).contains(global) {
                    return Some(Located {
                        machine: placement.machine,
                        monitor: monitor.id,
                        local: global - placement.origin,
                    });
                }
            }
        }
        None
    }

    /// Global position of a machine's primary monitor centre. Used by the
    /// "jump to machine" hotkey, which has no cursor trajectory to work from.
    pub fn center_of(&self, machine: MachineId) -> Option<Point> {
        let placement = self.get(machine)?;
        let monitor = placement.primary_monitor()?;
        let rect = placement.global_rect_of(monitor);
        Some(Point::new(
            rect.x + rect.width / 2,
            rect.y + rect.height / 2,
        ))
    }

    /// Place a newly discovered machine immediately to the right of everything
    /// already on the canvas, vertically centred against it.
    ///
    /// This is the auto-configuration the setup wizard proposes. It is only a
    /// starting point — a guess about physical desk arrangement is a guess —
    /// but it is right often enough to beat an empty canvas, and dragging one
    /// screen is cheaper than placing all of them.
    pub fn auto_place(&mut self, mut placement: Placement) {
        if self.contains(placement.machine) {
            self.upsert(placement);
            return;
        }

        let local = placement.local_bounds();
        placement.origin = if self.machines.is_empty() {
            // First machine defines the origin, so its local space and the
            // global canvas coincide. Keeps the common single-host case free of
            // pointless offsets.
            Point::new(-local.x, -local.y)
        } else {
            let canvas = self.bounds();
            let y = canvas.y + (canvas.height - local.height) / 2;
            Point::new(canvas.right() - local.x, y - local.y)
        };

        self.machines.push(placement);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(id: u32, x: i32, y: i32, w: i32, h: i32) -> MonitorInfo {
        MonitorInfo {
            id: MonitorId(id),
            name: format!("mon{id}"),
            bounds: Rect::new(x, y, w, h),
            scale: 1.0,
            primary: id == 0,
        }
    }

    fn machine(id: u64, monitors: Vec<MonitorInfo>) -> Placement {
        Placement {
            machine: MachineId(id),
            name: format!("m{id}"),
            platform: Platform::Linux,
            origin: Point::new(0, 0),
            monitors,
        }
    }

    #[test]
    fn auto_place_puts_the_first_machine_at_the_origin() {
        let mut layout = Layout::new();
        layout.auto_place(machine(1, vec![monitor(0, 0, 0, 1920, 1080)]));
        assert_eq!(layout.bounds(), Rect::new(0, 0, 1920, 1080));
    }

    #[test]
    fn auto_place_chains_machines_left_to_right() {
        let mut layout = Layout::new();
        layout.auto_place(machine(1, vec![monitor(0, 0, 0, 1920, 1080)]));
        layout.auto_place(machine(2, vec![monitor(0, 0, 0, 1280, 1024)]));

        let second = layout.get(MachineId(2)).unwrap();
        assert_eq!(second.global_bounds().left(), 1920);
        // Vertically centred against the taller first machine.
        assert_eq!(second.global_bounds().top(), (1080 - 1024) / 2);
    }

    #[test]
    fn auto_place_normalises_a_negative_local_origin() {
        // macOS reports a display left of the primary as a negative x. The
        // first machine still has to land at the canvas origin.
        let mut layout = Layout::new();
        layout.auto_place(machine(
            1,
            vec![monitor(0, 0, 0, 1920, 1080), monitor(1, -1280, 0, 1280, 1024)],
        ));
        assert_eq!(layout.bounds().left(), 0);
        assert_eq!(layout.bounds().width, 1920 + 1280);
    }

    #[test]
    fn locate_maps_a_global_point_back_to_local_coordinates() {
        let mut layout = Layout::new();
        layout.auto_place(machine(1, vec![monitor(0, 0, 0, 1920, 1080)]));
        layout.auto_place(machine(2, vec![monitor(0, 0, 0, 1280, 1024)]));

        let hit = layout.locate(Point::new(1920 + 10, 28 + 5)).unwrap();
        assert_eq!(hit.machine, MachineId(2));
        assert_eq!(hit.local, Point::new(10, 5));
    }

    #[test]
    fn locate_returns_none_in_a_gap_between_screens() {
        let mut layout = Layout::new();
        layout.auto_place(machine(1, vec![monitor(0, 0, 0, 1920, 1080)]));
        layout.auto_place(machine(2, vec![monitor(0, 0, 0, 1280, 200)]));
        // Machine 2 is short and vertically centred, so the canvas has empty
        // bands above and below it.
        assert!(layout.locate(Point::new(2000, 5)).is_none());
    }

    #[test]
    fn upsert_preserves_a_user_positioned_origin() {
        let mut layout = Layout::new();
        layout.auto_place(machine(1, vec![monitor(0, 0, 0, 1920, 1080)]));
        layout.get_mut(MachineId(1)).unwrap().origin = Point::new(500, 500);

        layout.upsert(machine(1, vec![monitor(0, 0, 0, 2560, 1440)]));

        let p = layout.get(MachineId(1)).unwrap();
        assert_eq!(p.origin, Point::new(500, 500));
        assert_eq!(p.local_bounds().width, 2560);
    }
}
