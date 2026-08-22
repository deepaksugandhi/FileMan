use crate::tab::Tab;
use std::path::PathBuf;

pub struct Pane {
    pub tabs: Vec<Tab>,
    pub active_tab: usize,
}

impl Pane {
    pub fn new(initial_path: PathBuf) -> Self {
        Pane {
            tabs: vec![Tab::new(initial_path)],
            active_tab: 0,
        }
    }

    pub fn active_tab(&self) -> &Tab {
        &self.tabs[self.active_tab]
    }

    pub fn active_tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active_tab]
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
}
