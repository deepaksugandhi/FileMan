use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

/// Progress update sent from a background file operation.
#[derive(Debug, Clone)]
pub struct ProgressUpdate {
    pub files_done: u64,
    pub files_total: u64,
    pub current_file: String,
}

/// Status of a background operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpStatus {
    #[allow(dead_code)]
    Idle,
    Running,
    Completed(String),
    Failed(String),
}

/// Tracks a background copy/move/delete operation.
pub struct BackgroundOp {
    pub rx: mpsc::Receiver<ProgressUpdate>,
    pub status_rx: mpsc::Receiver<OpStatus>,
    pub progress: ProgressUpdate,
    pub status: OpStatus,
}

impl BackgroundOp {
    /// Polls for updates without blocking. Returns true if still running.
    pub fn poll(&mut self) -> bool {
        while let Ok(update) = self.rx.try_recv() {
            self.progress = update;
        }
        while let Ok(status) = self.status_rx.try_recv() {
            self.status = status;
        }
        self.status == OpStatus::Running
    }
}

/// Counts files recursively under `dir`.
fn count_files(dir: &Path) -> u64 {
    let mut count = 0;
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                count += count_files(&entry.path());
            } else {
                count += 1;
            }
        }
    }
    count
}

/// Counts a single item: 1 for a file, recursive count for a directory.
fn count_item(path: &Path) -> u64 {
    if path.is_dir() { count_files(path) } else { 1 }
}

/// One unit of a background copy/move batch, carrying the per-item conflict
/// resolution the caller already got from the user (overwrite the existing
/// destination, or land under a fresh name instead).
#[derive(Debug, Clone)]
pub struct TransferItem {
    pub src: PathBuf,
    /// Name the item gets in the destination folder; `None` keeps the
    /// source's own file name.
    pub dest_name: Option<String>,
    /// True when an existing item of the same name in the destination may
    /// be replaced.
    pub overwrite: bool,
}

impl TransferItem {
    /// A plain transfer: keep the source name, never overwrite.
    pub fn plain(src: PathBuf) -> Self {
        TransferItem {
            src,
            dest_name: None,
            overwrite: false,
        }
    }

    /// The exact destination path of this item inside `dest_dir`.
    fn dest_path(&self, dest_dir: &Path) -> Option<PathBuf> {
        match &self.dest_name {
            Some(name) => Some(dest_dir.join(name)),
            None => self.src.file_name().map(|n| dest_dir.join(n)),
        }
    }
}

/// Copies each item in `items` (file or directory) into `dest_dir` on a
/// background thread, sending progress updates via channels.
///
/// The file count used for the progress bar is computed *inside* the
/// background thread rather than by the caller: for a large tree, counting
/// is itself a non-trivial recursive walk, and doing it on the UI thread
/// before spawning would just move the freeze earlier instead of removing
/// it.
pub fn copy_items_bg(items: Vec<TransferItem>, dest_dir: PathBuf) -> BackgroundOp {
    let (progress_tx, progress_rx) = mpsc::channel();
    let (status_tx, status_rx) = mpsc::channel();
    let _ = progress_tx.send(ProgressUpdate {
        files_done: 0,
        files_total: 0,
        current_file: "Counting…".to_string(),
    });
    let _ = status_tx.send(OpStatus::Running);

    thread::spawn(move || {
        let total: u64 = items.iter().map(|t| count_item(&t.src)).sum();
        let mut done = 0;
        let mut errors = Vec::new();
        for item in &items {
            match copy_item_transfer(item, &dest_dir, &progress_tx, done, total) {
                Ok(new_done) => done = new_done,
                Err(e) => errors.push(format!("{}: {e}", item.src.display())),
            }
        }
        let _ = status_tx.send(if errors.is_empty() {
            OpStatus::Completed(format!("Copied into {}", dest_dir.display()))
        } else {
            OpStatus::Failed(errors.join("\n"))
        });
    });

    BackgroundOp {
        rx: progress_rx,
        status_rx,
        progress: ProgressUpdate {
            files_done: 0,
            files_total: 0,
            current_file: "Counting…".to_string(),
        },
        status: OpStatus::Running,
    }
}

/// Transfers one top-level item into `dest_dir`, applying its conflict
/// resolution: an `overwrite` item removes whatever occupies its
/// destination first; a `dest_name` item lands under that fresh name.
fn copy_item_transfer(
    item: &TransferItem,
    dest_dir: &Path,
    progress: &mpsc::Sender<ProgressUpdate>,
    done: u64,
    total: u64,
) -> io::Result<u64> {
    let dest = item.dest_path(dest_dir).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "source has no file name")
    })?;
    let name = dest
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let _ = progress.send(ProgressUpdate {
        files_done: done,
        files_total: total,
        current_file: name,
    });
    if item.overwrite && dest.exists() {
        crate::fs_ops::delete_permanently(&dest)?;
    }
    copy_tree_into(&item.src, &dest, progress, done, total)
}

