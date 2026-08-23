use crate::pane::Pane;
use crate::session::{self, WindowGeometry};
use crate::tree;
use eframe::egui;
use rusqlite::Connection;
use std::path::PathBuf;

pub struct FileManApp {
    conn: Connection,
    panes: Vec<Pane>,
    active_pane: usize,
    dirty: bool,
    last_size: egui::Vec2,
}

/// Ensures the given panes vector has exactly two entries, padding with fresh
/// panes rooted at C:\ if there are fewer than two, truncating if there are
/// more (shouldn't happen given the session schema, but be safe), and
/// clamping `active_pane` into the resulting valid range.
fn ensure_two_panes(mut panes: Vec<Pane>, active_pane: usize) -> (Vec<Pane>, usize) {
    while panes.len() < 2 {
        panes.push(Pane::new(PathBuf::from("C:\\")));
    }
    panes.truncate(2);
    let active_pane = active_pane.min(panes.len().saturating_sub(1));
    (panes, active_pane)
}

impl FileManApp {
    pub fn new(conn: Connection, loaded: Option<session::LoadedSession>) -> Self {
        let (panes, active_pane) = match loaded {
            Some(s) if !s.panes.is_empty() => ensure_two_panes(s.panes, s.active_pane),
            _ => ensure_two_panes(Vec::new(), 0),
        };
        FileManApp {
            conn,
            panes,
            active_pane,
            dirty: false,
            last_size: egui::vec2(1200.0, 800.0),
        }
    }

    // ponytail: writes on every state-changing action rather than debouncing
    // resize-drag events. SQLite writes here are single-row upserts on a local
    // file, cheap enough for a desktop app; add debouncing only if a real
    // resize-storm shows up as jank.
    fn persist(&mut self) {
        let window = WindowGeometry {
            width: self.last_size.x,
            height: self.last_size.y,
            pos_x: None,
            pos_y: None,
            monitor_name: None,
        };
        let _ = session::save_session(&self.conn, &window, &self.panes, self.active_pane);
    }
}

