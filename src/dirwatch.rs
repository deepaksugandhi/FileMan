//! Live directory change detection for the folders shown in the panes.
//! A `notify` watcher (non-recursive, one per visible folder) feeds raw
//! events into a channel; `poll` debounces them so an event storm (copying
//! 10k files in) triggers one re-list after things settle, not thousands.

use eframe::egui;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const DEBOUNCE: Duration = Duration::from_millis(250);

pub struct DirWatch {
    watcher: RecommendedWatcher,
    watched: Vec<PathBuf>,
    requested: Vec<PathBuf>,
    rx: mpsc::Receiver<PathBuf>,
    /// Folder -> time of its most recent event, still waiting out the debounce.
    pending: HashMap<PathBuf, Instant>,
}

impl DirWatch {
    pub fn new(ctx: &egui::Context) -> Option<Self> {
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(event) = res else { return };
            if matches!(event.kind, notify::EventKind::Access(_)) {
                return;
            }
            for path in event.paths {
                // The event path is either the watched folder itself or an
                // entry inside it; report both so the caller can match either.
                if let Some(parent) = path.parent() {
                    let _ = tx.send(parent.to_path_buf());
                }
                let _ = tx.send(path);
            }
            ctx.request_repaint();
        })
        .ok()?;
        Some(DirWatch { watcher, watched: Vec::new(), requested: Vec::new(), rx, pending: HashMap::new() })
    }

    /// Makes the watched set equal `dirs` (cheap no-op when unchanged).
    /// Folders that can't be watched (offline share, no permission) are
    /// skipped until the requested set changes.
    pub fn sync(&mut self, dirs: &[PathBuf]) {
        if self.requested == dirs {
            return;
        }
        self.requested = dirs.to_vec();
        for old in std::mem::take(&mut self.watched) {
            if dirs.contains(&old) {
                self.watched.push(old);
            } else {
                let _ = self.watcher.unwatch(&old);
            }
        }
        for dir in dirs {
            if !self.watched.contains(dir)
                && self.watcher.watch(dir, RecursiveMode::NonRecursive).is_ok()
            {
                self.watched.push(dir.clone());
            }
        }
    }

    /// Watched folders whose events have settled since the last call.
    pub fn poll(&mut self, ctx: &egui::Context) -> Vec<PathBuf> {
        let now = Instant::now();
        while let Ok(path) = self.rx.try_recv() {
            if self.watched.contains(&path) {
                self.pending.insert(path, now);
            }
        }
        if self.pending.is_empty() {
            return Vec::new();
        }
        let (ready, waiting): (Vec<_>, Vec<_>) =
            self.pending.drain().partition(|(_, t)| now.duration_since(*t) >= DEBOUNCE);
        self.pending = waiting.into_iter().collect();
        if !self.pending.is_empty() {
            ctx.request_repaint_after(DEBOUNCE);
        }
        ready.into_iter().map(|(p, _)| p).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_a_watched_folder_after_a_file_appears() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = egui::Context::default();
        let mut watch = DirWatch::new(&ctx).expect("watcher");
        let root = dir.path().to_path_buf();
        watch.sync(std::slice::from_ref(&root));
        std::fs::write(root.join("new.txt"), b"x").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut hit = false;
        while Instant::now() < deadline && !hit {
            std::thread::sleep(Duration::from_millis(100));
            hit = watch.poll(&ctx).contains(&root);
        }
        assert!(hit, "expected a debounced event for the watched folder");
    }
}