/// Copies `src` (file or directory) onto the exact `dest` path — `dest`
/// is the item's own name inside its parent, not a containing folder.
/// Refuses to clobber anything that is still in the way.
fn copy_tree_into(
    src: &Path,
    dest: &Path,
    progress: &mpsc::Sender<ProgressUpdate>,
    done: u64,
    total: u64,
) -> io::Result<u64> {
    if src.is_dir() {
        fs::create_dir_all(dest)?;
        let mut done = done;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            done = copy_tree_into(
                &entry.path(),
                &dest.join(entry.file_name()),
                progress,
                done,
                total,
            )?;
        }
        Ok(done)
    } else {
        if dest.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("destination already exists: {}", dest.display()),
            ));
        }
        fs::copy(src, dest)?;
        let new_done = done + 1;
        let _ = progress.send(ProgressUpdate {
            files_done: new_done,
            files_total: total,
            current_file: dest
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        });
        Ok(new_done)
    }
}

/// Moves each item in `items` (file or directory) into `dest_dir` on a
/// background thread. Same rationale as `copy_items_bg` for counting inside
/// the thread rather than blocking the caller first.
pub fn move_items_bg(items: Vec<TransferItem>, dest_dir: PathBuf) -> BackgroundOp {
    let (progress_tx, progress_rx) = mpsc::channel();
    let (status_tx, status_rx) = mpsc::channel();
    let _ = progress_tx.send(ProgressUpdate {
        files_done: 0,
        files_total: 0,
        current_file: "Counting…".to_string(),
    });
    let _ = status_tx.send(OpStatus::Running);

    thread::spawn(move || {
        let total: u64 = items.iter().map(|t| count_item(&t.src)).sum();
        let mut done = 0;
        let mut errors = Vec::new();
        for item in &items {
            match move_item_transfer(item, &dest_dir, &progress_tx, done, total) {
                Ok(new_done) => done = new_done,
                Err(e) => errors.push(format!("{}: {e}", item.src.display())),
            }
        }
        let _ = status_tx.send(if errors.is_empty() {
            OpStatus::Completed(format!("Moved into {}", dest_dir.display()))
        } else {
            OpStatus::Failed(errors.join("\n"))
        });
    });

    BackgroundOp {
        rx: progress_rx,
        status_rx,
        progress: ProgressUpdate {
            files_done: 0,
            files_total: 0,
            current_file: "Counting…".to_string(),
        },
        status: OpStatus::Running,
    }
}

fn move_item_transfer(
    item: &TransferItem,
    dest_dir: &Path,
    progress: &mpsc::Sender<ProgressUpdate>,
    done: u64,
    total: u64,
) -> io::Result<u64> {
    let dest = item.dest_path(dest_dir).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "source has no file name")
    })?;
    if item.overwrite && dest.exists() {
        crate::fs_ops::delete_permanently(&dest)?;
    }
    if dest.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("destination already exists: {}", dest.display()),
        ));
    }
    // Try cheap rename first.
    if fs::rename(&item.src, &dest).is_ok() {
        let new_done = done + count_item(&dest);
        let _ = progress.send(ProgressUpdate {
            files_done: new_done,
            files_total: total,
            current_file: String::new(),
        });
        return Ok(new_done);
    }
    // Cross-volume: copy + delete.
    let new_done = copy_tree_into(&item.src, &dest, progress, done, total)?;
    fs::remove_dir_all(&item.src).or_else(|_| fs::remove_file(&item.src))?;
    Ok(new_done)
}

