//! The drag-and-drop screen arrangement.
//!
//! Draws every machine's monitors on one scaled canvas and lets you drag a
//! machine to say where it physically sits. On release the machine snaps
//! *flush* against its nearest neighbour rather than staying where it was
//! dropped — a gap between screens is canvas belonging to no monitor, and the
//! pointer would stop dead in it instead of crossing.

use eframe::egui;
use tether_core::layout::{Layout, MachineId, Side};
use tether_proto::Rect as ScreenRect;

/// A drag in progress.
pub struct Drag {
    pub machine: MachineId,
    /// Canvas position of the pointer when the drag began.
    start: egui::Pos2,
    /// The machine's origin at that moment, so the whole gesture is one edit.
    origin: tether_proto::Point,
}

/// Returns a new layout when the arrangement changed.
pub fn show(
    ui: &mut egui::Ui,
    layout: &Layout,
    this: MachineId,
    cursor_on: Option<MachineId>,
    cursor_position: Option<tether_proto::Point>,
    input_owner: Option<MachineId>,
    drag: &mut Option<Drag>,
) -> Option<Layout> {
    let available = ui.available_size();
    let (response, painter) = ui.allocate_painter(available, egui::Sense::drag());
    let viewport = response.rect;

    painter.rect_filled(viewport, 6.0, ui.visuals().extreme_bg_color);

    let bounds = layout.bounds();
    if bounds.width == 0 || bounds.height == 0 {
        return None;
    }

    // Uniform scale with a margin, so screens keep their real proportions —
    // stretching them to fill would misrepresent the very thing being arranged.
    let margin = 28.0;
    let scale = ((viewport.width() - margin * 2.0) / bounds.width as f32)
        .min((viewport.height() - margin * 2.0) / bounds.height as f32)
        .min(0.5);

    let drawn = egui::vec2(bounds.width as f32 * scale, bounds.height as f32 * scale);
    let offset = viewport.center() - drawn * 0.5;

    let to_screen = |r: ScreenRect| -> egui::Rect {
        egui::Rect::from_min_size(
            egui::pos2(
                offset.x + (r.x - bounds.x) as f32 * scale,
                offset.y + (r.y - bounds.y) as f32 * scale,
            ),
            egui::vec2(r.width as f32 * scale, r.height as f32 * scale),
        )
    };

    let mut result = None;
    let mut hovered: Option<MachineId> = None;

    // ---- draw ----
    for placement in &layout.machines {
        let is_this = placement.machine == this;
        let has_cursor = cursor_on == Some(placement.machine);
        let is_driving = input_owner == Some(placement.machine);

        let outer = to_screen(placement.global_bounds());
        if response.hover_pos().is_some_and(|p| outer.contains(p)) {
            hovered = Some(placement.machine);
        }

        for monitor in &placement.monitors {
            let rect = to_screen(placement.global_rect_of(monitor));

            let fill = if is_driving {
                egui::Color32::from_rgb(79, 70, 229)
            } else if is_this {
                egui::Color32::from_rgb(55, 58, 92)
            } else {
                egui::Color32::from_rgb(48, 50, 62)
            };
            painter.rect_filled(rect, 4.0, fill);

            // The pointer's current screen gets the bright outline: when you
            // are looking at this window to work out where the cursor went,
            // that is the only question you have.
            let stroke = if has_cursor {
                egui::Stroke::new(2.5_f32, egui::Color32::from_rgb(120, 200, 255))
            } else {
                egui::Stroke::new(1.0_f32, egui::Color32::from_gray(90))
            };
            painter.rect_stroke(rect, 4.0, stroke, egui::StrokeKind::Inside);

            painter.text(
                rect.center_bottom() - egui::vec2(0.0, 6.0),
                egui::Align2::CENTER_BOTTOM,
                format!("{}×{}", monitor.bounds.width, monitor.bounds.height),
                egui::FontId::proportional(10.0),
                egui::Color32::from_gray(150),
            );
        }

        let mut label = placement.name.clone();
        if is_this {
            label.push_str("  (this machine)");
        }
        painter.text(
            outer.center_top() + egui::vec2(0.0, 8.0),
            egui::Align2::CENTER_TOP,
            label,
            egui::FontId::proportional(13.0),
            egui::Color32::WHITE,
        );

        if is_driving {
            painter.text(
                outer.center(),
                egui::Align2::CENTER_CENTER,
                "driving",
                egui::FontId::proportional(11.0),
                egui::Color32::from_rgb(200, 210, 255),
            );
        }
    }

    // ---- where the pointer actually is ----
    if let Some(at) = cursor_position {
        let p = egui::pos2(
            offset.x + (at.x - bounds.x) as f32 * scale,
            offset.y + (at.y - bounds.y) as f32 * scale,
        );
        if viewport.contains(p) {
            painter.circle_filled(p, 5.0, egui::Color32::from_rgb(120, 200, 255));
            painter.circle_stroke(
                p,
                9.0,
                egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(120, 200, 255)),
            );
        }
    }

    // ---- drag ----
    if response.drag_started() {
        if let (Some(pos), Some(machine)) = (response.interact_pointer_pos(), hovered) {
            if let Some(placement) = layout.get(machine) {
                *drag = Some(Drag {
                    machine,
                    start: pos,
                    origin: placement.origin,
                });
            }
        }
    }

    if let Some(active) = drag.as_ref() {
        if response.dragged() {
            if let Some(pos) = response.interact_pointer_pos() {
                // Preview only. The layout is not committed until release, so a
                // half-finished drag never reaches the running daemon.
                if let Some(placement) = layout.get(active.machine) {
                    let delta = (pos - active.start) / scale;
                    let moved = ScreenRect::new(
                        placement.local_bounds().x + active.origin.x + delta.x as i32,
                        placement.local_bounds().y + active.origin.y + delta.y as i32,
                        placement.local_bounds().width,
                        placement.local_bounds().height,
                    );
                    painter.rect_stroke(
                        to_screen(moved),
                        4.0,
                        egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(120, 200, 255)),
                        egui::StrokeKind::Outside,
                    );
                }
            }
        }

        if response.drag_stopped() {
            if let Some(pos) = response.interact_pointer_pos() {
                let delta = (pos - active.start) / scale;
                result = snap(
                    layout,
                    active.machine,
                    tether_proto::Point::new(
                        active.origin.x + delta.x as i32,
                        active.origin.y + delta.y as i32,
                    ),
                );
            }
            *drag = None;
        }
    }

    ui.painter().text(
        viewport.left_bottom() + egui::vec2(10.0, -10.0),
        egui::Align2::LEFT_BOTTOM,
        "screens snap flush — a gap would stop the pointer instead of crossing",
        egui::FontId::proportional(11.0),
        ui.visuals().weak_text_color(),
    );

    result
}

