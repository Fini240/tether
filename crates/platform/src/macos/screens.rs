//! Enumerating displays.
//!
//! `CGDisplayBounds` reports the *global display coordinate space*: origin at
//! the top-left of the main display, y increasing downward, exactly the space
//! `CGWarpMouseCursorPosition` and mouse events use. No flipping is needed
//! here — that trap only appears if you reach for AppKit's `NSScreen`, whose
//! origin is bottom-left.

use tether_proto::{MonitorId, MonitorInfo, Rect};

use super::ffi::*;
use crate::traits::{Monitors, PlatformError, Result};

/// Backing pixels per logical point: 2.0 on a Retina panel, 1.0 otherwise.
///
/// Read from the display *mode* rather than `CGDisplayPixelsWide`, which
/// returns logical points once the user picks a scaled resolution — so a
/// Retina MacBook reports 1.0 through that call and the arrangement UI would
/// draw every screen at the wrong physical size.
fn backing_scale(display: CGDirectDisplayID) -> f32 {
    unsafe {
        let mode = CGDisplayCopyDisplayMode(display);
        if mode.is_null() {
            return 1.0;
        }
        let points = CGDisplayModeGetWidth(mode);
        let pixels = CGDisplayModeGetPixelWidth(mode);
        CGDisplayModeRelease(mode);

        if points == 0 {
            1.0
        } else {
            pixels as f32 / points as f32
        }
    }
}

/// Nobody has this many displays; the cap just bounds the stack array.
const MAX_DISPLAYS: u32 = 16;

pub struct MacMonitors;

impl Monitors for MacMonitors {
    fn enumerate(&self) -> Result<Vec<MonitorInfo>> {
        let mut ids = [0u32; MAX_DISPLAYS as usize];
        let mut count: u32 = 0;

        let err = unsafe { CGGetActiveDisplayList(MAX_DISPLAYS, ids.as_mut_ptr(), &mut count) };
        if err != 0 {
            return Err(PlatformError::backend(format!(
                "CGGetActiveDisplayList failed: {err}"
            )));
        }
        if count == 0 {
            // CoreGraphics answers successfully with zero displays when the
            // process has no WindowServer connection — an SSH session, a LaunchDaemon
            // in the system context, or a sandbox without GUI access. That is
            // not a hardware problem, and the message should not imply it is.
            return Err(PlatformError::backend(
                "CoreGraphics reports no active displays. This usually means the \
                 process has no window server session — it is running over SSH, as a \
                 system LaunchDaemon rather than a per-user LaunchAgent, or in a \
                 sandbox without GUI access. Run it from a logged-in graphical \
                 session, or use --backend headless",
            ));
        }

        let main = unsafe { CGMainDisplayID() };
        let monitors = ids[..count as usize]
            .iter()
            .map(|&id| {
                let bounds = unsafe { CGDisplayBounds(id) };
                let width = bounds.size.width as i32;
                MonitorInfo {
                    id: MonitorId(id),
                    name: format!("Display {id}"),
                    bounds: Rect::new(
                        bounds.origin.x as i32,
                        bounds.origin.y as i32,
                        width,
                        bounds.size.height as i32,
                    ),
                    scale: backing_scale(id),
                    primary: id == main,
                }
            })
            .collect();

        Ok(monitors)
    }
}
