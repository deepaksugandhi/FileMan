use std::collections::HashSet;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    #[default]
    Details,
    List,
    Icons,
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
}

pub const DEFAULT_COL_WIDTHS: [f32; 4] = [220.0, 140.0, 90.0, 60.0];

impl Tab {
    pub fn new(path: PathBuf) -> Self {
        Tab {
            path,
            history_back: Vec::new(),
            history_forward: Vec::new(),
            sort_col: "name".to_string(),
            sort_asc: true,
            view_mode: ViewMode::Details,
            selected: HashSet::new(),
            col_widths: DEFAULT_COL_WIDTHS,
            filter: String::new(),
        }
    }

    pub fn navigate_to(&mut self, new_path: PathBuf) {
        self.history_back.push(self.path.clone());
        self.history_forward.clear();
        self.path = new_path;
        self.clear_selection();
        self.filter.clear();
    }

    pub fn go_back(&mut self) -> bool {
        if let Some(prev) = self.history_back.pop() {
            self.history_forward.push(self.path.clone());
            self.path = prev;
            self.clear_selection();
            self.filter.clear();
            true
        } else {
            false
        }
    }

    pub fn go_forward(&mut self) -> bool {
        if let Some(next) = self.history_forward.pop() {
            self.history_back.push(self.path.clone());
            self.path = next;
            self.clear_selection();
            self.filter.clear();
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let mut b = Tab::new(PathBuf::from("C:\\b"));
        a.filter = "readme".to_string();
        assert!(b.filter.is_empty(), "each tab has its own independent filter");

        a.navigate_to(PathBuf::from("C:\\a\\sub"));
        assert!(a.filter.is_empty(), "navigating clears the stale filter");
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
}
