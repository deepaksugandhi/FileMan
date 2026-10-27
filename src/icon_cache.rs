//! Icon extraction for toolbar buttons and file listings.
//!
//! Two icon sources, sharing one HICON→RGBA→egui-texture pipeline:
//! - Executables: first (large) icon out of the binary via `ExtractIconExW`
//!   (used for custom "open with" toolbar actions).
//! - Arbitrary files: the shell's associated icon for that file — the same
//!   icon Explorer shows for its type — via `SHGetFileInfoW`.

use std::path::Path;

use eframe::egui;

/// Raw BGRA pixels and size of a shell icon, produced off the UI thread.
type RawIcon = Option<(Vec<u8>, [usize; 2])>;

/// Shell-icon cache for file listings. Misses are resolved on a background
/// worker (`SHGetFileInfoW` can take tens of ms — per file for exe/lnk/ico,
/// and far longer on network paths), so scrolling never blocks on the shell.
/// Until an icon arrives the caller sees `None` and draws its text fallback.
pub struct FileIconCache {
    cache: crate::bounded_cache::BoundedCache<String, Option<egui::TextureHandle>>,
    pending: std::collections::HashSet<String>,
    req_tx: std::sync::mpsc::Sender<(String, std::path::PathBuf, egui::Context)>,
    done_rx: std::sync::mpsc::Receiver<(String, RawIcon)>,
}

/// Cap on in-flight requests so a fast scroll through a huge folder doesn't
/// queue work for rows that are already off screen.
const MAX_PENDING_ICONS: usize = 64;

impl FileIconCache {
    pub fn new(max_entries: usize, max_bytes: usize) -> Self {
        let (req_tx, req_rx) =
            std::sync::mpsc::channel::<(String, std::path::PathBuf, egui::Context)>();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            init_worker_com();
            while let Ok((key, path, ctx)) = req_rx.recv() {
                let raw = file_icon_rgba(&path);
                if done_tx.send((key, raw)).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        });
        FileIconCache {
            cache: crate::bounded_cache::BoundedCache::new(max_entries, max_bytes),
            pending: Default::default(),
            req_tx,
            done_rx,
        }
    }

    fn drain(&mut self, ctx: &egui::Context) {
        while let Ok((key, raw)) = self.done_rx.try_recv() {
            self.pending.remove(&key);
            let texture = raw.map(|(rgba, size)| finish_texture(ctx, "file_icon", rgba, size));
            let bytes = key.len()
                + texture
                    .as_ref()
                    .map_or(0, |t| t.size()[0] * t.size()[1] * 4);
            self.cache.insert(key, texture, bytes);
        }
    }
}

pub fn cached_file_icon(
    cache: &mut FileIconCache,
    ctx: &egui::Context,
    path: &Path,
) -> Option<egui::TextureHandle> {
    cache.drain(ctx);
    let key = file_icon_cache_key(path);
    if let Some(texture) = cache.cache.get(&key) {
        return texture.clone();
    }
    if cache.pending.len() < MAX_PENDING_ICONS && !cache.pending.contains(&key) {
        cache.pending.insert(key.clone());
        let _ = cache.req_tx.send((key, path.to_path_buf(), ctx.clone()));
    }
    None
}

#[cfg(windows)]
fn init_worker_com() {
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
}

#[cfg(not(windows))]
fn init_worker_com() {}

/// Extracts the exe's icon and caches it as an egui texture.
#[cfg(windows)]
pub fn load_icon_texture(ctx: &egui::Context, exe_path: &str) -> Option<egui::TextureHandle> {
    let hicon = extract_first_icon_hicon(exe_path)?;
    let decoded = hicon_to_rgba(hicon);
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::DestroyIcon(hicon);
    }
    let (rgba, size) = decoded?;
    Some(finish_texture(ctx, "custom_action_icon", rgba, size))
}

#[cfg(not(windows))]
pub fn load_icon_texture(_ctx: &egui::Context, _exe_path: &str) -> Option<egui::TextureHandle> {
    None
}

