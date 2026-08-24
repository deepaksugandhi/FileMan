use std::path::{Component, Path, PathBuf, Prefix};

// ponytail: brute-force A-Z scan (26 stat calls) instead of the
// GetLogicalDrives Win32 bitmask API — simple and fast enough for a one-shot
// sidebar populate. Switch to the Win32 call if this ever shows up in profiling.
pub fn list_drives() -> Vec<PathBuf> {
    (b'A'..=b'Z')
        .filter_map(|letter| {
            let path = PathBuf::from(format!("{}:\\", letter as char));
            if path.exists() { Some(path) } else { None }
        })
        .collect()
}

/// Returns discovered network server paths (UNC `\\server` entries) by
/// enumerating the Windows network resource tree via `WNetEnumResourceW`.
/// Returns an empty vec on platforms where the API isn't available or on error.
pub fn list_network_servers() -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        use windows::Win32::NetworkManagement::WNet::*;
        use windows::Win32::System::Com::*;
        use windows::Win32::Foundation::*;

        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }

        let mut handle = HANDLE::default();
        let result = unsafe {
            WNetOpenEnumW(
                RESOURCE_GLOBALNET,
                RESOURCETYPE_DISK,
                WNET_OPEN_ENUM_USAGE(0),
                None,
                &mut handle,
            )
        };
        if result.is_err() {
            return Vec::new();
        }

        let mut servers = Vec::new();
        // 16 KB buffer for enumeration results
        let mut buffer = [0u32; 4096];
        let mut entries: u32 = 1024;

        loop {
            let mut size = (buffer.len() * 4) as u32;
            let enum_result = unsafe {
                WNetEnumResourceW(
                    handle,
                    &mut entries,
                    buffer.as_mut_ptr() as *mut _,
                    &mut size,
                )
            };
            if enum_result.is_err() {
                break;
            }

            let item_count = entries as usize;
            let _item_size = std::mem::size_of::<NETRESOURCEW>();
            for i in 0..item_count {
                let ptr = buffer.as_ptr() as *const NETRESOURCEW;
                let item = unsafe { ptr.add(i).read() };
                if !item.lpRemoteName.is_null() {
                    let name = unsafe {
                        let ptr = item.lpRemoteName.0;
                        let len = (0..).take_while(|&i| *ptr.add(i) != 0).count();
                        let slice = std::slice::from_raw_parts(ptr, len);
                        String::from_utf16_lossy(slice)
                    };
                    if name.starts_with("\\\\") {
                        let path = PathBuf::from(&name);
                        if !servers.contains(&path) {
                            servers.push(path);
                        }
                    }
                }
            }
            if entries == 0 {
                break;
            }
        }

        unsafe {
            let _ = WNetCloseEnum(handle);
        }

        servers
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// Returns the UNC share root (`\\server\share\`) that `path` lives under,
/// or `None` if `path` isn't a UNC path. `list_network_servers`'s
/// network-neighborhood browsing is best-effort and often finds nothing on
/// modern SMB networks (NetBIOS browsing is deprecated), so a path the user
/// actually navigated to (e.g. via the address bar) may never appear as a
/// discovered server. This lets the sidebar tree show/expand to it anyway.
pub fn unc_share_root(path: &Path) -> Option<PathBuf> {
    let Some(Component::Prefix(prefix)) = path.components().next() else {
        return None;
    };
    match prefix.kind() {
        Prefix::UNC(_, _) | Prefix::VerbatimUNC(_, _) => {
            Some(PathBuf::from(format!("{}\\", prefix.as_os_str().to_string_lossy())))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn includes_c_drive() {
        let drives = list_drives();
        assert!(drives.contains(&PathBuf::from("C:\\")));
    }
}
