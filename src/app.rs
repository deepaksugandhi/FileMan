use crate::fs_ops::{self, ClipboardOp};
use crate::pane::Pane;
use crate::session::{self, WindowGeometry};
use crate::tree;
use eframe::egui;
use rusqlite::Connection;
use std::path::PathBuf;

/// Modal dialog state (only one open at a time).
#[derive(Debug, Clone)]
enum Dialog {
    Rename { path: PathBuf, name: String },
    NewFolder { name: String },
    NewFile { name: String },
}

pub struct FileManApp {
    conn: Connection,
    panes: Vec<Pane>,
    active_pane: usize,
    dirty: bool,
    last_size: egui::Vec2,
    clipboard: Vec<PathBuf>,
    clipboard_op: Option<ClipboardOp>,
    dialog: Option<Dialog>,
    status: String,
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
            clipboard: Vec::new(),
            clipboard_op: None,
            dialog: None,
            status: String::new(),
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

    fn active_tab_dir(&self) -> PathBuf {
        self.panes[self.active_pane].active_tab().path.clone()
    }

    fn selected_paths(&self) -> Vec<PathBuf> {
        let tab = self.panes[self.active_pane].active_tab();
        tab.selected
            .iter()
            .map(|name| tab.path.join(name))
            .collect()
    }

    fn copy_selection(&mut self) {
        let paths = self.selected_paths();
        if paths.is_empty() {
            self.status = "Nothing selected".into();
            return;
        }
        self.clipboard = paths;
        self.clipboard_op = Some(ClipboardOp::Copy);
        self.status = format!("Copied {} item(s)", self.clipboard.len());
    }

    fn cut_selection(&mut self) {
        let paths = self.selected_paths();
        if paths.is_empty() {
            self.status = "Nothing selected".into();
            return;
        }
        self.clipboard = paths;
        self.clipboard_op = Some(ClipboardOp::Cut);
        self.status = format!("Cut {} item(s)", self.clipboard.len());
    }

    fn paste_clipboard(&mut self) {
        if self.clipboard.is_empty() {
            self.status = "Clipboard is empty".into();
            return;
        }
        let dest = self.active_tab_dir();
        let mut errors = Vec::new();
        for src in &self.clipboard {
            // Cutting into the same folder the items already live in: no-op.
            if self.clipboard_op == Some(ClipboardOp::Cut) && src.parent() == Some(dest.as_path()) {
                continue;
            }
            let result = match self.clipboard_op {
                Some(ClipboardOp::Copy) => fs_ops::copy_item(src, &dest).map(|_| ()),
                _ => fs_ops::move_item(src, &dest),
            };
            if let Err(err) = result {
                errors.push(format!("{}: {err}", src.display()));
            }
        }
        if self.clipboard_op == Some(ClipboardOp::Cut) {
            self.clipboard.clear();
            self.panes[self.active_pane]
                .active_tab_mut()
                .clear_selection();
        }
        self.status = if errors.is_empty() {
            format!("Pasted into {}", dest.display())
        } else {
            errors.join("\n")
        };
    }

    fn delete_selection(&mut self) {
        let paths = self.selected_paths();
        if paths.is_empty() {
            self.status = "Nothing selected".into();
            return;
        }
        match fs_ops::delete_to_trash(&paths) {
            Ok(()) => {
                self.status = format!("Sent {} item(s) to Recycle Bin", paths.len());
                self.panes[self.active_pane]
                    .active_tab_mut()
                    .clear_selection();
            }
            Err(err) => self.status = format!("Delete failed: {err}"),
        }
    }

    fn begin_rename(&mut self) {
        let tab = self.panes[self.active_pane].active_tab();
        if tab.selected.len() != 1 {
            self.status = "Select exactly one item to rename".into();
            return;
        }
        let name = tab.selected.iter().next().unwrap().clone();
        self.dialog = Some(Dialog::Rename {
            path: tab.path.join(&name),
            name,
        });
    }

