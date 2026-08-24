//! Icon extraction for custom "open with" actions.
//!
//! Pulls the first (large) icon out of an executable via `ExtractIconExW`,
//! decodes it to RGBA pixels with `GetIconInfo` + `GetDIBits`, and wraps the
//! result in an egui texture for use on toolbar buttons.

use eframe::egui;

/// Extracts the exe's icon and caches it as an egui texture.
#[cfg(windows)]
pub fn load_icon_texture(ctx: &egui::Context, exe_path: &str) -> Option<egui::TextureHandle> {
    let (mut rgba, [w, h]) = extract_first_icon_rgba(exe_path)?;
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2); // BGRA -> RGBA
    }
    if !has_any_alpha(&rgba) {
        // Legacy icons carry no alpha channel; treat them as fully opaque.
        for px in rgba.chunks_exact_mut(4) {
            px[3] = 255;
        }
    }
    let image = egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba);
    Some(ctx.load_texture("custom_action_icon", image, egui::TextureOptions::LINEAR))
}

/// True if any pixel in `rgba` has a non-zero alpha byte.
fn has_any_alpha(rgba: &[u8]) -> bool {
    rgba.chunks_exact(4).any(|px| px[3] != 0)
}

#[cfg(windows)]
fn extract_first_icon_rgba(exe_path: &str) -> Option<(Vec<u8>, [usize; 2])> {
    use windows::core::PCWSTR;
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC,
        BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS,
    };
    use windows::Win32::UI::Shell::ExtractIconExW;
    use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};

    let wide: Vec<u16> = exe_path.encode_utf16().chain(std::iter::once(0)).collect();
    let mut hicon = HICON::default();
    let count =
        unsafe { ExtractIconExW(PCWSTR(wide.as_ptr()), 0, Some(&mut hicon), None, 1) };
    if count == 0 || hicon.is_invalid() {
        return None;
    }

    let result = (|| {
        let mut info = ICONINFO::default();
        unsafe { GetIconInfo(hicon, &mut info) }.ok()?;

        let screen_dc = unsafe { GetDC(None) };
        let mem_dc = unsafe { CreateCompatibleDC(Some(screen_dc)) };

        let mut out: Option<(Vec<u8>, [usize; 2])> = None;
        unsafe {
            let mut bm = BITMAP::default();
            if GetObjectW(
                info.hbmColor.into(),
                std::mem::size_of::<BITMAP>() as i32,
                Some(&mut bm as *mut _ as *mut _),
            ) != 0
            {
                let w = bm.bmWidth.max(0) as usize;
                let h = bm.bmHeight.max(0) as usize;
                if w > 0 && h > 0 && w <= 256 && h <= 256 {
                    let mut buf = vec![0u8; w * h * 4];
                    let mut bi = BITMAPINFO::default();
                    bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
                    bi.bmiHeader.biWidth = w as i32;
                    bi.bmiHeader.biHeight = -(h as i32); // top-down rows
                    bi.bmiHeader.biPlanes = 1;
                    bi.bmiHeader.biBitCount = 32;
                    bi.bmiHeader.biCompression = 0; // BI_RGB
                    let lines = GetDIBits(
                        mem_dc,
                        info.hbmColor,
                        0,
                        h as u32,
                        Some(buf.as_mut_ptr() as _),
                        &mut bi,
                        DIB_RGB_COLORS,
                    );
                    if lines == h as i32 {
                        out = Some((buf, [w, h]));
                    }
                }
            }
            let _ = DeleteObject(info.hbmColor.into());
            let _ = DeleteObject(info.hbmMask.into());
            let _ = DeleteDC(mem_dc);
            ReleaseDC(None, screen_dc);
        }
        out
    })();

    unsafe {
        let _ = DestroyIcon(hicon);
    }
    result
}

#[cfg(not(windows))]
pub fn load_icon_texture(_ctx: &egui::Context, _exe_path: &str) -> Option<egui::TextureHandle> {
    None
}
