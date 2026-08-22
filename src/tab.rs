use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub enum ViewMode {
    List,
    Details,
    Icons,
}

#[derive(Debug, Clone)]
pub struct Tab {
    pub path: PathBuf,
    history_back: Vec<PathBuf>,
    history_forward: Vec<PathBuf>,
    pub sort_col: String,
    pub sort_asc: bool,
    pub view_mode: ViewMode,
}

impl Tab {
    pub fn new(path: PathBuf) -> Self {
        Tab {
            path,
            history_back: Vec::new(),
            history_forward: Vec::new(),
            sort_col: "name".to_string(),
            sort_asc: true,
            view_mode: ViewMode::Details,
        }
    }

    pub fn navigate_to(&mut self, new_path: PathBuf) {
        self.history_back.push(self.path.clone());
        self.history_forward.clear();
        self.path = new_path;
    }

    pub fn go_back(&mut self) -> bool {
        if let Some(prev) = self.history_back.pop() {
            self.history_forward.push(self.path.clone());
            self.path = prev;
            true
        } else {
            false
        }
    }

    pub fn go_forward(&mut self) -> bool {
        if let Some(next) = self.history_forward.pop() {
            self.history_back.push(self.path.clone());
            self.path = next;
            true
        } else {
            false
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
}