    /// Runs the pending dialog's filesystem action and closes it.
    fn commit_dialog(&mut self) {
        let Some(dialog) = self.dialog.take() else {
            return;
        };
        let parent = self.active_tab_dir();
        let result = match &dialog {
            Dialog::Rename { path, name } => fs_ops::rename_item(path, name)
                .map(|_| format!("Renamed to {name}"))
                .map_err(|err| format!("Rename failed: {err}")),
            Dialog::NewFolder { name } => fs_ops::create_folder(&parent, name)
                .map(|_| format!("Created folder {name}"))
                .map_err(|err| format!("Create folder failed: {err}")),
            Dialog::NewFile { name } => fs_ops::create_file(&parent, name)
                .map(|_| format!("Created file {name}"))
                .map_err(|err| format!("Create file failed: {err}")),
        };
        self.status = match result {
            Ok(msg) => msg,
            Err(msg) => msg,
        };
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
            ui.horizontal(|ui| {
                if ui.button("Copy").clicked() {
                    self.copy_selection();
                }
                if ui.button("Cut").clicked() {
                    self.cut_selection();
                }
                if ui.button("Paste").clicked() {
                    self.paste_clipboard();
                }
                if ui.button("Delete").clicked() {
                    self.delete_selection();
                }
                if ui.button("Rename").clicked() {
                    self.begin_rename();
                }
                if ui.button("New Folder").clicked() {
                    self.dialog = Some(Dialog::NewFolder {
                        name: String::new(),
                    });
                }
                if ui.button("New File").clicked() {
                    self.dialog = Some(Dialog::NewFile {
                        name: String::new(),
                    });
                }
            });
            if !self.status.is_empty() {
                ui.label(&self.status);
            }

            // Modal dialogs (rename / new folder / new file).
            let mut commit = false;
            let mut cancel = false;
            if let Some(dialog) = &mut self.dialog {
                let (title, name) = match dialog {
                    Dialog::Rename { name, .. } => ("Rename", name),
                    Dialog::NewFolder { name } => ("New Folder", name),
                    Dialog::NewFile { name } => ("New File", name),
                };
                egui::Window::new(title).show(&ctx, |ui| {
                    let edit = ui.text_edit_singleline(name);
                    edit.request_focus();
                    commit = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.horizontal(|ui| {
                        if ui.button("OK").clicked() {
                            commit = true;
                        }
                        if ui.button("Cancel").clicked() {
                            cancel = true;
                        }
                    });
                });
            }
            if cancel {
                self.dialog = None;
            } else if commit {
                self.commit_dialog();
            }

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
                            if ui.button("Forward").clicked() && pane.active_tab_mut().go_forward()
                            {
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
                                egui::ScrollArea::vertical()
                                    .id_salt(format!("file_list_pane_{pane_idx}"))
                                    .show(ui, |ui| {
                                    let ctrl = ui.input(|i| i.modifiers.ctrl);
                                    let mut select_name: Option<String> = None;
                                    let mut nav_target: Option<PathBuf> = None;
                                    for entry in entries {
                                        let is_selected =
                                            pane.active_tab().selected.contains(&entry.name);
                                        let label = if entry.is_dir {
                                            format!("{} {}", entry.name, "[dir]")
                                        } else {
                                            format!("{} ({} bytes)", entry.name, entry.size)
                                        };
                                        let resp = ui.selectable_label(is_selected, label);
                                        if resp.clicked() && ctrl {
                                            select_name = Some(entry.name.clone());
                                        }
                                        if resp.double_clicked() && entry.is_dir {
                                            nav_target = Some(entry.path.clone());
                                        } else if resp.clicked() {
                                            select_name = Some(entry.name.clone());
                                        }
                                    }
                                    if let Some(name) = select_name {
                                        if ctrl {
                                            pane.active_tab_mut().toggle_select(&name);
                                        } else {
                                            pane.active_tab_mut().select_only(&name);
                                        }
                                        self.active_pane = pane_idx;
                                    }
                                    if let Some(target) = nav_target {
                                        pane.active_tab_mut().navigate_to(target);
                                        self.active_pane = pane_idx;
                                        self.dirty = true;
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
