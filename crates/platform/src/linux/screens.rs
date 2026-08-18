//! Enumerating displays, from the kernel rather than the display server.
//!
//! The other two backends ask the window system, which knows both the size of
//! each screen *and* where the user dragged it in the arrangement. Linux has
//! no single answer to ask: that layout lives in X11's RandR, or in a Wayland
//! compositor that need not expose it to anyone, or nowhere at all on a bare
//! console. Picking one would mean a backend that works on one kind of desktop
//! and not the others, which is exactly what using evdev everywhere else here
//! is meant to avoid.
//!
//! So this reads `/sys/class/drm`, which the kernel maintains regardless of
//! what is drawing on top of it. That gives the connected outputs and their
//! resolutions exactly, and their *arrangement* not at all — so the screens are
//! laid out left to right in connector order.
//!
//! For one screen, which is most laptops, that is exactly right and there is
//! nothing to correct. For several it is a guess, and a wrong guess shows up
//! as the pointer entering the next screen at the wrong height. Tether's own
//! arrangement editor is the fix: it already exists for placing machines
//! relative to each other, and the same drag places these.

use std::fs;
use std::path::Path;

use tether_proto::{MonitorId, MonitorInfo, Rect};

use crate::traits::{Monitors, PlatformError, Result};

pub struct LinuxMonitors;

impl Monitors for LinuxMonitors {
    fn enumerate(&self) -> Result<Vec<MonitorInfo>> {
        let outputs = connected_outputs()?;
        if outputs.is_empty() {
            return Err(PlatformError::backend(
                "no connected display found in /sys/class/drm. If this machine \
                 really has a screen, its driver is not exposing one — run with \
                 --backend headless to check the rest of the stack works.",
            ));
        }

        // Left to right in connector order, because nothing here knows better.
        let mut x = 0;
        let monitors = outputs
            .into_iter()
            .enumerate()
            .map(|(index, output)| {
                let bounds = Rect::new(x, 0, output.width, output.height);
                x += output.width;
                MonitorInfo {
                    id: MonitorId(index as u32),
                    name: output.name,
                    bounds,
                    // DRM reports pixels. Whatever scaling the desktop applies
                    // on top is the display server's business and it does not
                    // tell us, so 1.0 is the only honest answer.
                    scale: 1.0,
                    primary: index == 0,
                }
            })
            .collect();
        Ok(monitors)
    }
}

/// One connected output, as the kernel describes it.
struct Output {
    name: String,
    width: i32,
    height: i32,
}

/// Every connected output with a usable mode, in connector order.
fn connected_outputs() -> Result<Vec<Output>> {
    let root = Path::new("/sys/class/drm");
    let entries = fs::read_dir(root)
        .map_err(|e| PlatformError::backend(format!("cannot read /sys/class/drm: {e}")))?;

    let mut outputs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        // Connector directories are `cardN-<CONNECTOR>`; `cardN` itself is the
        // device and has no status file, which the read below rules out anyway.
        let Ok(status) = fs::read_to_string(path.join("status")) else {
            continue;
        };
        if status.trim() != "connected" {
            continue;
        }
        let Some((width, height)) = first_mode(&path) else {
            continue;
        };

        let name = entry.file_name().to_string_lossy().to_string();
        // `card0-DP-1` reads as `DP-1`: the card number is noise in a UI that
        // is showing one machine's screens.
        let name = name
            .split_once('-')
            .map_or(name.clone(), |(_, rest)| rest.to_string());
        outputs.push(Output {
            name,
            width,
            height,
        });
    }

    // Connector order is directory order, which is not sorted. Sort by name so
    // the arrangement is at least stable across restarts — a layout the user
    // has corrected by hand must not be reshuffled by a reboot.
    outputs.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(outputs)
}

/// The first line of the connector's `modes` file: its preferred mode.
fn first_mode(connector: &Path) -> Option<(i32, i32)> {
    let modes = fs::read_to_string(connector.join("modes")).ok()?;
    let line = modes.lines().next()?.trim();
    let (width, height) = line.split_once('x')?;
    // Interlaced modes are written `1920x1080i`; the trailing letter is not
    // part of the number and `parse` would reject the whole thing.
    let height: String = height.chars().take_while(char::is_ascii_digit).collect();
    Some((width.parse().ok()?, height.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_mode_parses() {
        let dir = tempdir("plain");
        fs::write(dir.join("modes"), "1920x1080\n1280x720\n").unwrap();
        assert_eq!(first_mode(&dir), Some((1920, 1080)));
    }

    #[test]
    fn an_interlaced_mode_parses() {
        // `1920x1080i` must not take the whole file down with it.
        let dir = tempdir("interlaced");
        fs::write(dir.join("modes"), "1920x1080i\n").unwrap();
        assert_eq!(first_mode(&dir), Some((1920, 1080)));
    }

    #[test]
    fn an_output_with_no_modes_is_skipped() {
        let dir = tempdir("empty");
        fs::write(dir.join("modes"), "").unwrap();
        assert_eq!(first_mode(&dir), None);
    }

    fn tempdir(tag: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("tether-drm-{tag}-{}", std::process::id()));
        let _ = fs::create_dir_all(&path);
        path
    }
}
