use crate::fs_entry::FsEntry;
use std::collections::HashSet;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    #[default]
    Details,
    List,
    Icons,
}

/// Distinguishes a normal folder-browsing tab from a pinned file-link tab.
/// File-link tabs display the file's parent folder but open the file itself
/// when activated (double-click / Enter).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabKind {
    /// Regular directory tab — shows folder contents.
    Folder,
    /// Pinned file shortcut — parent folder is listed, but activating the
    /// tab opens the target file with its default application.
    File,
}

impl Default for TabKind {
    fn default() -> Self {
        TabKind::Folder
    }
}

impl ViewMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ViewMode::Details => "details",
            ViewMode::List => "list",
            ViewMode::Icons => "icons",
        }
    }

    pub fn from_str(raw: &str) -> Self {
        match raw {
            "list" => ViewMode::List,
            "icons" => ViewMode::Icons,
            _ => ViewMode::Details,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Tab {
    pub path: PathBuf,
    /// For `TabKind::File` this holds the full path to the target file;
    /// for `TabKind::Folder` it is `None`.
    pub file_target: Option<PathBuf>,
    pub kind: TabKind,
    history_back: Vec<PathBuf>,
    history_forward: Vec<PathBuf>,
    pub sort_col: String,
    pub sort_asc: bool,
    pub view_mode: ViewMode,
    /// Names (not full paths) of selected entries in the current directory.
    pub selected: HashSet<String>,
    /// Resizable file-table column widths (name, modified, size, archive).
    pub col_widths: [f32; 4],
    /// Name-filter text for this tab's listing. Per-tab (not shared across
    /// panes/tabs) and not persisted across sessions — resets to empty on
    /// navigation, same as the selection.
    pub filter: String,
    /// Cached directory listing, refreshed on a background thread. Not
    /// persisted — reloaded via `listing_dirty` on session restore.
    pub listing: std::sync::Arc<Vec<FsEntry>>,
    /// True when `listing` is stale (fresh tab, navigation, or an external
    /// mutation) and needs to be reloaded via a background listing job.
    pub listing_dirty: bool,
    /// Bumped every time `listing` is replaced with a fresh background-job
    /// result. Lets the UI cache the filtered+sorted view (which involves an
    /// O(n log n) sort) and only redo that
    /// work when the listing, filter, or sort actually changed — not on
    /// every repaint (blinking cursor, hover, toast fade, ...).
    pub listing_version: u64,
    /// Filtered/sorted indices into `listing`; filenames and paths are owned
    /// once, and unchanged frames reuse the order without filtering/sorting.
    pub display_cache: Option<((u64, String, String, bool), Vec<usize>)>,
    /// Set when the last background listing job for this tab's path failed
    /// (e.g. permission denied). Cleared on the next successful listing.
    pub listing_error: Option<String>,
    /// Pinned tabs refuse to close or navigate away from their folder.
    pub locked: bool,
    /// User-assigned tab label. When set, overrides the folder-name label
    /// and survives navigation (the tab keeps its custom title even as
    /// `path` changes).
    pub custom_name: Option<String>,
}

pub const DEFAULT_COL_WIDTHS: [f32; 4] = [220.0, 140.0, 90.0, 60.0];

impl Tab {
    pub fn new(path: PathBuf) -> Self {
        Tab {
            path,
            file_target: None,
            kind: TabKind::Folder,
            history_back: Vec::new(),
            history_forward: Vec::new(),
            sort_col: "name".to_string(),
            sort_asc: true,
            view_mode: ViewMode::Details,
            selected: HashSet::new(),
            col_widths: DEFAULT_COL_WIDTHS,
            filter: String::new(),
            listing: Default::default(),
            listing_dirty: true,
            listing_error: None,
            listing_version: 0,
            display_cache: None,
            locked: false,
            custom_name: None,
        }
    }

    /// Creates a file-link tab: `path` is set to the file's parent directory
    /// so the listing still works, and `file_target` records the actual file.
    pub fn new_file(file_path: PathBuf) -> Self {
        let parent = file_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("C:\\"));
        let file_name = file_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut tab = Tab::new(parent);
        tab.kind = TabKind::File;
        tab.file_target = Some(file_path);
        tab.custom_name = Some(file_name);
        tab
    }

    /// The filtered+sorted view for `filter`/`sort_col`/`sort_asc`, recomputed
    /// only when the cache key (those three plus `listing_version`) changed
    /// since the last call.
    pub fn display_entries(&mut self, filter: &str, sort_col: &str, sort_asc: bool) -> &[usize] {
        let key = (
            self.listing_version,
            filter.to_string(),
            sort_col.to_string(),
            sort_asc,
        );
        let stale = match &self.display_cache {
            Some((cached_key, _)) => *cached_key != key,
            None => true,
        };
        if stale {
            let query = filter.to_lowercase();
            let mut entries: Vec<usize> = (0..self.listing.len())
                .filter(|&i| {
                    query.is_empty() || self.listing[i].name.to_lowercase().contains(&query)
                })
                .collect();
            entries.sort_by(|&a, &b| {
                crate::fs_entry::compare_entries(
                    &self.listing[a],
                    &self.listing[b],
                    sort_col,
                    sort_asc,
                )
            });
            self.display_cache = Some((key, entries));
        }
        &self.display_cache.as_ref().unwrap().1
    }

    /// Keep navigation and selection state, but release directory payloads.
    pub fn release_listing(&mut self) {
        self.listing = Default::default();
        self.display_cache = None;
        self.listing_error = None;
        self.listing_dirty = true;
    }

    /// The tab's display label: the custom name if one was set, otherwise
    /// the current folder's name (for folder tabs) or the file name (for
    /// file-link tabs).
    pub fn display_label(&self) -> String {
        self.custom_name.clone().unwrap_or_else(|| {
            if self.kind == TabKind::File {
                self.file_target
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| self.path.display().to_string())
            } else {
                self.path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| self.path.display().to_string())
            }
        })
    }

    /// Navigates to `new_path` unless the tab is pinned (`locked`), in which
    /// case it refuses and returns false. All folder-changing moves (tree,
    /// address bar, double-click, up/back/forward) funnel through here.
    pub fn try_navigate(&mut self, new_path: PathBuf) -> bool {
        if self.locked {
            return false;
        }
        self.navigate_to(new_path);
        true
    }

    pub fn navigate_to(&mut self, new_path: PathBuf) {
        self.history_back.push(self.path.clone());
        self.history_forward.clear();
        self.path = new_path;
        self.clear_selection();
        self.filter.clear();
        self.release_listing();
    }

    pub fn go_back(&mut self) -> bool {
        if self.locked {
            return false;
        }
        if let Some(prev) = self.history_back.pop() {
            self.history_forward.push(self.path.clone());
            self.path = prev;
            self.clear_selection();
            self.filter.clear();
            self.release_listing();
            true
        } else {
            false
        }
    }

    pub fn go_forward(&mut self) -> bool {
        if self.locked {
            return false;
        }
        if let Some(next) = self.history_forward.pop() {
            self.history_back.push(self.path.clone());
            self.path = next;
            self.clear_selection();
            self.filter.clear();
            self.release_listing();
            true
        } else {
            false
        }
    }

    pub fn toggle_select(&mut self, name: &str) {
        if !self.selected.insert(name.to_string()) {
            self.selected.remove(name);
        }
    }

    pub fn select_only(&mut self, name: &str) {
        self.selected.clear();
        self.selected.insert(name.to_string());
    }

    pub fn clear_selection(&mut self) {
        self.selected.clear();
    }

    /// Selects all items in the given range (inclusive) by name.
    /// Used for Shift+click range selection.
    pub fn select_range(&mut self, names: &[String]) {
        for name in names {
            self.selected.insert(name.clone());
        }
    }

    /// Adds every given name to the selection (Ctrl+A select-all).
    pub fn select_all(&mut self, names: &[String]) {
        for name in names {
            self.selected.insert(name.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_indices_filter_sort_and_release_without_copying_payloads() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("B.txt"), b"bb").unwrap();
        std::fs::write(dir.path().join("a.txt"), b"a").unwrap();
        std::fs::create_dir(dir.path().join("Folder")).unwrap();
        let mut tab = Tab::new(dir.path().into());
        tab.listing = std::sync::Arc::new(crate::fs_entry::list_dir(dir.path()).unwrap());
        let original = tab.listing.as_ptr();
        assert_eq!(tab.display_entries("", "size", false), &[0, 2, 1]);
        assert_eq!(tab.display_entries("TXT", "name", true), &[1, 2]);
        assert_eq!(tab.display_entries("folder", "name", true), &[0]);
        assert!(tab.display_entries("absent", "name", true).is_empty());
        assert_eq!(tab.listing.as_ptr(), original);
        tab.select_only("B.txt");
        tab.release_listing();
        assert!(tab.listing.is_empty() && tab.display_cache.is_none() && tab.listing_dirty);
        assert!(tab.selected.contains("B.txt"));
    }

    #[test]
    fn navigate_to_updates_path_and_records_history() {
        let mut tab = Tab::new(PathBuf::from("C:\\a"));
        tab.navigate_to(PathBuf::from("C:\\a\\b"));
        assert_eq!(tab.path, PathBuf::from("C:\\a\\b"));
    }

    #[test]
    fn go_back_returns_to_previous_path() {
        let mut tab = Tab::new(PathBuf::from("C:\\a"));
        tab.navigate_to(PathBuf::from("C:\\a\\b"));
        assert!(tab.go_back());
        assert_eq!(tab.path, PathBuf::from("C:\\a"));
    }

    #[test]
    fn go_back_on_empty_history_returns_false_and_keeps_path() {
        let mut tab = Tab::new(PathBuf::from("C:\\a"));
        assert!(!tab.go_back());
        assert_eq!(tab.path, PathBuf::from("C:\\a"));
    }

    #[test]
    fn go_forward_after_go_back_returns_to_next_path() {
        let mut tab = Tab::new(PathBuf::from("C:\\a"));
        tab.navigate_to(PathBuf::from("C:\\a\\b"));
        tab.go_back();
        assert!(tab.go_forward());
        assert_eq!(tab.path, PathBuf::from("C:\\a\\b"));
    }

    #[test]
    fn navigate_to_clears_forward_history() {
        let mut tab = Tab::new(PathBuf::from("C:\\a"));
        tab.navigate_to(PathBuf::from("C:\\a\\b"));
        tab.go_back();
        tab.navigate_to(PathBuf::from("C:\\a\\c"));
        assert!(!tab.go_forward());
    }

    #[test]
    fn toggle_select_adds_then_removes() {
        let mut tab = Tab::new(PathBuf::from("C:\\a"));
        tab.toggle_select("x.txt");
        assert!(tab.selected.contains("x.txt"));
        tab.toggle_select("y.txt");
        assert_eq!(tab.selected.len(), 2);
        tab.toggle_select("x.txt");
        assert!(tab.selected.contains("y.txt"));
        assert!(!tab.selected.contains("x.txt"));
    }

    #[test]
    fn select_only_replaces_selection() {
        let mut tab = Tab::new(PathBuf::from("C:\\a"));
        tab.select_only("a.txt");
        tab.select_only("b.txt");
        assert_eq!(
            tab.selected,
            ["b.txt"].into_iter().map(String::from).collect()
        );
    }

    #[test]
    fn filter_is_per_tab_and_clears_on_navigation() {
        let mut a = Tab::new(PathBuf::from("C:\\a"));
        let b = Tab::new(PathBuf::from("C:\\b"));
        a.filter = "readme".to_string();
        assert!(
            b.filter.is_empty(),
            "each tab has its own independent filter"
        );

        a.navigate_to(PathBuf::from("C:\\a\\sub"));
        assert!(a.filter.is_empty(), "navigating clears the stale filter");
    }

    #[test]
    fn listing_dirty_starts_true_and_is_set_by_navigation() {
        let mut tab = Tab::new(PathBuf::from("C:\\a"));
        assert!(tab.listing_dirty);
        tab.listing_dirty = false;
        tab.navigate_to(PathBuf::from("C:\\a\\b"));
        assert!(tab.listing_dirty);
        tab.listing_dirty = false;
        tab.go_back();
        assert!(tab.listing_dirty);
        tab.listing_dirty = false;
        tab.go_forward();
        assert!(tab.listing_dirty);
    }

    #[test]
    fn navigation_clears_selection() {
        let mut tab = Tab::new(PathBuf::from("C:\\a"));
        tab.select_only("a.txt");
        tab.navigate_to(PathBuf::from("C:\\a\\b"));
        assert!(tab.selected.is_empty());

        tab.select_only("c.txt");
        tab.go_back();
        assert!(tab.selected.is_empty());
    }

    #[test]
    fn locked_tab_refuses_navigation_and_history_moves() {
        let mut tab = Tab::new(PathBuf::from("C:\\a"));
        tab.navigate_to(PathBuf::from("C:\\b"));
        tab.locked = true;

        let target = PathBuf::from("C:\\c");
        assert!(!tab.try_navigate(target.clone()), "locked tab must refuse");
        assert_eq!(tab.path, PathBuf::from("C:\\b"), "path unchanged");

        assert!(!tab.go_back(), "back blocked while locked");
        assert!(!tab.go_forward(), "forward blocked while locked");
        assert_eq!(tab.path, PathBuf::from("C:\\b"));
        assert_eq!(target, PathBuf::from("C:\\c"));
    }

    #[test]
    fn unlocked_tab_navigates_normally() {
        let mut tab = Tab::new(PathBuf::from("C:\\a"));
        assert!(tab.try_navigate(PathBuf::from("C:\\b")));
        assert_eq!(tab.path, PathBuf::from("C:\\b"));
    }
}
