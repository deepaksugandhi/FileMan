use crate::fs_entry::FsEntry;
use std::path::PathBuf;

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

pub const RESULT_LIMIT: usize = 10_000;
const QUEUE_LIMIT: usize = 256;

pub struct SearchJob {
    rx: mpsc::Receiver<FsEntry>,
    cancelled: Arc<AtomicBool>,
}

impl SearchJob {
    pub fn start(root: PathBuf, query: String) -> Self {
        let (tx, rx) = mpsc::sync_channel(QUEUE_LIMIT);
        let cancelled = Arc::new(AtomicBool::new(false));
        let signal = cancelled.clone();
        std::thread::spawn(move || {
            let mut remaining = RESULT_LIMIT;
            walk_recursive(&root, &query.to_lowercase(), &tx, &signal, &mut remaining);
        });
        Self { rx, cancelled }
    }

    pub fn try_recv(&self) -> Result<FsEntry, mpsc::TryRecvError> {
        self.rx.try_recv()
    }
}

impl Drop for SearchJob {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
        // Dropping rx also wakes a producer blocked on the bounded queue.
    }
}

fn walk_recursive(
    dir: &std::path::Path,
    query: &str,
    tx: &mpsc::SyncSender<FsEntry>,
    cancelled: &AtomicBool,
    remaining: &mut usize,
) -> bool {
    if cancelled.load(Ordering::Relaxed) || *remaining == 0 {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return true;
    };
    for item in entries {
        if cancelled.load(Ordering::Relaxed) {
            return false;
        }
        let Ok(item) = item else {
            continue;
        };
        // Do not follow symlinks or Windows junctions into cycles/outside root.
        let mut follow = item
            .file_type()
            .is_ok_and(|t| t.is_dir() && !t.is_symlink());
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            follow &= item
                .metadata()
                .is_ok_and(|m| m.file_attributes() & 0x400 == 0);
        }
        let Ok(entry) = crate::fs_entry::read_entry(item) else {
            continue;
        };
        let child = follow.then(|| entry.path.clone());
        if entry.name.to_lowercase().contains(query) {
            if tx.send(entry).is_err() {
                return false;
            }
            *remaining -= 1;
            if *remaining == 0 {
                return false;
            }
        }
        if let Some(child) = child {
            if !walk_recursive(&child, query, tx, cancelled, remaining) {
                return false;
            }
        }
    }
    true
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

        let job = SearchJob::start(dir.path().to_path_buf(), "needle".into());
        let mut names: Vec<String> = Vec::new();
        while let Ok(entry) = job.rx.recv_timeout(std::time::Duration::from_secs(5)) {
            names.push(entry.name);
        }
        names.sort();
        assert_eq!(names, vec!["needle.txt", "other_needle.log"]);
    }

    #[test]
    fn cancelled_and_limited_searches_stop_and_receiver_drop_unblocks() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..20 {
            std::fs::write(dir.path().join(format!("hit{i}")), b"").unwrap();
        }
        let (tx, rx) = mpsc::sync_channel(2);
        let mut remaining = 2;
        assert!(!walk_recursive(
            dir.path(),
            "hit",
            &tx,
            &AtomicBool::new(false),
            &mut remaining
        ));
        assert_eq!(remaining, 0);
        assert_eq!(rx.try_iter().count(), 2);
        let mut remaining = 20;
        assert!(!walk_recursive(
            dir.path(),
            "missing",
            &tx,
            &AtomicBool::new(true),
            &mut remaining
        ));
        assert_eq!(remaining, 20);
        drop(rx);
        assert!(!walk_recursive(
            dir.path(),
            "hit",
            &tx,
            &AtomicBool::new(false),
            &mut remaining
        ));
        let job = SearchJob::start(dir.path().into(), "hit".into());
        let signal = job.cancelled.clone();
        drop(job);
        assert!(signal.load(Ordering::Relaxed));
    }
}
