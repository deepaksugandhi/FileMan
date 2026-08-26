//! Per-instance taskbar/title-bar differentiation (SPEC §11).
//!
//! Two things are needed, not just one:
//!
//! 1. **A distinct taskbar button per instance.** By default Windows groups
//!    all windows from the same exe into one taskbar button (whichever
//!    instance last had focus "wins" the icon/label shown), so recoloring a
//!    window's icon alone is invisible — you only ever see one button.
//!    Fixed by giving each process its own `AppUserModelID` via
//!    `SetCurrentProcessExplicitAppUserModelID`, called once before the
//!    window is created; Windows then never merges them.
//! 2. **A distinct icon per instance**, applied via `WM_SETICON` — the same
//!    icon the taskbar always displays, so (unlike an `ITaskbarList3`
//!    overlay badge) there's no "show badges" setting that can hide it.
//!
//! Both are keyed off the same open-order slot, claimed once via a
//! pagefile-backed shared memory counter (name-scoped to this app, no disk
//! file): it resets to zero automatically once every instance exits, since
//! the OS frees the mapping when the last handle to it closes.

/// Fixed palette assigned in open-order, cycling if more instances than colors.
const PALETTE: [(u8, u8, u8); 6] = [
    (66, 133, 244), // blue
    (52, 168, 83),  // green
    (251, 140, 0),  // orange
    (156, 39, 176), // purple
    (229, 57, 53),  // red
    (0, 172, 193),  // cyan
];

#[cfg(windows)]
mod win {
    use super::PALETTE;
    use std::sync::atomic::{AtomicU32, Ordering};
    use windows::Win32::Foundation::{HWND, INVALID_HANDLE_VALUE, LPARAM, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS, HDC,
    };
    use windows::Win32::System::Memory::{
        CreateFileMappingW, FILE_MAP_ALL_ACCESS, MapViewOfFile, PAGE_READWRITE,
    };
    use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateIconIndirect, HICON, ICON_BIG, ICON_SMALL, ICONINFO, SendMessageW, WM_SETICON,
    };
    use windows::core::PCWSTR;

    /// Atomically claims the next open-order slot across all running
    /// instances via a named, pagefile-backed shared memory section.
    pub fn claim_instance_slot() -> usize {
        unsafe {
            let name: Vec<u16> = "Local\\FileManInstanceCounter\0".encode_utf16().collect();
            let Ok(mapping) = CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                None,
                PAGE_READWRITE,
                0,
                4,
                PCWSTR(name.as_ptr()),
            ) else {
                return 0;
            };
            let view = MapViewOfFile(mapping, FILE_MAP_ALL_ACCESS, 0, 0, 4);
            if view.Value.is_null() {
                return 0;
            }
            // Leaked intentionally: mapping/view must outlive this function
            // for the counter to stay alive for the process's lifetime; the
            // OS reclaims both on process exit.
            let counter = &*(view.Value as *const AtomicU32);
            counter.fetch_add(1, Ordering::SeqCst) as usize % PALETTE.len()
        }
    }

    /// Gives this process its own taskbar identity so Windows doesn't group
    /// it with other running instances into a single shared button. Must be
    /// called before the window is created.
    pub fn set_app_identity(slot: usize) {
        let id: Vec<u16> = format!("FileMan.Instance.{slot}.{}\0", std::process::id())
            .encode_utf16()
            .collect();
        unsafe {
            let _ = SetCurrentProcessExplicitAppUserModelID(PCWSTR(id.as_ptr()));
        }
    }

    /// Builds a solid-color square icon of `size`x`size` pixels.
    fn colored_icon((r, g, b): (u8, u8, u8), size: i32) -> Option<HICON> {
        unsafe {
            let mut bmi = BITMAPINFO::default();
            bmi.bmiHeader = BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            };
            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            let color = CreateDIBSection(
                None as Option<HDC>,
                &bmi,
                DIB_RGB_COLORS,
                &mut bits,
                None,
                0,
            )
            .ok()?;
            if bits.is_null() {
                return None;
            }
            let pixels =
                std::slice::from_raw_parts_mut(bits as *mut u8, (size * size * 4) as usize);
            for px in pixels.chunks_exact_mut(4) {
                px[0] = b;
                px[1] = g;
                px[2] = r;
                px[3] = 255;
            }
            let mask = CreateBitmap(size, size, 1, 1, None);
            let icon_info = ICONINFO {
                fIcon: true.into(),
                xHotspot: 0,
                yHotspot: 0,
                hbmMask: mask,
                hbmColor: color,
            };
            CreateIconIndirect(&icon_info).ok()
        }
    }

    /// Replaces this window's title-bar and taskbar icon with the color for `slot`.
    pub fn apply_instance_icon(hwnd: HWND, slot: usize) {
        let color = PALETTE[slot % PALETTE.len()];
        unsafe {
            if let Some(big) = colored_icon(color, 32) {
                SendMessageW(
                    hwnd,
                    WM_SETICON,
                    Some(WPARAM(ICON_BIG as usize)),
                    Some(LPARAM(big.0 as isize)),
                );
            }
            if let Some(small) = colored_icon(color, 16) {
                SendMessageW(
                    hwnd,
                    WM_SETICON,
                    Some(WPARAM(ICON_SMALL as usize)),
                    Some(LPARAM(small.0 as isize)),
                );
            }
        }
    }
}

#[cfg(not(windows))]
mod win {
    pub fn claim_instance_slot() -> usize {
        0
    }
    pub fn set_app_identity(_slot: usize) {}
    pub fn apply_instance_icon(_hwnd: (), _slot: usize) {}
}

/// Claims this process's open-order slot and sets its taskbar identity so it
/// won't be grouped with other running instances. Call once, early in
/// `main`, before the window is created.
pub fn claim_instance_slot_and_set_identity() -> usize {
    let slot = win::claim_instance_slot();
    win::set_app_identity(slot);
    slot
}

/// Applies `slot`'s color to this window's title-bar/taskbar icon. No-op on
/// non-Windows or if the HWND can't be resolved.
pub fn apply_instance_icon(frame: &eframe::Frame, slot: usize) {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if let Ok(handle) = frame.window_handle() {
            if let RawWindowHandle::Win32(h) = handle.as_raw() {
                let hwnd = windows::Win32::Foundation::HWND(h.hwnd.get() as *mut _);
                win::apply_instance_icon(hwnd, slot);
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (frame, slot);
    }
}
