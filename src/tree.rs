use std::path::PathBuf;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn includes_c_drive() {
        let drives = list_drives();
        assert!(drives.contains(&PathBuf::from("C:\\")));
    }
}
