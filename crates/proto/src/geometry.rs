//! Screen geometry types.
//!
//! Two coordinate spaces exist and confusing them is the single easiest way to
//! break edge switching, so the names are explicit everywhere:
//!
//! * **local** — a machine's own desktop space, exactly as its OS reports it.
//!   Origin is wherever that OS puts it (top-left of the primary display on
//!   Windows/X11; bottom-left on macOS before we normalise it).
//! * **global** — the virtual canvas the user arranges in the UI, spanning
//!   every monitor of every machine. Only the host thinks in this space.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MonitorId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

impl std::ops::Add for Point {
    type Output = Point;
    fn add(self, rhs: Point) -> Point {
        Point::new(self.x + rhs.x, self.y + rhs.y)
    }
}

impl std::ops::Sub for Point {
    type Output = Point;
    fn sub(self, rhs: Point) -> Point {
        Point::new(self.x - rhs.x, self.y - rhs.y)
    }
}

/// Half-open rectangle: `x .. x + width`, `y .. y + height`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub const fn left(&self) -> i32 {
        self.x
    }
    pub const fn top(&self) -> i32 {
        self.y
    }
    pub const fn right(&self) -> i32 {
        self.x + self.width
    }
    pub const fn bottom(&self) -> i32 {
        self.y + self.height
    }

    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.left() && p.x < self.right() && p.y >= self.top() && p.y < self.bottom()
    }

    /// Nearest point inside the rectangle. Used to pin the cursor when it walks
    /// into a gap in the layout that no monitor covers.
    pub fn clamp(&self, p: Point) -> Point {
        Point::new(
            p.x.clamp(self.left(), self.right() - 1),
            p.y.clamp(self.top(), self.bottom() - 1),
        )
    }

    pub fn translate(&self, by: Point) -> Rect {
        Rect::new(self.x + by.x, self.y + by.y, self.width, self.height)
    }

    /// Smallest rectangle covering both. Returns `other` if `self` is empty.
    pub fn union(&self, other: &Rect) -> Rect {
        if self.width == 0 || self.height == 0 {
            return *other;
        }
        if other.width == 0 || other.height == 0 {
            return *self;
        }
        let left = self.left().min(other.left());
        let top = self.top().min(other.top());
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());
        Rect::new(left, top, right - left, bottom - top)
    }
}

/// One physical display attached to one machine, in that machine's local space.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonitorInfo {
    pub id: MonitorId,
    /// Human-readable name for the arrangement UI, e.g. "Built-in Retina".
    pub name: String,
    pub bounds: Rect,
    /// Backing-scale factor. Carried so the UI can draw the canvas in physical
    /// proportions; coordinates on the wire are always logical points.
    pub scale: f32,
    pub primary: bool,
}