/// Deletes a list of paths to the Recycle Bin on a background thread.
pub fn delete_to_trash_bg(paths: Vec<PathBuf>) -> BackgroundOp {
    let (progress_tx, progress_rx) = mpsc::channel();
    let (status_tx, status_rx) = mpsc::channel();
    let total = paths.len() as u64;
    let _ = progress_tx.send(ProgressUpdate {
        files_done: 0,
        files_total: total,
        current_file: String::new(),
    });
    let _ = status_tx.send(OpStatus::Running);

    thread::spawn(move || {
        let mut errors = Vec::new();
        let mut permanent_count = 0u64;
        for (i, path) in paths.iter().enumerate() {
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let _ = progress_tx.send(ProgressUpdate {
                files_done: i as u64,
                files_total: total,
                current_file: name,
            });
            match trash::delete(path) {
                Ok(()) => {}
                Err(_) => {
                    // Fallback to permanent delete (network/UNC paths have
                    // no Recycle Bin). Must handle folders too: removing a
                    // directory with `remove_file` fails with a misleading
                    // "Access is denied (os error 5)".
                    match crate::fs_ops::delete_permanently(path) {
                        Ok(()) => {
                            permanent_count += 1;
                        }
                        Err(e) => {
                            errors.push(format!("{}: {e}", path.display()));
                        }
                    }
                }
            }
        }
        let _ = progress_tx.send(ProgressUpdate {
            files_done: total,
            files_total: total,
            current_file: String::new(),
        });
        let _ = status_tx.send(if errors.is_empty() {
            if permanent_count > 0 {
                OpStatus::Completed(format!(
                    "Deleted {permanent_count} item(s) permanently (network drive)"
                ))
            } else {
                OpStatus::Completed("Sent to Recycle Bin".into())
            }
        } else {
            OpStatus::Failed(errors.join("\n"))
        });
    });

    BackgroundOp {
        rx: progress_rx,
        status_rx,
        progress: ProgressUpdate {
            files_done: 0,
            files_total: total,
            current_file: String::new(),
        },
        status: OpStatus::Running,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_items_bg_copies_file() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("hello.txt");
        fs::write(&src, b"test content").unwrap();
        let dest = dir.path().join("out");
        fs::create_dir(&dest).unwrap();

        let mut op = copy_items_bg(vec![TransferItem::plain(src)], dest.clone());
        while op.poll() {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            op.status,
            OpStatus::Completed(format!("Copied into {}", dest.display()))
        );
        assert!(dest.join("hello.txt").exists());
    }

    #[test]
    fn copy_items_bg_overwrite_replaces_existing_destination() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("hello.txt");
        fs::write(&src, b"new content").unwrap();
        let dest = dir.path().join("out");
        fs::create_dir(&dest).unwrap();
        fs::write(dest.join("hello.txt"), b"old content").unwrap();

        let item = TransferItem {
            src,
            dest_name: None,
            overwrite: true,
        };
        let mut op = copy_items_bg(vec![item], dest.clone());
        while op.poll() {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            op.status,
            OpStatus::Completed(format!("Copied into {}", dest.display()))
        );
        assert_eq!(
            fs::read(dest.join("hello.txt")).unwrap(),
            b"new content"
        );
    }

    #[test]
    fn copy_items_bg_dest_name_lands_under_fresh_name() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("hello.txt");
        fs::write(&src, b"kept both").unwrap();
        let dest = dir.path().join("out");
        fs::create_dir(&dest).unwrap();
        fs::write(dest.join("hello.txt"), b"original").unwrap();

        let item = TransferItem {
            src,
            dest_name: Some("hello (copy).txt".to_string()),
            overwrite: false,
        };
        let mut op = copy_items_bg(vec![item], dest.clone());
        while op.poll() {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            op.status,
            OpStatus::Completed(format!("Copied into {}", dest.display()))
        );
        assert_eq!(fs::read(dest.join("hello.txt")).unwrap(), b"original");
        assert_eq!(
            fs::read(dest.join("hello (copy).txt")).unwrap(),
            b"kept both"
        );
    }

    #[test]
    fn copy_items_bg_fails_on_nested_collision() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("tree");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("f.txt"), b"new").unwrap();
        let dest = dir.path().join("out");
        fs::create_dir(&dest).unwrap();
        fs::create_dir(dest.join("tree")).unwrap();
        fs::write(dest.join("tree").join("f.txt"), b"old").unwrap();

        let mut op = copy_items_bg(vec![TransferItem::plain(src)], dest.clone());
        while op.poll() {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(matches!(op.status, OpStatus::Failed(_)));
        assert_eq!(fs::read(dest.join("tree").join("f.txt")).unwrap(), b"old");
    }

    #[test]
    fn move_items_bg_moves_file() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("hello.txt");
        fs::write(&src, b"data").unwrap();
        let dest = dir.path().join("out");
        fs::create_dir(&dest).unwrap();

        let mut op = move_items_bg(vec![TransferItem::plain(src.clone())], dest.clone());
        while op.poll() {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            op.status,
            OpStatus::Completed(format!("Moved into {}", dest.display()))
        );
        assert!(!src.exists());
        assert!(dest.join("hello.txt").exists());
    }
}
