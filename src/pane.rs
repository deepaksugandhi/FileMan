use crate::tab::Tab;
use std::path::PathBuf;

pub struct Pane {
    pub tabs: Vec<Tab>,
    pub active_tab: usize,
    /// Editable address bar text for this pane.
    pub address_bar: String,
    /// True while showing the typeable address bar (`address_bar`) instead
    /// of the clickable breadcrumb trail.
    pub address_edit_mode: bool,
}

impl Pane {
    pub fn new(initial_path: PathBuf) -> Self {
        Pane {
            address_bar: initial_path.display().to_string(),
            tabs: vec![Tab::new(initial_path)],
            active_tab: 0,
            address_edit_mode: false,
        }
    }

    pub fn active_tab(&self) -> &Tab {
        &self.tabs[self.active_tab]
    }

    pub fn active_tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active_tab]
    }

    /// Switches the active tab index and marks the newly activated tab as
    /// needing a fresh listing, so external file-system changes that arrived
    /// while the tab was in the background become visible immediately.
    pub fn set_active_tab(&mut self, idx: usize) {
        if idx != self.active_tab && idx < self.tabs.len() {
            self.tabs[idx].listing_dirty = true;
        }
        self.active_tab = idx;
    }

    pub fn open_tab(&mut self, path: PathBuf) {
        self.tabs.push(Tab::new(path));
        self.active_tab = self.tabs.len() - 1;
    }

    pub fn close_tab(&mut self, index: usize) {
        if self.tabs.len() <= 1 || index >= self.tabs.len() {
            return;
        }
        self.tabs.remove(index);
        if index < self.active_tab {
            self.active_tab -= 1;
        } else if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        }
    }

    /// Moves a tab from slot `from` to slot `to` (both indices into `tabs`),
    /// keeping `active_tab` pointed at the same tab afterwards. No-op when
    /// either index is out of bounds or the move wouldn't change anything.
    pub fn move_tab(&mut self, from: usize, to: usize) {
        if from == to || from >= self.tabs.len() || to >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        if self.active_tab == from {
            self.active_tab = to;
        } else if from < self.active_tab && to >= self.active_tab {
            self.active_tab -= 1;
        } else if from > self.active_tab && to <= self.active_tab {
            self.active_tab += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_tab_adds_and_activates_it() {
        let mut pane = Pane::new(PathBuf::from("C:\\a"));
        pane.open_tab(PathBuf::from("C:\\b"));
        assert_eq!(pane.tabs.len(), 2);
        assert_eq!(pane.active_tab, 1);
        assert_eq!(pane.active_tab().path, PathBuf::from("C:\\b"));
    }

    #[test]
    fn close_tab_removes_it_and_keeps_valid_active_index() {
        let mut pane = Pane::new(PathBuf::from("C:\\a"));
        pane.open_tab(PathBuf::from("C:\\b"));
        pane.close_tab(1);
        assert_eq!(pane.tabs.len(), 1);
        assert_eq!(pane.active_tab, 0);
    }

    #[test]
    fn close_tab_refuses_to_close_last_remaining_tab() {
        let mut pane = Pane::new(PathBuf::from("C:\\a"));
        pane.close_tab(0);
        assert_eq!(pane.tabs.len(), 1);
    }

    #[test]
    fn close_tab_before_active_tab_keeps_tracking_same_tab() {
        let mut pane = Pane::new(PathBuf::from("C:\\a"));
        pane.open_tab(PathBuf::from("C:\\b"));
        pane.open_tab(PathBuf::from("C:\\c"));
        pane.open_tab(PathBuf::from("C:\\d"));
        // tabs: [a, b, c, d], active_tab = 3 (d)
        pane.active_tab = 2; // point at c
        pane.close_tab(0); // remove a -> [b, c, d]
        assert_eq!(pane.active_tab, 1);
        assert_eq!(pane.active_tab().path, PathBuf::from("C:\\c"));
    }

    #[test]
    fn close_tab_with_out_of_bounds_index_is_a_no_op() {
        let mut pane = Pane::new(PathBuf::from("C:\\a"));
        pane.open_tab(PathBuf::from("C:\\b"));
        pane.close_tab(99);
        assert_eq!(pane.tabs.len(), 2);
    }

    #[test]
    fn move_tab_reorders_and_keeps_active_on_the_moved_tab() {
        let mut pane = Pane::new(PathBuf::from("C:\\a"));
        pane.open_tab(PathBuf::from("C:\\b"));
        pane.open_tab(PathBuf::from("C:\\c"));
        pane.active_tab = 0; // open_tab auto-activates; point back at a
        // tabs: [a, b, c], active = 0 (a)
        pane.move_tab(0, 2);
        let paths: Vec<_> = pane.tabs.iter().map(|t| t.path.clone()).collect();
        assert_eq!(paths, [r"C:\b", r"C:\c", r"C:\a"].map(PathBuf::from));
        assert_eq!(pane.active_tab, 2, "active tab must follow the moved tab");
        assert_eq!(pane.active_tab().path, PathBuf::from("C:\\a"));
    }

    #[test]
    fn move_tab_before_active_shifts_active_index_forward() {
        let mut pane = Pane::new(PathBuf::from("C:\\a"));
        pane.open_tab(PathBuf::from("C:\\b"));
        pane.active_tab = 1; // point at b
        pane.move_tab(0, 1); // a moves after b -> [b, a]
        assert_eq!(pane.active_tab, 0, "b slid down one slot");
        assert_eq!(pane.active_tab().path, PathBuf::from("C:\\b"));
    }

    #[test]
    fn move_tab_after_active_shifts_active_index_back() {
        let mut pane = Pane::new(PathBuf::from("C:\\a"));
        pane.open_tab(PathBuf::from("C:\\b"));
        pane.open_tab(PathBuf::from("C:\\c"));
        pane.active_tab = 0; // point at a
        pane.move_tab(2, 0); // c jumps to front -> [c, a, b]
        assert_eq!(pane.active_tab, 1, "a slid up one slot");
        assert_eq!(pane.active_tab().path, PathBuf::from("C:\\a"));
    }

    #[test]
    fn move_tab_with_invalid_indices_is_a_no_op() {
        let mut pane = Pane::new(PathBuf::from("C:\\a"));
        pane.open_tab(PathBuf::from("C:\\b"));
        pane.active_tab = 0;
        pane.move_tab(0, 5);
        pane.move_tab(9, 1);
        pane.move_tab(1, 1);
        assert_eq!(pane.tabs.len(), 2);
        assert_eq!(pane.active_tab, 0);
    }
}
