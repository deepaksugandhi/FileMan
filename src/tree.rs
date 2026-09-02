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

/// What kind of storage a drive root is, per `GetDriveTypeW` — distinguishes
/// removable media (USB sticks, SD cards, external drives without a fixed
/// interface) from fixed disks so the sidebar can group them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveKind {
    Removable,
    Fixed,
    Network,
    CdRom,
    Other,
}

/// Classifies a drive root (e.g. `E:\`) via `GetDriveTypeW`.
pub fn drive_kind(path: &Path) -> DriveKind {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::Win32::Storage::FileSystem::GetDriveTypeW;
        use windows::core::PCWSTR;
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        return match unsafe { GetDriveTypeW(PCWSTR(wide.as_ptr())) } {
            2 => DriveKind::Removable,
            3 => DriveKind::Fixed,
            4 => DriveKind::Network,
            5 => DriveKind::CdRom,
            _ => DriveKind::Other,
        };
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        DriveKind::Other
    }
}

/// The volume label for a drive root (e.g. "Data"), via
/// `GetVolumeInformationW`. `None` if the drive has no label or can't be
/// read (e.g. an empty card reader slot).
pub fn volume_label(path: &Path) -> Option<String> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::Win32::Storage::FileSystem::GetVolumeInformationW;
        use windows::core::PCWSTR;
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut name_buf = [0u16; 128];
        let ok = unsafe {
            GetVolumeInformationW(
                PCWSTR(wide.as_ptr()),
                Some(&mut name_buf),
                None,
                None,
                None,
                None,
            )
        };
        if ok.is_ok() {
            let len = name_buf.iter().position(|&c| c == 0).unwrap_or(0);
            let label = String::from_utf16_lossy(&name_buf[..len]);
            if !label.is_empty() {
                return Some(label);
            }
        }
        None
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        None
    }
}

/// Attempts to safely eject a removable drive: lock the volume, dismount it,
/// then eject the physical media — the same sequence Explorer's "Eject"
/// menu item performs. Returns a human-readable error on failure (most
/// commonly because a file on the drive is still open somewhere).
pub fn eject_drive(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE};
        use windows::Win32::Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        };
        use windows::Win32::System::Ioctl::{
            FSCTL_DISMOUNT_VOLUME, FSCTL_LOCK_VOLUME, IOCTL_STORAGE_EJECT_MEDIA,
        };
        use windows::Win32::System::IO::DeviceIoControl;
        use windows::core::PCWSTR;

        // Volume handles use `\\.\E:` (no trailing backslash), not the
        // `E:\` root path used everywhere else in this app.
        let drive_letter = path
            .to_string_lossy()
            .chars()
            .next()
            .ok_or_else(|| "Not a drive".to_string())?;
        let wide: Vec<u16> = format!("\\\\.\\{drive_letter}:")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        unsafe {
            let handle = CreateFileW(
                PCWSTR(wide.as_ptr()),
                (GENERIC_READ | GENERIC_WRITE).0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
            .map_err(|_| "Couldn't open the drive".to_string())?;

            let lock_ok =
                DeviceIoControl(handle, FSCTL_LOCK_VOLUME, None, 0, None, 0, None, None).is_ok();
            if !lock_ok {
                let _ = CloseHandle(handle);
                return Err(
                    "Drive is busy — close any open files on it and try again".to_string()
                );
            }
            let _ = DeviceIoControl(handle, FSCTL_DISMOUNT_VOLUME, None, 0, None, 0, None, None);
            let eject_ok = DeviceIoControl(
                handle,
                IOCTL_STORAGE_EJECT_MEDIA,
                None,
                0,
                None,
                0,
                None,
                None,
            )
            .is_ok();
            let _ = CloseHandle(handle);
            if eject_ok {
                Ok(())
            } else {
                Err("Dismounted — it's safe to unplug now".to_string())
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err("Eject is only supported on Windows".to_string())
    }
}

/// The user's shell-known folders (Desktop, Documents, Downloads, …) as
/// `(label, path)` pairs, resolved through `SHGetKnownFolderPath` so
/// redirected folders (OneDrive, custom locations) resolve to where they
/// really live. Only folders that actually exist are returned.
pub fn list_system_folders() -> Vec<(String, PathBuf)> {
    #[cfg(windows)]
    {
        use windows::Win32::System::Com::CoTaskMemFree;
        use windows::Win32::UI::Shell::{
            FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Music,
            FOLDERID_Pictures, FOLDERID_Videos, KF_FLAG_DEFAULT, SHGetKnownFolderPath,
        };

        let folders: [(&str, windows::core::GUID); 6] = [
            ("Desktop", FOLDERID_Desktop),
            ("Documents", FOLDERID_Documents),
            ("Downloads", FOLDERID_Downloads),
            ("Music", FOLDERID_Music),
            ("Pictures", FOLDERID_Pictures),
            ("Videos", FOLDERID_Videos),
        ];

        let mut out = Vec::new();
        for (label, guid) in folders {
            let Ok(pwstr) = (unsafe { SHGetKnownFolderPath(&guid, KF_FLAG_DEFAULT, None) })
            else {
                continue;
            };
            let mut len = 0usize;
            unsafe {
                while *pwstr.0.add(len) != 0 {
                    len += 1;
                }
            }
            let slice = unsafe { std::slice::from_raw_parts(pwstr.0, len) };
            let path = PathBuf::from(String::from_utf16_lossy(slice));
            unsafe { CoTaskMemFree(Some(pwstr.0.cast())) };
            if path.is_dir() {
                out.push((label.to_string(), path));
            }
        }
        out
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// Returns discovered network server paths (UNC `\\server` entries) by
/// enumerating the Windows network resource tree via `WNetEnumResourceW`.
/// Returns an empty vec on platforms where the API isn't available or on error.
pub fn list_network_servers() -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::*;
        use windows::Win32::NetworkManagement::WNet::*;
        use windows::Win32::System::Com::*;

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
        Prefix::UNC(_, _) | Prefix::VerbatimUNC(_, _) => Some(PathBuf::from(format!(
            "{}\\",
            prefix.as_os_str().to_string_lossy()
        ))),
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
