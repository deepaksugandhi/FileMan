use std::fs;
use std::io;
#[cfg(windows)]
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq)]
pub struct FsEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    /// Windows FILE_ATTRIBUTE_ARCHIVE (0x20).
    pub archive: bool,
    /// Windows FILE_ATTRIBUTE_READONLY (0x1).
    pub readonly: bool,
    /// Windows FILE_ATTRIBUTE_HIDDEN (0x2).
    pub hidden: bool,
    /// Windows FILE_ATTRIBUTE_SYSTEM (0x4).
    pub system: bool,
}

const FILE_ATTRIBUTE_READONLY: u32 = 0x1;
const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
const FILE_ATTRIBUTE_ARCHIVE: u32 = 0x20;

pub fn list_dir(dir: &Path) -> io::Result<Vec<FsEntry>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        #[cfg(windows)]
        let attrs = metadata.file_attributes();
        #[cfg(not(windows))]
        let attrs = 0u32;
        entries.push(FsEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            path: entry.path(),
            is_dir: metadata.is_dir(),
            size: metadata.len(),
            modified: metadata.modified().ok(),
            archive: attrs & FILE_ATTRIBUTE_ARCHIVE != 0,
            readonly: attrs & FILE_ATTRIBUTE_READONLY != 0,
            hidden: attrs & FILE_ATTRIBUTE_HIDDEN != 0,
            system: attrs & FILE_ATTRIBUTE_SYSTEM != 0,
        });
    }
    sort_entries(&mut entries, "name", true);
    Ok(entries)
}

/// Sorts entries in place. Directories always come first, sorted
/// alphabetically by name (case-insensitive) regardless of the requested
/// sort column. Files follow the user's chosen sort column.
/// Recognized columns: "name", "modified", "size", "archive".
pub fn sort_entries(entries: &mut [FsEntry], sort_col: &str, asc: bool) {
    entries.sort_by(|a, b| {
        b.is_dir.cmp(&a.is_dir).then_with(|| {
            if a.is_dir {
                // Dirs always sorted alphabetically, always ascending.
                a.name.to_lowercase().cmp(&b.name.to_lowercase())
            } else {
                // Files sorted by the requested column.
                let ord = match sort_col {
                    "modified" => a.modified.cmp(&b.modified),
                    "size" => a.size.cmp(&b.size),
                    "archive" => a.archive.cmp(&b.archive),
                    _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                };
                if asc { ord } else { ord.reverse() }
            }
        })
    });
}

/// Lists only the immediate subdirectories of `dir`, sorted by name.
/// Returns an empty list on errors (e.g. permission denied on network shares)
/// so the sidebar tree can still render the node even if subdirectories
/// can't be enumerated.
pub fn list_subdirs(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    let read_dir = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return Ok(dirs), // graceful degradation
    };
    for entry in read_dir {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if let Ok(ft) = entry.file_type() {
            if ft.is_dir() {
                dirs.push(entry.path());
            }
        }
    }
    dirs.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
    Ok(dirs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    #[test]
    fn lists_dirs_first_then_files_alphabetically() {
        let dir = tempfile::tempdir().unwrap();
        File::create(dir.path().join("b.txt")).unwrap();
        File::create(dir.path().join("a.txt")).unwrap();
        fs::create_dir(dir.path().join("z_folder")).unwrap();

        let entries = list_dir(dir.path()).unwrap();

        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].name, "z_folder");
        assert!(entries[0].is_dir);
        assert_eq!(entries[1].name, "a.txt");
        assert!(!entries[1].is_dir);
        assert_eq!(entries[2].name, "b.txt");
    }

    #[test]
    fn errors_on_missing_dir() {
        let result = list_dir(Path::new("Z:\\definitely_missing_path_xyz"));
        assert!(result.is_err());
    }

    #[test]
    fn list_subdirs_returns_only_directories_sorted() {
        let dir = tempfile::tempdir().unwrap();
        File::create(dir.path().join("f.txt")).unwrap();
        fs::create_dir(dir.path().join("b")).unwrap();
        fs::create_dir(dir.path().join("a")).unwrap();

        let subdirs = list_subdirs(dir.path()).unwrap();

        assert_eq!(subdirs, vec![dir.path().join("a"), dir.path().join("b")]);
    }

    fn entry(name: &str, is_dir: bool, size: u64, modified: Option<SystemTime>) -> FsEntry {
        FsEntry {
            name: name.to_string(),
            path: PathBuf::from(name),
            is_dir,
            size,
            modified,
            archive: false,
            readonly: false,
            hidden: false,
            system: false,
        }
    }

    #[test]
    fn sort_by_size_ascending_and_descending() {
        let mut entries = vec![
            entry("c.txt", false, 30, None),
            entry("a.txt", false, 10, None),
            entry("b.txt", false, 20, None),
        ];

        sort_entries(&mut entries, "size", true);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["a.txt", "b.txt", "c.txt"]);

        sort_entries(&mut entries, "size", false);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["c.txt", "b.txt", "a.txt"]);
    }

    #[test]
    fn sort_by_modified_treats_missing_time_as_oldest() {
        let t1 = SystemTime::UNIX_EPOCH;
        let t2 = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(60);
        let mut entries = vec![
            entry("none.txt", false, 0, None),
            entry("old.txt", false, 0, Some(t1)),
            entry("new.txt", false, 0, Some(t2)),
        ];

        sort_entries(&mut entries, "modified", true);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["none.txt", "old.txt", "new.txt"]);
    }

    #[test]
    fn sort_keeps_directories_first_regardless_of_column() {
        let mut entries = vec![
            entry("zfile.txt", false, 999, None),
            entry("afolder", true, 0, None),
        ];

        sort_entries(&mut entries, "name", true);
        assert!(entries[0].is_dir);

        sort_entries(&mut entries, "size", true);
        assert!(entries[0].is_dir);
    }

    #[test]
    fn sort_by_archive_flag() {
        let mut clean = entry("clean.bin", false, 0, None);
        clean.archive = false;
        let mut dirty = entry("dirty.bin", false, 0, None);
        dirty.archive = true;
        let mut entries = vec![clean, dirty];

        sort_entries(&mut entries, "archive", true);
        assert!(!entries[0].archive);
        assert!(entries[1].archive);

        sort_entries(&mut entries, "archive", false);
        assert!(entries[0].archive);
    }
}