/// Cache key for a file's associated icon. Most file types resolve their
/// icon purely from the extension (one texture serves every `.txt`), but
/// exe-like formats embed an icon per binary — those cache per full path.
pub fn file_icon_cache_key(path: &Path) -> String {
    const PER_FILE_EXTS: [&str; 3] = ["exe", "lnk", "ico"];
    match path.extension().map(|e| e.to_string_lossy().to_lowercase()) {
        Some(ext) if PER_FILE_EXTS.contains(&ext.as_str()) => {
            format!("path:{}", path.to_string_lossy().to_lowercase())
        }
        Some(ext) => format!("ext:{ext}"),
        None => "ext:".to_string(),
    }
}

/// Extracts the shell-associated icon for a file path (what Explorer shows
/// for that file type) as raw BGRA pixels. Safe to call off the UI thread.
#[cfg(windows)]
fn file_icon_rgba(path: &std::path::Path) -> RawIcon {
    use std::os::windows::ffi::OsStrExt;

    use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL;
    use windows::Win32::UI::Shell::{SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGetFileInfoW};
    use windows::Win32::UI::WindowsAndMessaging::DestroyIcon;
    use windows::core::PCWSTR;

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut sfi = SHFILEINFOW::default();
    let ok = unsafe {
        SHGetFileInfoW(
            PCWSTR(wide.as_ptr()),
            FILE_ATTRIBUTE_NORMAL,
            Some(&mut sfi as *mut _),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        )
    };
    if ok == 0 || sfi.hIcon.is_invalid() {
        return None;
    }
    let decoded = hicon_to_rgba(sfi.hIcon);
    unsafe {
        let _ = DestroyIcon(sfi.hIcon);
    }
    decoded
}

#[cfg(not(windows))]
fn file_icon_rgba(_path: &std::path::Path) -> RawIcon {
    None
}

/// True if any pixel in `rgba` has a non-zero alpha byte.
fn has_any_alpha(rgba: &[u8]) -> bool {
    rgba.chunks_exact(4).any(|px| px[3] != 0)
}

/// BGRA→RGBA swap, alpha fix-up for legacy icons, GPU upload.
fn finish_texture(
    ctx: &egui::Context,
    name: &str,
    mut rgba: Vec<u8>,
    [w, h]: [usize; 2],
) -> egui::TextureHandle {
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
    ctx.load_texture(name, image, egui::TextureOptions::LINEAR)
}

/// Decodes a HICON into top-down BGRA pixels plus its dimensions.
#[cfg(windows)]
fn hicon_to_rgba(
    hicon: windows::Win32::UI::WindowsAndMessaging::HICON,
) -> Option<(Vec<u8>, [usize; 2])> {
    use windows::Win32::Graphics::Gdi::{
        BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC,
        DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetIconInfo, ICONINFO};

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
}

#[cfg(windows)]
fn extract_first_icon_hicon(
    exe_path: &str,
) -> Option<windows::Win32::UI::WindowsAndMessaging::HICON> {
    use windows::Win32::UI::Shell::ExtractIconExW;
    use windows::core::PCWSTR;

    let wide: Vec<u16> = exe_path.encode_utf16().chain(std::iter::once(0)).collect();
    let mut hicon = Default::default();
    let count = unsafe { ExtractIconExW(PCWSTR(wide.as_ptr()), 0, Some(&mut hicon), None, 1) };
    if count == 0 || hicon.is_invalid() {
        return None;
    }
    Some(hicon)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn cache_key_groups_by_extension_but_pins_exe_like_types_to_their_path() {
        assert_eq!(
            file_icon_cache_key(Path::new("C:\\a\\b.txt")),
            file_icon_cache_key(Path::new("D:\\other\\c.txt"))
        );
        assert_eq!(file_icon_cache_key(Path::new("C:\\a\\b.TXT")), "ext:txt");
        assert_ne!(
            file_icon_cache_key(Path::new("C:\\a\\notepad.exe")),
            file_icon_cache_key(Path::new("C:\\b\\notepad.exe"))
        );
        assert_eq!(
            file_icon_cache_key(Path::new("C:\\a\\README")).starts_with("ext:"),
            true
        );
    }
}
