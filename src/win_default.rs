//! Per-user registration of FileMan as the default folder explorer.
//!
//! Windows resolves a double-clicked folder through the `Directory\shell`
//! and `Folder\shell` verb tables. Writing an `open\command` entry (without
//! the system's `DelegateExecute` value) under `HKCU\Software\Classes`
//! overrides the machine defaults for the current user only — no admin
//! rights required. Deleting those keys restores stock Explorer behaviour.

#[cfg(windows)]
const OVERRIDE_KEYS: [&str; 2] = [
    r"Software\Classes\Directory\shell\open",
    r"Software\Classes\Folder\shell\open",
];

/// Is this FileMan executable currently written into the per-user override?
#[cfg(windows)]
pub fn is_default() -> bool {
    let exe = match std::env::current_exe() {
        Ok(p) => p.to_string_lossy().to_lowercase(),
        Err(_) => return false,
    };
    let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    OVERRIDE_KEYS.iter().any(|key| {
        hkcu.open_subkey(key)
            .and_then(|k| k.get_value::<String, _>(""))
            .map(|cmd| cmd.to_lowercase().contains(&exe))
            .unwrap_or(false)
    })
}

/// Points the per-user folder-open verbs at this executable. `%V` is the
/// parameter Windows substitutes with the clicked folder's path.
#[cfg(windows)]
pub fn set_default() -> std::io::Result<()> {
    use winreg::enums::HKEY_CURRENT_USER;

    let exe = std::env::current_exe()?;
    let cmd = format!("\"{}\" \"%V\"", exe.display());
    let hkcu = winreg::RegKey::predef(HKEY_CURRENT_USER);
    for key in OVERRIDE_KEYS {
        let (key, _) = hkcu.create_subkey(key)?;
        key.set_value("", &cmd)?;
    }
    Ok(())
}

/// Deletes the per-user overrides so Windows Explorer handles folders again.
#[cfg(windows)]
pub fn clear_default() -> std::io::Result<()> {
    use winreg::enums::HKEY_CURRENT_USER;

    let hkcu = winreg::RegKey::predef(HKEY_CURRENT_USER);
    for key in OVERRIDE_KEYS {
        match hkcu.delete_subkey_all(key) {
            Ok(()) => {}
            // Nothing to restore — the override was never written.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Opens the native Windows "Properties" dialog for a file/folder (the same
/// one Explorer's right-click menu shows), via the `properties` shell verb.
#[cfg(windows)]
pub fn show_properties(path: &std::path::Path) {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Shell::{SEE_MASK_INVOKEIDLIST, SHELLEXECUTEINFOW, ShellExecuteExW};
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::PCWSTR;

    let verb: Vec<u16> = "properties\0".encode_utf16().collect();
    let file: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_INVOKEIDLIST,
        hwnd: HWND(std::ptr::null_mut()),
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe {
        let _ = ShellExecuteExW(&mut info);
    }
}

#[cfg(not(windows))]
pub fn show_properties(_path: &std::path::Path) {}

#[cfg(not(windows))]
pub fn is_default() -> bool {
    false
}

#[cfg(not(windows))]
pub fn set_default() -> std::io::Result<()> {
    Ok(())
}

#[cfg(not(windows))]
pub fn clear_default() -> std::io::Result<()> {
    Ok(())
}