/// Place `machine` against its nearest neighbour, on whichever side the drop
/// points at, at the position along that edge where it was dropped.
///
/// The drop decides the side *and* where along it — only the perpendicular
/// coordinate is snapped. Snapping both, which is what centring does, makes a
/// machine impossible to put under one screen of a two-screen desktop: it
/// always lands under the middle, which with unequal screens can be under the
/// gap between them, touching neither.
///
/// The perpendicular axis is still snapped flush. A hand-placed gap is canvas
/// belonging to no monitor, and the pointer stops dead in it.
fn snap(
    layout: &Layout,
    machine: MachineId,
    dropped_origin: tether_proto::Point,
) -> Option<Layout> {
    let mut next = layout.clone();
    {
        let placement = next.get_mut(machine)?;
        placement.origin = dropped_origin;
    }

    let dropped = next.get(machine)?.global_bounds();
    let dropped_centre = (
        dropped.x + dropped.width / 2,
        dropped.y + dropped.height / 2,
    );

    // Nearest other machine by centre distance.
    let anchor = next
        .machines
        .iter()
        .filter(|p| p.machine != machine)
        .min_by_key(|p| {
            let b = p.global_bounds();
            let dx = (b.x + b.width / 2 - dropped_centre.0) as i64;
            let dy = (b.y + b.height / 2 - dropped_centre.1) as i64;
            dx * dx + dy * dy
        })
        .map(|p| p.machine)?;

    let anchor_rect = next.get(anchor)?.global_bounds();

    // How far outside the anchor the drop sits on each axis. Comparing these
    // rather than centre offsets is what lets a small machine be dropped below
    // a wide one: against a 3840-wide desktop the horizontal centre offset
    // dominates almost everywhere, so centre comparison would call nearly
    // every drop "left" or "right".
    let out_left = anchor_rect.left() - dropped.right();
    let out_right = dropped.left() - anchor_rect.right();
    let out_above = anchor_rect.top() - dropped.bottom();
    let out_below = dropped.top() - anchor_rect.bottom();

    let horizontal = out_left.max(out_right);
    let vertical = out_above.max(out_below);

    let side = if horizontal >= vertical {
        if out_left >= out_right {
            Side::Left
        } else {
            Side::Right
        }
    } else if out_above >= out_below {
        Side::Above
    } else {
        Side::Below
    };

    // Keep where it was dropped along the shared edge.
    let along = match side {
        Side::Left | Side::Right => dropped.y,
        Side::Above | Side::Below => dropped.x,
    };

    next.place_flush(machine, side, anchor, along).ok()?;
    Some(next)
}
