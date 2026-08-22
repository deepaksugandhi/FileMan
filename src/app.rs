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

impl FileManApp {
    pub fn new(conn: Connection, loaded: Option<session::LoadedSession>) -> Self {
        let (panes, active_pane) = match loaded {
            Some(s) if !s.panes.is_empty() => (s.panes, s.active_pane),
            _ => (
                vec![Pane::new(PathBuf::from("C:\\")), Pane::new(PathBuf::from("C:\\"))],
                0,
            ),
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
                            }
                        });
                        if let Some(idx) = tab_clicked {
                            pane.active_tab = idx;
                        }

                        let current_path = pane.active_tab().path.clone();
                        ui.label(current_path.display().to_string());

                        if ui.button("Up").clicked() {
                            if let Some(parent) = current_path.parent() {
                                pane.active_tab_mut().navigate_to(parent.to_path_buf());
                                self.dirty = true;
                            }
                        }

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
