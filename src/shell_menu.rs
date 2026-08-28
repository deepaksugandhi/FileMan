//! Windows Explorer shell context menu integration.
//!
//! Queries the real shell `IContextMenu` for a file or folder and surfaces
//! those items in our egui right-click menu. When the user picks one, we
//! invoke the shell verb directly.

/// A single item from the Windows Explorer context menu.
#[derive(Debug, Clone)]
pub struct ShellMenuItem {
    pub label: String,
    pub id: u32,
    pub disabled: bool,
    pub separator: bool,
    pub sub_items: Vec<ShellMenuItem>,
}

#[cfg(windows)]
mod win {
    use super::*;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{
        Common::ITEMIDLIST, CMINVOKECOMMANDINFO, IContextMenu, IShellFolder, SHGetDesktopFolder,
        SHParseDisplayName, GCS_VERBW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreatePopupMenu, DestroyMenu, GetMenuItemCount, GetMenuItemInfoW, HMENU,
        MENU_ITEM_STATE, MENU_ITEM_TYPE, MENUITEMINFOW, MFS_DISABLED, MFT_OWNERDRAW,
        MFT_SEPARATOR, MIIM_ID, MIIM_STATE, MIIM_STRING, MIIM_SUBMENU,
    };

    /// Collects shell context menu items covering every path in `paths` (all
    /// must be siblings in the same folder — true for a multi-selection in
    /// one pane), so entries like "Combine files in Foxit PDF" that need the
    /// whole selection actually see it.
    pub fn query_items(paths: &[std::path::PathBuf]) -> Vec<ShellMenuItem> {
        match unsafe { query_items_inner(paths) } {
            Some(items) => items,
            None => {
                eprintln!("[shell_menu] query_items returned None for {paths:?}");
                Vec::new()
            }
        }
    }

    /// Invokes a shell command on every path in `paths` by its numeric menu id.
    pub fn invoke(hwnd: HWND, paths: &[std::path::PathBuf], id: u32) {
        unsafe {
            let _ = invoke_inner(hwnd, paths, id);
        }
    }

    // ── internals ─────────────────────────────────────────────────────

    /// Binds `paths`' shared parent folder once, then resolves each path to
    /// a child PIDL relative to that same parent — the shape
    /// `IShellFolder::GetUIObjectOf` needs to build one `IContextMenu`
    /// spanning multiple items, instead of one PIDL per path from
    /// independent (and not necessarily interchangeable) parent bindings.
    unsafe fn bind_children(
        paths: &[std::path::PathBuf],
    ) -> Option<(IShellFolder, Vec<*mut ITEMIDLIST>)> {
        unsafe {
            let dir = paths.first()?.parent()?;
            let dir_wide: Vec<u16> = dir
                .to_string_lossy()
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let mut pidl_dir: *mut ITEMIDLIST = std::ptr::null_mut();
            SHParseDisplayName(
                windows::core::PCWSTR::from_raw(dir_wide.as_ptr()),
                None,
                &mut pidl_dir,
                0,
                None,
            )
            .ok()?;
            if pidl_dir.is_null() {
                return None;
            }
            let desktop: IShellFolder = SHGetDesktopFolder().ok()?;
            let parent: IShellFolder = desktop.BindToObject(pidl_dir, None).ok()?;
            CoTaskMemFree(Some(pidl_dir.cast()));

            let mut children: Vec<*mut ITEMIDLIST> = Vec::with_capacity(paths.len());
            for path in paths {
                let name = path.file_name()?.to_string_lossy();
                let name_wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
                let mut child: *mut ITEMIDLIST = std::ptr::null_mut();
                if parent
                    .ParseDisplayName(
                        HWND::default(),
                        None,
                        windows::core::PCWSTR::from_raw(name_wide.as_ptr()),
                        None,
                        &mut child,
                        std::ptr::null_mut(),
                    )
                    .is_err()
                    || child.is_null()
                {
                    for c in children {
                        CoTaskMemFree(Some(c.cast()));
                    }
                    return None;
                }
                children.push(child);
            }
            Some((parent, children))
        }
    }

