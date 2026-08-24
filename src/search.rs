use crate::fs_entry::FsEntry;
use std::path::PathBuf;

/// Filters entries in-place by a substring match on the name. Applies to
/// both files and folders alike.
pub fn filter_entries(entries: &mut Vec<FsEntry>, query: &str) {
    if query.is_empty() {
        return;
    }
    let q = query.to_lowercase();
    entries.retain(|e| e.name.to_lowercase().contains(&q));
}

/// Recursively walks `root` on a background thread, sending every entry
/// whose name matches the query through `tx` as soon as it is found, so the
/// UI can show results progressively instead of waiting for the whole walk.
/// The channel closing signals completion.
pub fn recursive_search(
    root: PathBuf,
    query: String,
    tx: std::sync::mpsc::Sender<FsEntry>,
) {
    walk_recursive(&root, &query, &tx);
}

fn walk_recursive(dir: &std::path::Path, query: &str, tx: &std::sync::mpsc::Sender<FsEntry>) {
    let q = query.to_lowercase();
    if let Ok(entries) = crate::fs_entry::list_dir(dir) {
        for entry in entries {
            if entry.is_dir {
                if entry.name.to_lowercase().contains(&q) {
                    let _ = tx.send(entry.clone());
                }
                walk_recursive(&entry.path, query, tx);
            } else if entry.name.to_lowercase().contains(&q) {
                let _ = tx.send(entry);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn recursive_search_streams_matching_entries_from_nested_dirs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("needle.txt"), b"x").unwrap();
        std::fs::write(dir.path().join("sub").join("other_needle.log"), b"x").unwrap();
        std::fs::write(dir.path().join("unrelated.bin"), b"x").unwrap();

        let (tx, rx) = mpsc::channel();
        recursive_search(dir.path().to_path_buf(), "needle".into(), tx);

        let mut names: Vec<String> = Vec::new();
        while let Ok(entry) = rx.recv_timeout(std::time::Duration::from_secs(5)) {
            names.push(entry.name);
        }
        names.sort();
        assert_eq!(names, vec!["needle.txt", "other_needle.log"]);
    }

    fn entry(name: &str, is_dir: bool) -> FsEntry {
        FsEntry {
            name: name.to_string(),
            path: PathBuf::from(name),
            is_dir,
            size: 0,
            modified: None,
            archive: false,
        }
    }

    #[test]
    fn filter_removes_non_matching_files_and_folders() {
        let mut entries = vec![
            entry("readme.txt", false),
            entry("data.csv", false),
            entry("src", true),
        ];
        filter_entries(&mut entries, "readme");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "readme.txt");
    }

    #[test]
    fn filter_keeps_matching_folders() {
        let mut entries = vec![entry("readme.txt", false), entry("resources", true)];
        filter_entries(&mut entries, "res");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "resources");
        assert!(entries[0].is_dir);
    }

    #[test]
    fn filter_is_case_insensitive() {
        let mut entries = vec![
            entry("MyFile.txt", false),
            entry("other.txt", false),
        ];
        filter_entries(&mut entries, "myfile");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "MyFile.txt");
    }

    #[test]
    fn empty_filter_keeps_all() {
        let mut entries = vec![entry("a.txt", false), entry("b.txt", false)];
        filter_entries(&mut entries, "");
        assert_eq!(entries.len(), 2);
    }
}