// ADAPTED for the actually-resolved eframe/egui 0.36.1 API, which differs from
// the plan in two ways:
// 1. `eframe::App`'s method is `fn ui(&mut self, ui: &mut egui::Ui, frame: &mut
//    eframe::Frame)`, not `fn update(&mut self, ctx: &egui::Context, ...)`.
// 2. There is no `egui::SidePanel`/`egui::TopBottomPanel` type in this egui
//    version; side/top/bottom panels are all constructed via `egui::Panel`
//    (e.g. `egui::Panel::left(id)`), and both `Panel::show` and
//    `CentralPanel::show` take `ui: &mut egui::Ui` (not `&egui::Context`) since
//    panels now nest directly inside the enclosing `Ui` rather than being shown
//    against the top-level `Context`. So instead of `ctx.clone()` plus
//    `Panel::show(&ctx, ...)`, we show both panels directly against the `ui`
//    passed into this method. `egui::Context::screen_rect()` also doesn't
//    exist here; the equivalent is `ctx.input(|i| i.viewport_rect())`.
impl eframe::App for FileManApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        let screen = ctx.input(|i| i.viewport_rect()).size();
        if (screen - self.last_size).length() > 1.0 {
            self.last_size = screen;
            self.dirty = true;
        }

        egui::Panel::left("folder_tree").show(ui, |ui| {
            ui.heading("Folders");
            for drive in tree::list_drives() {
                if ui.button(drive.display().to_string()).clicked() {
                    self.panes[self.active_pane]
                        .active_tab_mut()
                        .navigate_to(drive.clone());
                    self.dirty = true;
                }
            }
        });

        egui::CentralPanel::default().show(ui, |ui| {
            ui.columns(2, |columns| {
                for pane_idx in 0..2 {
                    let is_active = pane_idx == self.active_pane;
                    columns[pane_idx].group(|ui| {
                        if ui.selectable_label(is_active, "active").clicked() {
                            self.active_pane = pane_idx;
                            self.dirty = true;
                        }

                        let pane = &mut self.panes[pane_idx];
                        let mut tab_clicked = None;
                        let mut tab_closed = None;
                        let mut tab_opened = false;
                        ui.horizontal(|ui| {
                            for (tab_idx, tab) in pane.tabs.iter().enumerate() {
                                let label = tab
                                    .path
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| tab.path.display().to_string());
                                if ui
                                    .selectable_label(tab_idx == pane.active_tab, label)
                                    .clicked()
                                {
                                    tab_clicked = Some(tab_idx);
                                }
                                if ui.small_button("x").clicked() {
                                    tab_closed = Some(tab_idx);
                                }
                            }
                            if ui.button("+").clicked() {
                                tab_opened = true;
                            }
                        });
                        if let Some(idx) = tab_clicked {
                            pane.active_tab = idx;
                        }
                        if let Some(idx) = tab_closed {
                            pane.close_tab(idx);
                            self.dirty = true;
                        }
                        if tab_opened {
                            let current_path = pane.active_tab().path.clone();
                            pane.open_tab(current_path);
                            self.dirty = true;
                        }

                        let current_path = pane.active_tab().path.clone();
                        ui.label(current_path.display().to_string());

                        ui.horizontal(|ui| {
                            if ui.button("Back").clicked() && pane.active_tab_mut().go_back() {
                                self.dirty = true;
                            }
                            if ui.button("Forward").clicked() && pane.active_tab_mut().go_forward() {
                                self.dirty = true;
                            }
                            if ui.button("Up").clicked() {
                                if let Some(parent) = current_path.parent() {
                                    pane.active_tab_mut().navigate_to(parent.to_path_buf());
                                    self.dirty = true;
                                }
                            }
                        });

                        match crate::fs_entry::list_dir(&current_path) {
                            Ok(entries) => {
                                egui::ScrollArea::vertical().show(ui, |ui| {
                                    for entry in entries {
                                        let label = if entry.is_dir {
                                            format!("[dir] {}", entry.name)
                                        } else {
                                            format!("{} ({} bytes)", entry.name, entry.size)
                                        };
                                        if ui.button(label).clicked() && entry.is_dir {
                                            pane.active_tab_mut().navigate_to(entry.path.clone());
                                            self.dirty = true;
                                        }
                                    }
                                });
                            }
                            Err(err) => {
                                ui.colored_label(egui::Color32::RED, format!("Error: {err}"));
                            }
                        }
                    });
                }
            });
        });

        if self.dirty {
            self.persist();
            self.dirty = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_two_panes_pads_a_single_pane_up_to_two() {
        let panes = vec![Pane::new(PathBuf::from("D:\\one"))];
        let (panes, active_pane) = ensure_two_panes(panes, 0);
        assert_eq!(panes.len(), 2);
        assert_eq!(panes[0].tabs[0].path, PathBuf::from("D:\\one"));
        assert_eq!(panes[1].tabs[0].path, PathBuf::from("C:\\"));
        assert_eq!(active_pane, 0);
    }

    #[test]
    fn ensure_two_panes_creates_two_fresh_panes_from_empty() {
        let (panes, active_pane) = ensure_two_panes(Vec::new(), 0);
        assert_eq!(panes.len(), 2);
        assert_eq!(panes[0].tabs[0].path, PathBuf::from("C:\\"));
        assert_eq!(panes[1].tabs[0].path, PathBuf::from("C:\\"));
        assert_eq!(active_pane, 0);
    }

    #[test]
    fn ensure_two_panes_leaves_a_valid_two_pane_vector_unchanged() {
        let panes = vec![
            Pane::new(PathBuf::from("D:\\left")),
            Pane::new(PathBuf::from("E:\\right")),
        ];
        let (panes, active_pane) = ensure_two_panes(panes, 1);
        assert_eq!(panes.len(), 2);
        assert_eq!(panes[0].tabs[0].path, PathBuf::from("D:\\left"));
        assert_eq!(panes[1].tabs[0].path, PathBuf::from("E:\\right"));
        assert_eq!(active_pane, 1);
    }

    #[test]
    fn ensure_two_panes_truncates_more_than_two_panes() {
        let panes = vec![
            Pane::new(PathBuf::from("D:\\one")),
            Pane::new(PathBuf::from("E:\\two")),
            Pane::new(PathBuf::from("F:\\three")),
        ];
        let (panes, _) = ensure_two_panes(panes, 0);
        assert_eq!(panes.len(), 2);
        assert_eq!(panes[0].tabs[0].path, PathBuf::from("D:\\one"));
        assert_eq!(panes[1].tabs[0].path, PathBuf::from("E:\\two"));
    }

    #[test]
    fn ensure_two_panes_clamps_out_of_range_active_pane() {
        let panes = vec![Pane::new(PathBuf::from("C:\\"))];
        let (panes, active_pane) = ensure_two_panes(panes, 99);
        assert_eq!(panes.len(), 2);
        assert_eq!(active_pane, 1);
    }
}
