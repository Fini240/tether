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

/// Where to put one machine relative to another on the canvas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Left,
    Right,
    Above,
    Below,
}

impl Side {
    pub fn opposite(self) -> Side {
        match self {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
            Side::Above => Side::Below,
            Side::Below => Side::Above,
        }
    }
}

impl std::str::FromStr for Side {
    type Err = String;

    fn from_str(s: &str) -> Result<Side, String> {
        match s
            .trim()
            .to_ascii_lowercase()
            .replace(['-', '_'], "")
            .as_str()
        {
            "left" | "leftof" | "westof" | "west" => Ok(Side::Left),
            "right" | "rightof" | "eastof" | "east" => Ok(Side::Right),
            "above" | "up" | "top" | "northof" | "north" => Ok(Side::Above),
            "below" | "down" | "bottom" | "under" | "southof" | "south" => Ok(Side::Below),
            other => Err(format!(
                "unknown side {other:?} — use left, right, above or below"
            )),
        }
    }
}

impl std::fmt::Display for Side {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Side::Left => "left of",
            Side::Right => "right of",
            Side::Above => "above",
            Side::Below => "below",
        })
    }
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
        self.machines.iter().fold(Rect::new(0, 0, 0, 0), |acc, p| {
            acc.union(&p.global_bounds())
        })
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

    /// Move `machine` so it sits flush against `anchor` on the given side,
    /// centred on the shared edge.
    ///
    /// Flush, with no gap: a gap would be a band of canvas that belongs to no
    /// monitor, and dragging the pointer into it would stop the cursor dead
    /// instead of crossing.
    pub fn place_relative(
        &mut self,
        machine: MachineId,
        side: Side,
        anchor: MachineId,
    ) -> Result<(), String> {
        if machine == anchor {
            return Err("a machine cannot be placed relative to itself".into());
        }

        let anchor_rect = self
            .get(anchor)
            .ok_or_else(|| format!("machine {anchor} is not on the canvas"))?
            .global_bounds();
        let placement = self
            .get(machine)
            .ok_or_else(|| format!("machine {machine} is not on the canvas"))?;

        let local = placement.local_bounds();
        if local.width == 0 || local.height == 0 {
            return Err(format!("machine {machine} reports no displays"));
        }

        // Target position of the machine's *bounding box*, then back out the
        // origin that puts it there.
        let (x, y) = match side {
            Side::Left => (
                anchor_rect.left() - local.width,
                anchor_rect.top() + (anchor_rect.height - local.height) / 2,
            ),
            Side::Right => (
                anchor_rect.right(),
                anchor_rect.top() + (anchor_rect.height - local.height) / 2,
            ),
            Side::Above => (
                anchor_rect.left() + (anchor_rect.width - local.width) / 2,
                anchor_rect.top() - local.height,
            ),
            Side::Below => (
                anchor_rect.left() + (anchor_rect.width - local.width) / 2,
                anchor_rect.bottom(),
            ),
        };

        let placement = self.get_mut(machine).expect("checked above");
        placement.origin = Point::new(x - local.x, y - local.y);
        Ok(())
    }

    /// Find a machine by name (case-insensitive) or by the start of its id.
    ///
    /// Names are what a person actually types; ids are what survives a rename.
    /// An ambiguous prefix is an error rather than a guess — silently picking
    /// one of two machines would move the wrong screen.
    pub fn resolve(&self, needle: &str) -> Result<MachineId, String> {
        let needle_lower = needle.to_ascii_lowercase();

        let by_name: Vec<_> = self
            .machines
            .iter()
            .filter(|p| p.name.to_ascii_lowercase() == needle_lower)
            .collect();
        if by_name.len() == 1 {
            return Ok(by_name[0].machine);
        }
        if by_name.len() > 1 {
            return Err(format!(
                "{needle:?} matches {} machines; use the id instead",
                by_name.len()
            ));
        }

        let by_id: Vec<_> = self
            .machines
            .iter()
            .filter(|p| p.machine.to_string().starts_with(&needle_lower))
            .collect();
        match by_id.len() {
            1 => Ok(by_id[0].machine),
            0 => Err(format!("no machine matches {needle:?}")),
            n => Err(format!("{needle:?} matches {n} machines; be more specific")),
        }
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
            vec![
                monitor(0, 0, 0, 1920, 1080),
                monitor(1, -1280, 0, 1280, 1024),
            ],
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
    fn place_relative_puts_a_machine_on_the_left() {
        let mut layout = Layout::new();
        layout.auto_place(machine(1, vec![monitor(0, 0, 0, 1920, 1080)]));
        layout.auto_place(machine(2, vec![monitor(0, 0, 0, 1280, 1024)]));

        layout
            .place_relative(MachineId(2), Side::Left, MachineId(1))
            .unwrap();

        let two = layout.get(MachineId(2)).unwrap().global_bounds();
        let one = layout.get(MachineId(1)).unwrap().global_bounds();
        assert_eq!(
            two.right(),
            one.left(),
            "must be flush, or the cursor sticks"
        );
        // Vertically centred against the anchor.
        assert_eq!(two.top(), one.top() + (one.height - two.height) / 2);
    }

    #[test]
    fn place_relative_survives_a_round_trip() {
        let mut layout = Layout::new();
        layout.auto_place(machine(1, vec![monitor(0, 0, 0, 1920, 1080)]));
        layout.auto_place(machine(2, vec![monitor(0, 0, 0, 1280, 1024)]));

        let start = layout.get(MachineId(2)).unwrap().origin;
        layout
            .place_relative(MachineId(2), Side::Left, MachineId(1))
            .unwrap();
        layout
            .place_relative(MachineId(2), Side::Right, MachineId(1))
            .unwrap();
        assert_eq!(layout.get(MachineId(2)).unwrap().origin, start);
    }

    #[test]
    fn place_relative_stacks_vertically() {
        let mut layout = Layout::new();
        layout.auto_place(machine(1, vec![monitor(0, 0, 0, 1920, 1080)]));
        layout.auto_place(machine(2, vec![monitor(0, 0, 0, 1280, 1024)]));

        layout
            .place_relative(MachineId(2), Side::Above, MachineId(1))
            .unwrap();
        let two = layout.get(MachineId(2)).unwrap().global_bounds();
        let one = layout.get(MachineId(1)).unwrap().global_bounds();
        assert_eq!(two.bottom(), one.top());
        assert_eq!(two.left(), one.left() + (one.width - two.width) / 2);
    }

    #[test]
    fn a_machine_placed_left_is_reachable_by_moving_left() {
        // The point of the whole feature: after placing it left, walking the
        // cursor left must actually land there.
        use crate::transition::{CursorRouter, Transition};

        let mut layout = Layout::new();
        layout.auto_place(machine(1, vec![monitor(0, 0, 0, 1920, 1080)]));
        layout.auto_place(machine(2, vec![monitor(0, 0, 0, 1280, 1024)]));
        layout
            .place_relative(MachineId(2), Side::Left, MachineId(1))
            .unwrap();

        let mut router = CursorRouter::new(layout, MachineId(1));
        match router.move_by(-1000, 0) {
            Transition::Switch { to, .. } => assert_eq!(to.machine, MachineId(2)),
            other => panic!("expected to cross left, got {other:?}"),
        }
    }

    #[test]
    fn place_relative_rejects_nonsense() {
        let mut layout = Layout::new();
        layout.auto_place(machine(1, vec![monitor(0, 0, 0, 1920, 1080)]));
        assert!(layout
            .place_relative(MachineId(1), Side::Left, MachineId(1))
            .is_err());
        assert!(layout
            .place_relative(MachineId(1), Side::Left, MachineId(9))
            .is_err());
    }

    #[test]
    fn resolve_matches_names_and_id_prefixes() {
        let mut layout = Layout::new();
        layout.auto_place(machine(0xABCDEF, vec![monitor(0, 0, 0, 800, 600)]));
        assert_eq!(layout.resolve("m11259375").unwrap(), MachineId(0xABCDEF));
        assert!(
            layout.resolve("M11259375").is_ok(),
            "names are case-insensitive"
        );
        assert!(layout.resolve("0000000000abcdef").is_ok(), "id prefix");
        assert!(layout.resolve("nope").is_err());
    }

    #[test]
    fn sides_parse_from_the_words_people_type() {
        use std::str::FromStr;
        assert_eq!(Side::from_str("left").unwrap(), Side::Left);
        assert_eq!(Side::from_str("left-of").unwrap(), Side::Left);
        assert_eq!(Side::from_str("ABOVE").unwrap(), Side::Above);
        assert!(Side::from_str("sideways").is_err());
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
