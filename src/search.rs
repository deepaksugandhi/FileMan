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

/// Recursively walks `root` on a background thread, collecting entries whose
/// names match the query. Returns the results via the provided channel.
pub fn recursive_search(
    root: PathBuf,
    query: String,
    tx: std::sync::mpsc::Sender<Vec<FsEntry>>,
) {
    let results = walk_recursive(&root, &query);
    let _ = tx.send(results);
}

fn walk_recursive(dir: &std::path::Path, query: &str) -> Vec<FsEntry> {
    let mut results = Vec::new();
    let q = query.to_lowercase();
    if let Ok(entries) = crate::fs_entry::list_dir(dir) {
        for entry in &entries {
            if entry.is_dir {
                if entry.name.to_lowercase().contains(&q) {
                    results.push(entry.clone());
                }
                results.extend(walk_recursive(&entry.path, query));
            } else if entry.name.to_lowercase().contains(&q) {
                results.push(entry.clone());
            }
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

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
