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
        if self.tabs.len() <= 1 {
            return;
        }
        self.tabs.remove(index);
        if self.active_tab >= self.tabs.len() {
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
}
