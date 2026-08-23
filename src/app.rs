use crate::fs_ops::{self, ClipboardOp};
use crate::pane::Pane;
use crate::session::{self, WindowGeometry};
use crate::tree;
use eframe::egui;
use rusqlite::Connection;
use std::io;
use std::path::{Path, PathBuf};

/// Modal dialog state (only one open at a time).
#[derive(Debug, Clone)]
enum Dialog {
    Rename { path: PathBuf, name: String },
    NewFolder { name: String },
    NewFile { name: String },
    /// Shown when a copy/paste hits a name collision; user enters a new name.
    DuplicateName { src: PathBuf, dest_dir: PathBuf, suggested: String },
    /// Tab context menu: right-click on a tab to duplicate or close it.
    TabContext { pane_idx: usize, tab_idx: usize },
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
    theme_pref: egui::ThemePreference,
    show_settings: bool,
    /// Which tab is being hovered (pane_idx, tab_idx) for showing the close "×" button.
    tab_hover: Option<(usize, usize)>,
}

fn parse_theme_pref(raw: &str) -> egui::ThemePreference {
    match raw {
        "dark" => egui::ThemePreference::Dark,
        "light" => egui::ThemePreference::Light,
        _ => egui::ThemePreference::System,
    }
}

fn theme_pref_str(pref: egui::ThemePreference) -> &'static str {
    match pref {
        egui::ThemePreference::Dark => "dark",
        egui::ThemePreference::Light => "light",
        egui::ThemePreference::System => "system",
    }
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
        let theme_pref = crate::db::get_theme(&conn)
            .map(|raw| parse_theme_pref(&raw))
            .unwrap_or_default();
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
            theme_pref,
            show_settings: false,
            tab_hover: None,
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

    /// Renders one node of the sidebar folder tree: a collapsing header that
    /// lazily lists its subdirectories when expanded. Ancestor folders of
    /// `active_path` are forced open so the tree stays in sync with (and
    /// highlights) the active pane's current directory. Clicking a header
    /// navigates the active pane to that folder.
    fn show_dir_node(&mut self, ui: &mut egui::Ui, dir: &Path, active_path: &Path) {
        let label = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| dir.display().to_string());
        let is_active = dir == active_path;
        let is_ancestor = active_path.starts_with(dir);
        let mut header = egui::CollapsingHeader::new(if is_active {
            egui::RichText::new(label).strong()
        } else {
            egui::RichText::new(label)
        })
        .id_salt(format!("tree_{}", dir.display()));
        if is_ancestor {
            // Auto-expand every ancestor of the active folder (and itself).
            header = header.open(Some(true));
        } else if dir != active_path {
            // Collapse nodes that aren't ancestors of the active path.
            header = header.open(Some(false));
        }
        let response = header.show(ui, |ui| {
            if let Ok(subdirs) = crate::fs_entry::list_subdirs(dir) {
                for subdir in subdirs {
                    self.show_dir_node(ui, &subdir, active_path);
                }
            }
        });
        if is_active {
            let rect = response.header_response.rect;
            ui.painter().rect_filled(
                rect,
                4.0,
                egui::Color32::from_rgba_premultiplied(80, 160, 255, 40),
            );
        }
        if response.header_response.clicked() {
            self.panes[self.active_pane]
                .active_tab_mut()
                .navigate_to(dir.to_path_buf());
            self.dirty = true;
        }
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

    /// Copies the full path of the selected file/folder to the system clipboard.
    fn copy_filename(&mut self, ctx: &egui::Context) {
        let paths = self.selected_paths();
        if paths.is_empty() {
            self.status = "Nothing selected".into();
            return;
        }
        let text = paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("\n");
        Self::set_clipboard_text(ctx, &text);
        self.status = format!("Copied {} path(s)", paths.len());
    }

    /// Copies the current folder path to the system clipboard.
    fn copy_folder_path(&mut self, ctx: &egui::Context) {
        let dir = self.active_tab_dir();
        let text = dir.to_string_lossy();
        Self::set_clipboard_text(ctx, &text);
        self.status = format!("Copied folder path: {text}");
    }

    /// Writes text to the OS clipboard via egui's output.
    fn set_clipboard_text(ctx: &egui::Context, text: &str) {
        ctx.copy_text(text.to_string());
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
            if self.clipboard_op == Some(ClipboardOp::Cut) && src.parent() == Some(dest.as_path()) {
                continue;
            }
            let result = match self.clipboard_op {
                Some(ClipboardOp::Copy) => fs_ops::copy_item(src, &dest).map(|_| ()),
                _ => fs_ops::move_item(src, &dest),
            };
            match result {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                    // Prompt user for a new name for this item.
                    let stem = src
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "Copy".to_string());
                    let ext = src
                        .extension()
                        .map(|e| {
                            let e = e.to_string_lossy();
                            format!(".{e}")
                        })
                        .unwrap_or_default();
                    self.dialog = Some(Dialog::DuplicateName {
                        src: src.clone(),
                        dest_dir: dest.clone(),
                        suggested: format!("{stem} (copy){ext}"),
                    });
                    return;
                }
                Err(err) => {
                    errors.push(format!("{}: {err}", src.display()));
                }
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
            Dialog::DuplicateName { src, dest_dir, suggested } => {
                let dest = dest_dir.join(suggested);
                match fs_ops::copy_item_to(src, &dest) {
                    Ok(()) => Ok(format!("Copied to {}", dest.display())),
                    Err(err) => Err(format!("Copy failed: {err}")),
                }
            }
            Dialog::TabContext { .. } => Ok(String::new()),
        };
        self.status = match result {
            Ok(msg) if msg.is_empty() => self.status.clone(),
            Ok(msg) => msg,
            Err(msg) => msg,
        };
    }

    fn show_tab_context_menu(&mut self, ctx: &egui::Context) {
        if let Some(Dialog::TabContext { pane_idx, tab_idx }) = self.dialog.take() {
            let path = self.panes[pane_idx].tabs[tab_idx].path.clone();
            let label = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            egui::Window::new(&label)
                .title_bar(false)
                .resizable(false)
                .collapsible(false)
                .show(&ctx, |ui| {
                    if ui.button("Duplicate Tab").clicked() {
                        self.panes[pane_idx].open_tab(path.clone());
                        self.dirty = true;
                        self.dialog = None;
                    }
                    if ui.button("Close Tab").clicked() {
                        self.panes[pane_idx].close_tab(tab_idx);
                        self.dirty = true;
                        self.dialog = None;
                    }
                });
        }
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
        ctx.set_theme(self.theme_pref);
        for theme in [egui::Theme::Dark, egui::Theme::Light] {
            ctx.style_mut_of(theme, |style| {
                style.spacing.item_spacing = egui::vec2(8.0, 6.0);
                style.spacing.button_padding = egui::vec2(10.0, 5.0);
            });
        }

        let screen = ctx.input(|i| i.viewport_rect()).size();
        if (screen - self.last_size).length() > 1.0 {
            self.last_size = screen;
            self.dirty = true;
        }

        // Global shortcuts (disabled while a modal dialog is open so typing
        // in the name field doesn't trigger them).
        if self.dialog.is_none() {
            ctx.input(|i| {
                let ctrl = i.modifiers.ctrl;
                if ctrl && i.key_pressed(egui::Key::X) {
                    self.cut_selection();
                } else if ctrl && i.key_pressed(egui::Key::C) {
                    self.copy_selection();
                } else if ctrl && i.key_pressed(egui::Key::V) {
                    self.paste_clipboard();
                }
            });
        }

        egui::Panel::left("folder_tree").show(ui, |ui| {
            ui.heading("Folders");
            let active_path = self.panes[self.active_pane].active_tab().path.clone();
            for drive in tree::list_drives() {
                self.show_dir_node(ui, &drive, &active_path);
            }
        });

        egui::CentralPanel::default().show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .button("📋 Copy")
                    .on_hover_text("Copy selection (Ctrl+C)")
                    .clicked()
                {
                    self.copy_selection();
                }
                if ui
                    .button("Cut")
                    .on_hover_text("Cut selection (Ctrl+X)")
                    .clicked()
                {
                    self.cut_selection();
                }
                if ui
                    .button("Paste")
                    .on_hover_text("Paste clipboard (Ctrl+V)")
                    .clicked()
                {
                    self.paste_clipboard();
                }
                ui.separator();
                if ui
                    .button("🗑 Delete")
                    .on_hover_text("Send selection to Recycle Bin")
                    .clicked()
                {
                    self.delete_selection();
                }
                if ui
                    .button("Rename")
                    .on_hover_text("Rename the selected item")
                    .clicked()
                {
                    self.begin_rename();
                }
                ui.separator();
                if ui
                    .button("Copy Filename")
                    .on_hover_text("Copy full path of selected file")
                    .clicked()
                {
                    self.copy_filename(&ctx);
                }
                if ui
                    .button("Copy Folder Path")
                    .on_hover_text("Copy current folder path")
                    .clicked()
                {
                    self.copy_folder_path(&ctx);
                }
                ui.separator();
                if ui
                    .button("🗀 New Folder")
                    .on_hover_text("Create a new folder here")
                    .clicked()
                {
                    self.dialog = Some(Dialog::NewFolder {
                        name: String::new(),
                    });
                }
                if ui
                    .button("🗋 New File")
                    .on_hover_text("Create a new file here")
                    .clicked()
                {
                    self.dialog = Some(Dialog::NewFile {
                        name: String::new(),
                    });
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button("⚙ Settings")
                        .on_hover_text("Preferences")
                        .clicked()
                    {
                        self.show_settings = true;
                    }
                });
            });
            if !self.status.is_empty() {
                ui.label(egui::RichText::new(&self.status).weak());
            }

            let mut settings_open = self.show_settings;
            if settings_open {
                egui::Window::new("Settings")
                    .open(&mut settings_open)
                    .resizable(false)
                    .show(&ctx, |ui| {
                        ui.label(egui::RichText::new("Theme").strong());
                        let mut pref = self.theme_pref;
                        pref.radio_buttons(ui);
                        if pref != self.theme_pref {
                            self.theme_pref = pref;
                            let _ = crate::db::set_theme(&self.conn, theme_pref_str(pref));
                        }
                    });
            }
            self.show_settings = settings_open;

            // Handle tab context menu separately (non-modal, not a text-dialog).
            if matches!(&self.dialog, Some(Dialog::TabContext { .. })) {
                self.show_tab_context_menu(&ctx);
            }

            // Modal dialogs (rename / new folder / new file / duplicate name).
            if !matches!(&self.dialog, Some(Dialog::TabContext { .. })) {
                let mut commit = false;
                let mut cancel = false;
                if let Some(dialog) = &mut self.dialog {
                    // Extract src filename before borrowing dialog further.
                    let src_label: Option<String> = if let Dialog::DuplicateName { src, .. } = dialog {
                        src.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                    } else {
                        None
                    };
                    let (title, name) = match dialog {
                        Dialog::Rename { name, .. } => ("Rename", name),
                        Dialog::NewFolder { name } => ("New Folder", name),
                        Dialog::NewFile { name } => ("New File", name),
                        Dialog::DuplicateName { suggested, .. } => {
                            ("Duplicate Name", suggested)
                        }
                        Dialog::TabContext { .. } => unreachable!(),
                    };
                    egui::Window::new(title).show(&ctx, |ui| {
                        if let Some(ref label) = src_label {
                            ui.label(label.as_str());
                        }
                        let edit = ui.text_edit_singleline(name);
                        edit.request_focus();
                        commit = edit.lost_focus()
                            && ui.input(|i| i.key_pressed(egui::Key::Enter));
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
                        let mut tab_context_menu: Option<usize> = None;
                        ui.horizontal(|ui| {
                            for (tab_idx, tab) in pane.tabs.iter().enumerate() {
                                let label = tab
                                    .path
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| tab.path.display().to_string());
                                let tab_resp = ui
                                    .selectable_label(tab_idx == pane.active_tab, label);
                                if tab_resp.clicked() {
                                    tab_clicked = Some(tab_idx);
                                }
                                // Right-click on tab for context menu
                                if tab_resp.secondary_clicked() {
                                    tab_context_menu = Some(tab_idx);
                                }
                                // Track hover for close button
                                if tab_resp.contains_pointer() {
                                    self.tab_hover = Some((pane_idx, tab_idx));
                                }
                                // Show × close button only when this tab is hovered
                                let is_hovered = self.tab_hover == Some((pane_idx, tab_idx));
                                if is_hovered {
                                    if ui.small_button("×").on_hover_text("Close tab").clicked() {
                                        tab_closed = Some(tab_idx);
                                    }
                                }
                            }
                            if ui.button("+").clicked() {
                                tab_opened = true;
                            }
                        });
                        // Show tab context menu via dialog
                        if let Some(idx) = tab_context_menu {
                            self.dialog = Some(Dialog::TabContext { pane_idx, tab_idx: idx });
                        }
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
                            if ui
                                .button("⬅")
                                .on_hover_text("Back")
                                .clicked()
                                && pane.active_tab_mut().go_back()
                            {
                                self.dirty = true;
                            }
                            if ui
                                .button("➡")
                                .on_hover_text("Forward")
                                .clicked()
                                && pane.active_tab_mut().go_forward()
                            {
                                self.dirty = true;
                            }
                            if ui.button("⬆").on_hover_text("Up").clicked() {
                                if let Some(parent) = current_path.parent() {
                                    pane.active_tab_mut().navigate_to(parent.to_path_buf());
                                    self.dirty = true;
                                }
                            }
                        });

                        match crate::fs_entry::list_dir(&current_path) {
                            Ok(mut entries) => {
                                let (sort_col, sort_asc) = {
                                    let tab = pane.active_tab();
                                    (tab.sort_col.clone(), tab.sort_asc)
                                };
                                crate::fs_entry::sort_entries(&mut entries, &sort_col, sort_asc);
                                let ctrl = ui.input(|i| i.modifiers.ctrl);
                                let col_w = pane.active_tab().col_widths;

                                let mut select_name: Option<String> = None;
                                let mut nav_target: Option<PathBuf> = None;
                                let mut context_menu_name: Option<String> = None;
                                let mut sort_clicked: Option<String> = None;
                                let mut live_widths: Option<Vec<f32>> = None;

                                egui_extras::TableBuilder::new(ui)
                                    .id_salt(format!("file_table_pane_{pane_idx}"))
                                    .striped(true)
                                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                                    .column(
                                        egui_extras::Column::initial(col_w[0])
                                            .resizable(true)
                                            .clip(true)
                                            .range(40.0..=2000.0),
                                    )
                                    .column(
                                        egui_extras::Column::initial(col_w[1])
                                            .resizable(true)
                                            .clip(true)
                                            .range(40.0..=2000.0),
                                    )
                                    .column(
                                        egui_extras::Column::initial(col_w[2])
                                            .resizable(true)
                                            .clip(true)
                                            .range(30.0..=2000.0),
                                    )
                                    .column(
                                        egui_extras::Column::initial(col_w[3])
                                            .resizable(true)
                                            .clip(true)
                                            .range(20.0..=500.0),
                                    )
                                    .header(20.0, |mut header| {
                                        header.col(|ui| {
                                            sort_header(ui, "Name", "name", &sort_col, sort_asc, &mut sort_clicked);
                                        });
                                        header.col(|ui| {
                                            sort_header(ui, "Modified", "modified", &sort_col, sort_asc, &mut sort_clicked);
                                        });
                                        header.col(|ui| {
                                            sort_header(ui, "Size", "size", &sort_col, sort_asc, &mut sort_clicked);
                                        });
                                        header.col(|ui| {
                                            sort_header(ui, "Archive", "archive", &sort_col, sort_asc, &mut sort_clicked);
                                        });
                                    })
                                    .body(|body| {
                                        live_widths = Some(body.widths().to_vec());
                                        body.rows(18.0, entries.len(), |mut row| {
                                            let entry = &entries[row.index()];
                                            let is_selected = pane
                                                .active_tab()
                                                .selected
                                                .contains(&entry.name);

                                            row.set_selected(is_selected);

                                            row.col(|ui| {
                                                let resp = ui.selectable_label(
                                                    is_selected,
                                                    &entry.name,
                                                );
                                                register_entry_click(
                                                    &resp, entry,
                                                    &mut select_name,
                                                    &mut nav_target,
                                                );
                                                if resp.secondary_clicked() {
                                                    context_menu_name = Some(entry.name.clone());
                                                    self.active_pane = pane_idx;
                                                }
                                            });
                                            row.col(|ui| {
                                                let text = entry
                                                    .modified
                                                    .map(|t| {
                                                        chrono::DateTime::<chrono::Local>::from(t)
                                                            .format("%Y-%m-%d %H:%M")
                                                            .to_string()
                                                    })
                                                    .unwrap_or_default();
                                                ui.label(text);
                                            });
                                            row.col(|ui| {
                                                let size_text = if entry.is_dir {
                                                    String::new()
                                                } else {
                                                    format!("{}", entry.size)
                                                };
                                                ui.label(size_text);
                                            });
                                            row.col(|ui| {
                                                if entry.archive {
                                                    ui.label("A");
                                                }
                                            });
                                        });
                                    });

                                if let Some(widths) = live_widths {
                                    let w: [f32; 4] = [
                                        widths.first().copied().unwrap_or(col_w[0]),
                                        widths.get(1).copied().unwrap_or(col_w[1]),
                                        widths.get(2).copied().unwrap_or(col_w[2]),
                                        widths.get(3).copied().unwrap_or(col_w[3]),
                                    ];
                                    if (w[0] - col_w[0]).abs() > 0.5
                                        || (w[1] - col_w[1]).abs() > 0.5
                                        || (w[2] - col_w[2]).abs() > 0.5
                                        || (w[3] - col_w[3]).abs() > 0.5
                                    {
                                        pane.active_tab_mut().col_widths = w;
                                        self.dirty = true;
                                    }
                                }
                                if let Some(col) = sort_clicked {
                                    let tab = pane.active_tab_mut();
                                    if tab.sort_col == col {
                                        tab.sort_asc = !tab.sort_asc;
                                    } else {
                                        tab.sort_col = col;
                                        tab.sort_asc = true;
                                    }
                                    self.dirty = true;
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
                                if let Some(_name) = context_menu_name {
                                    let paths: Vec<PathBuf> = pane
                                        .active_tab()
                                        .selected
                                        .iter()
                                        .map(|n| current_path.join(n))
                                        .collect();
                                    let is_single = paths.len() <= 1;
                                    let ctx = ui.ctx().clone();
                                    egui::Area::new(egui::Id::new(("ctx_menu", pane_idx)))
                                        .fixed_pos(ui.input(|i| i.pointer.hover_pos().unwrap_or_default()))
                                        .show(&ctx, |ui| {
                                            egui::Frame::popup(ui.style()).show(ui, |ui| {
                                                ui.set_min_width(140.0);
                                                if ui.button("Copy").clicked() {
                                                    self.copy_selection();
                                                    self.dialog = None;
                                                }
                                                if ui.button("Cut").clicked() {
                                                    self.cut_selection();
                                                    self.dialog = None;
                                                }
                                                if ui.button("Paste").clicked() {
                                                    self.paste_clipboard();
                                                    self.dialog = None;
                                                }
                                                ui.separator();
                                                if is_single {
                                                    if ui.button("Rename").clicked() {
                                                        self.begin_rename();
                                                        self.dialog = None;
                                                    }
                                                }
                                                if ui.button("Delete").clicked() {
                                                    self.delete_selection();
                                                    self.dialog = None;
                                                }
                                                ui.separator();
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
                                                ui.separator();
                                                if ui.button("Copy Filename").clicked() {
                                                    if let Some(first) = paths.first() {
                                                        let name = first
                                                            .file_name()
                                                            .map(|n| n.to_string_lossy().into_owned())
                                                            .unwrap_or_default();
                                                        ctx.copy_text(name);
                                                    }
                                                    self.dialog = None;
                                                }
                                                if ui.button("Copy Folder Path").clicked() {
                                                    ctx.copy_text(current_path.display().to_string());
                                                    self.dialog = None;
                                                }
                                            });
                                        });
                                }
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

/// Clickable column header that shows a sort-direction arrow when its
/// column is the active sort column.
fn sort_header(
    ui: &mut egui::Ui,
    label: &str,
    col: &str,
    current_col: &str,
    asc: bool,
    clicked: &mut Option<String>,
) {
    let arrow = if current_col == col {
        if asc { " ▲" } else { " ▼" }
    } else {
        ""
    };
    if ui
        .selectable_label(current_col == col, format!("{label}{arrow}"))
        .clicked()
    {
        *clicked = Some(col.to_string());
    }
}

fn register_entry_click(
    resp: &egui::Response,
    entry: &crate::fs_entry::FsEntry,
    select_name: &mut Option<String>,
    nav_target: &mut Option<PathBuf>,
) {
    if resp.double_clicked() && entry.is_dir {
        *nav_target = Some(entry.path.clone());
    } else if resp.clicked() {
        *select_name = Some(entry.name.clone());
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
