//! Monitor-aware window positioning: on Windows, resolve the real device
//! name (e.g. `\\.\DISPLAY1`) a window sits on, so a saved position can be
//! matched back to the same physical monitor on restore rather than an
//! arbitrary index, and clamped into that monitor's work area so the window
//! never restores fully off-screen.

#[derive(Debug, Clone, PartialEq)]
pub struct MonitorInfo {
    pub name: String,
    /// Work-area rect in screen points: (left, top, right, bottom).
    pub work: (f32, f32, f32, f32),
    pub is_primary: bool,
}

/// Clamps a saved window position into the given monitor work area. Pure
/// logic (no HWND needed), so it's directly unit-testable.
pub fn clamp_into_work_area(
    pos: (f32, f32),
    size: (f32, f32),
    work: (f32, f32, f32, f32),
) -> (f32, f32) {
    let (x, y) = pos;
    let (w, h) = size;
    let (left, top, right, bottom) = work;
    let max_x = (right - w).max(left);
    let max_y = (bottom - h).max(top);
    (x.clamp(left, max_x), y.clamp(top, max_y))
}

#[cfg(windows)]
mod win {
    use super::MonitorInfo;
    use windows::Win32::Foundation::{HWND, LPARAM, RECT};
    use windows::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, GetMonitorInfoW, MonitorFromWindow, HDC, HMONITOR,
        MONITORINFOEXW, MONITOR_DEFAULTTONEAREST,
    };

    fn monitor_info_for(hmonitor: HMONITOR) -> Option<MonitorInfo> {
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        let ok = unsafe { GetMonitorInfoW(hmonitor, &mut info.monitorInfo as *mut _) };
        if !ok.as_bool() {
            return None;
        }
        let len = info
            .szDevice
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(info.szDevice.len());
        let name = String::from_utf16_lossy(&info.szDevice[..len]);
        let rc = info.monitorInfo.rcWork;
        const MONITORINFOF_PRIMARY: u32 = 1;
        Some(MonitorInfo {
            name,
            work: (rc.left as f32, rc.top as f32, rc.right as f32, rc.bottom as f32),
            is_primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
        })
    }

    /// Returns the device name of the monitor nearest the given HWND.
    pub fn monitor_name_for_hwnd(hwnd: HWND) -> Option<String> {
        let hmonitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
        monitor_info_for(hmonitor).map(|m| m.name)
    }

    unsafe extern "system" fn collect_monitor(
        hmonitor: HMONITOR,
        _hdc: HDC,
        _rect: *mut RECT,
        lparam: LPARAM,
    ) -> windows::core::BOOL {
        let monitors = unsafe { &mut *(lparam.0 as *mut Vec<MonitorInfo>) };
        if let Some(info) = monitor_info_for(hmonitor) {
            monitors.push(info);
        }
        windows::Win32::Foundation::TRUE
    }

    /// Enumerates all currently connected monitors.
    pub fn list_monitors() -> Vec<MonitorInfo> {
        let mut monitors: Vec<MonitorInfo> = Vec::new();
        unsafe {
            let _ = EnumDisplayMonitors(
                None,
                None,
                Some(collect_monitor),
                LPARAM(&mut monitors as *mut _ as isize),
            );
        }
        monitors
    }
}

#[cfg(windows)]
pub use win::{list_monitors, monitor_name_for_hwnd};

#[cfg(not(windows))]
pub fn list_monitors() -> Vec<MonitorInfo> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_leaves_in_bounds_position_unchanged() {
        let pos = clamp_into_work_area((100.0, 100.0), (800.0, 600.0), (0.0, 0.0, 1920.0, 1080.0));
        assert_eq!(pos, (100.0, 100.0));
    }

    #[test]
    fn clamp_pulls_negative_position_back_into_work_area() {
        let pos = clamp_into_work_area((-500.0, -500.0), (800.0, 600.0), (0.0, 0.0, 1920.0, 1080.0));
        assert_eq!(pos, (0.0, 0.0));
    }

    #[test]
    fn clamp_pulls_position_beyond_far_edge_back_in() {
        let pos = clamp_into_work_area((3000.0, 3000.0), (800.0, 600.0), (0.0, 0.0, 1920.0, 1080.0));
        assert_eq!(pos, (1120.0, 480.0));
    }

    #[test]
    fn clamp_handles_a_window_larger_than_the_work_area() {
        // Oversized window: pin to the work-area origin rather than going negative.
        let pos = clamp_into_work_area((50.0, 50.0), (2000.0, 2000.0), (0.0, 0.0, 1920.0, 1080.0));
        assert_eq!(pos, (0.0, 0.0));
    }

    #[test]
    fn clamp_respects_a_non_zero_origin_work_area() {
        // e.g. a secondary monitor to the right of the primary.
        let pos = clamp_into_work_area((100.0, 100.0), (800.0, 600.0), (1920.0, 0.0, 3840.0, 1080.0));
        assert_eq!(pos, (1920.0, 100.0));
    }
}
