//! Enumerating displays with `EnumDisplayMonitors`.

use std::cell::RefCell;

use tether_proto::{MonitorId, MonitorInfo, Rect};
use windows_sys::Win32::Foundation::{BOOL, LPARAM, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};

/// `MONITORINFOF_PRIMARY`. Not re-exported by windows-sys under the features
/// this crate enables, and it is a stable documented value.
const MONITORINFOF_PRIMARY: u32 = 1;

use crate::traits::{Monitors, PlatformError, Result};

thread_local! {
    /// `EnumDisplayMonitors` hands the callback an `LPARAM`, but keeping the
    /// collection here avoids passing a raw pointer to a Rust `Vec` through C.
    static COLLECTED: RefCell<Vec<MonitorInfo>> = const { RefCell::new(Vec::new()) };
}

pub struct WindowsMonitors;

impl Monitors for WindowsMonitors {
    fn enumerate(&self) -> Result<Vec<MonitorInfo>> {
        COLLECTED.with(|c| c.borrow_mut().clear());

        let ok = unsafe {
            EnumDisplayMonitors(
                std::ptr::null_mut(),
                std::ptr::null(),
                Some(monitor_callback),
                0,
            )
        };
        if ok == 0 {
            return Err(PlatformError::backend("EnumDisplayMonitors failed"));
        }

        let monitors = COLLECTED.with(|c| c.borrow().clone());
        if monitors.is_empty() {
            return Err(PlatformError::backend("no displays were reported"));
        }
        Ok(monitors)
    }
}

unsafe extern "system" fn monitor_callback(
    monitor: HMONITOR,
    _hdc: HDC,
    _clip: *mut RECT,
    _data: LPARAM,
) -> BOOL {
    let mut info: MONITORINFOEXW = std::mem::zeroed();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;

    if GetMonitorInfoW(monitor, &mut info as *mut _ as *mut MONITORINFO) == 0 {
        // Skip this one rather than abandoning the whole enumeration: one
        // unreadable display should not cost us the others.
        return 1;
    }

    // rcMonitor, not rcWork: the work area excludes the taskbar, and a cursor
    // must be able to reach every pixel of the screen — including the taskbar —
    // for edge crossing to feel right.
    let r = info.monitorInfo.rcMonitor;
    let primary = info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0;

    let name = String::from_utf16_lossy(
        &info
            .szDevice
            .iter()
            .copied()
            .take_while(|&c| c != 0)
            .collect::<Vec<u16>>(),
    );

    // HMONITOR is a handle and is not stable across display changes, so the id
    // is derived from position instead — stable for as long as the arrangement
    // is, which is what the saved layout needs.
    let id = MonitorId(((r.left as u32) << 16) ^ (r.top as u32) ^ (r.right as u32));

    COLLECTED.with(|c| {
        c.borrow_mut().push(MonitorInfo {
            id,
            name: if name.is_empty() {
                format!("Display {id:?}")
            } else {
                name.trim_start_matches(r"\\.\").to_string()
            },
            bounds: Rect::new(r.left, r.top, r.right - r.left, r.bottom - r.top),
            // Coordinates are already in physical pixels because the process
            // declares per-monitor DPI awareness at startup; see mod.rs.
            scale: 1.0,
            primary,
        })
    });

    1
}