    unsafe fn query_items_inner(paths: &[std::path::PathBuf]) -> Option<Vec<ShellMenuItem>> {
        unsafe {
            let (parent, children) = bind_children(paths)?;
            let refs: Vec<*const ITEMIDLIST> =
                children.iter().map(|c| *c as *const ITEMIDLIST).collect();

            let ctx_menu: IContextMenu =
                match parent.GetUIObjectOf(HWND::default(), &refs, None) {
                    Ok(m) => m,
                    Err(e) => {
                        eprintln!("[shell_menu] GetUIObjectOf failed: {e}");
                        for c in children {
                            CoTaskMemFree(Some(c.cast()));
                        }
                        return None;
                    }
                };

            let hmenu = match CreatePopupMenu() {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("[shell_menu] CreatePopupMenu failed: {e}");
                    return None;
                }
            };
            let id_first: u32 = 0x8000;
            let id_last: u32 = 0xBFFF;
            // CMF_EXPLORE (0x4): full Explorer-style menu. The previous
            // value (0x1 = CMF_DEFAULTONLY) restricted the shell to
            // returning only the single default verb, so almost nothing
            // showed up in the submenu.
            let hr = ctx_menu.QueryContextMenu(hmenu, 0, id_first, id_last, 0x00000004);
            eprintln!("[shell_menu] QueryContextMenu hr={hr:?}");

            let items = enumerate_menu(hmenu, &ctx_menu);
            eprintln!("[shell_menu] enumerate_menu returned {} items", items.len());

            let _ = DestroyMenu(hmenu);
            for c in children {
                CoTaskMemFree(Some(c.cast()));
            }

            Some(items)
        }
    }

    fn enumerate_menu(hmenu: HMENU, ctx_menu: &IContextMenu) -> Vec<ShellMenuItem> {
        let count = unsafe { GetMenuItemCount(Some(hmenu)) };
        if count < 0 {
            return Vec::new();
        }

        let mut items = Vec::new();
        // Some shell extensions register under more than one registry path
        // (e.g. a wildcard `*` handler plus a per-extension one) and each
        // registration adds its own identical-looking item, so the same
        // label can legitimately come back twice. Keep only the first.
        let mut seen_labels = std::collections::HashSet::new();
        for i in 0..count {
            let mut info = unsafe {
                MENUITEMINFOW {
                    cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
                    fMask: MIIM_ID | MIIM_STATE | MIIM_STRING | MIIM_SUBMENU,
                    ..std::mem::zeroed()
                }
            };

            let mut buf = [0u16; 260];
            info.dwTypeData = windows::core::PWSTR(buf.as_mut_ptr());
            info.cch = buf.len() as u32;

            if unsafe { GetMenuItemInfoW(hmenu, i as u32, true, &mut info) }.is_ok() {
                let id = info.wID;

                if info.fType & MFT_OWNERDRAW != MENU_ITEM_TYPE(0) {
                    continue;
                }

                if info.fType & MFT_SEPARATOR != MENU_ITEM_TYPE(0) {
                    items.push(ShellMenuItem {
                        label: String::new(),
                        id,
                        disabled: false,
                        separator: true,
                        sub_items: Vec::new(),
                    });
                    continue;
                }

                let label = String::from_utf16_lossy(
                    &buf[..buf.iter().position(|&c| c == 0).unwrap_or(buf.len())],
                );
                if !seen_labels.insert(label.clone()) {
                    continue;
                }
                let disabled = info.fState & MFS_DISABLED != MENU_ITEM_STATE(0);

                let sub_items = if !info.hSubMenu.is_invalid() {
                    enumerate_menu(info.hSubMenu, ctx_menu)
                } else {
                    Vec::new()
                };

                items.push(ShellMenuItem {
                    label,
                    id,
                    disabled,
                    separator: false,
                    sub_items,
                });
            }
        }
        items
    }

    unsafe fn invoke_inner(hwnd: HWND, paths: &[std::path::PathBuf], id: u32) -> Option<()> {
        unsafe {
            let (parent, children) = bind_children(paths)?;
            let refs: Vec<*const ITEMIDLIST> =
                children.iter().map(|c| *c as *const ITEMIDLIST).collect();

            let ctx_menu: IContextMenu = match parent.GetUIObjectOf(HWND::default(), &refs, None) {
                Ok(m) => m,
                Err(_) => {
                    for c in children {
                        CoTaskMemFree(Some(c.cast()));
                    }
                    return None;
                }
            };

            let hmenu = match CreatePopupMenu() {
                Ok(m) => m,
                Err(_) => {
                    for c in children {
                        CoTaskMemFree(Some(c.cast()));
                    }
                    return None;
                }
            };
            let id_first: u32 = 0x8000;
            let _ =
                ctx_menu.QueryContextMenu(hmenu, 0, id_first, 0xBFFF, 0x00000004);

            let offset = (id - id_first) as usize;
            let verb_wide = get_verb_wide(&ctx_menu, offset);

            let verb_ansi: Vec<u8> = verb_wide
                .as_deref()
                .map(|w| {
                    w.encode_utf16()
                        .flat_map(|c| {
                            let b = (c & 0xFF) as u8;
                            if b == 0 { vec![] } else { vec![b] }
                        })
                        .chain(std::iter::once(0))
                        .collect()
                })
                .unwrap_or_default();

            let verb_ptr = if !verb_ansi.is_empty() {
                windows::core::PCSTR::from_raw(verb_ansi.as_ptr())
            } else {
                windows::core::PCSTR(offset as *const u8)
            };

            let cmi = CMINVOKECOMMANDINFO {
                cbSize: std::mem::size_of::<CMINVOKECOMMANDINFO>() as u32,
                fMask: 0,
                hwnd,
                lpVerb: verb_ptr,
                lpParameters: windows::core::PCSTR::null(),
                lpDirectory: windows::core::PCSTR::null(),
                nShow: 1,
                dwHotKey: 0,
                hIcon: windows::Win32::Foundation::HANDLE(std::ptr::null_mut()),
            };

            let _ = ctx_menu.InvokeCommand(&cmi);
            let _ = DestroyMenu(hmenu);
            for c in children {
                CoTaskMemFree(Some(c.cast()));
            }

            Some(())
        }
    }

    unsafe fn get_verb_wide(ctx_menu: &IContextMenu, offset: usize) -> Option<String> {
        unsafe {
            let mut buf = [0u16; 260];
            ctx_menu
                .GetCommandString(
                    offset,
                    GCS_VERBW,
                    None,
                    windows::core::PSTR(buf.as_mut_ptr() as *mut u8),
                    buf.len() as u32,
                )
                .ok()?;
            let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            Some(String::from_utf16_lossy(&buf[..len]))
        }
    }

}

#[cfg(not(windows))]
mod win {
    use super::*;
    pub fn query_items(_paths: &[std::path::PathBuf]) -> Vec<ShellMenuItem> {
        Vec::new()
    }
    pub fn invoke(_hwnd: (), _paths: &[std::path::PathBuf], _id: u32) {}
}

pub use win::{invoke, query_items};
