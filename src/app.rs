use crate::archive;
use crate::fs_ops::{self, ClipboardOp};
use crate::pane::Pane;
use crate::progress::{self, BackgroundOp, OpStatus};
use crate::search;
use crate::session::{self, WindowGeometry};
use crate::tab::ViewMode;
use crate::tree;
use eframe::egui;
use rusqlite::Connection;
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
    /// Find dialog for searching files.
    Find { query: String, results: Vec<PathBuf>, search_path: PathBuf },
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
    font_size: f32,
    /// Index of the last selected entry (anchor for Shift+click range selection).
    last_selected_index: Option<usize>,
    /// Previous active path for detecting navigation changes (tree auto-expand).
    prev_active_path: Option<PathBuf>,
    font_family: String,
    /// The font_family that was last handed to `ctx.set_fonts`, so we only
    /// rebuild the font atlas when the setting actually changes.
    fonts_applied_family: Option<String>,
    /// Last known top-left window position in screen points, for persistence.
    last_pos: Option<(f32, f32)>,
    /// Background file operation in progress (copy/move/delete).
    background_op: Option<BackgroundOp>,
    /// Editable address bar text for this pane.
    /// Index of the pane whose address bar is currently focused (being edited).
    focused_address_pane: Option<usize>,
    /// Cached network server UNC paths for the sidebar tree.
    network_servers: Vec<PathBuf>,
    /// Favourite folder paths for quick access.
    favourites: Vec<String>,
}

/// Maps a Settings font-family choice to its file on disk under the Windows
/// Fonts folder. `None` means "use the embedded Inter font" (either because
/// the family IS Inter, or it has no known system file).
fn system_font_path(family: &str) -> Option<PathBuf> {
    let filename = match family {
        "Segoe UI" => "segoeui.ttf",
        "Arial" => "arial.ttf",
        "Times New Roman" => "times.ttf",
        "Courier New" => "cour.ttf",
        _ => return None,
    };
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".to_string());
    Some(PathBuf::from(windir).join("Fonts").join(filename))
}

/// Applies the chosen font family to egui, falling back to the embedded
/// Inter font (always available) if the family is "Inter" itself or its
/// system font file can't be read. Writes a status message on fallback.
fn apply_fonts(ctx: &egui::Context, family: &str, status: &mut String) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "inter".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../fonts/Inter-Regular.ttf"
        ))),
    );
    let mut active = "inter".to_owned();
    if let Some(path) = system_font_path(family) {
        match std::fs::read(&path) {
            Ok(bytes) => {
                fonts
                    .font_data
                    .insert("custom".to_owned(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
                active = "custom".to_owned();
            }
            Err(_) => {
                *status = format!("Font '{family}' not found on this system, using Inter");
            }
        }
    }
    for family_key in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .get_mut(&family_key)
            .unwrap()
            .insert(0, active.clone());
    }
    ctx.set_fonts(fonts);
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
        let font_size = crate::db::get_font_size(&conn).unwrap_or(14.0);
        let font_family = crate::db::get_font_family(&conn)
            .unwrap_or_else(|| "Inter".to_string());
        let favourites = crate::db::get_favourites(&conn);
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
            font_size,
            last_selected_index: None,
            prev_active_path: None,
            font_family,
            fonts_applied_family: None,
            last_pos: None,
            background_op: None,
            focused_address_pane: None,
            network_servers: tree::list_network_servers(),
            favourites,
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
            pos_x: self.last_pos.map(|p| p.0),
            pos_y: self.last_pos.map(|p| p.1),
            monitor_name: None,
        };
        let _ = session::save_session(&self.conn, &window, &self.panes, self.active_pane);
    }

    fn active_tab_dir(&self) -> PathBuf {
        self.panes[self.active_pane].active_tab().path.clone()
    }

    /// Adds the current active folder to favourites.
    fn add_favourite(&mut self) {
        let path = self.active_tab_dir();
        let path_str = path.display().to_string();
        if crate::db::add_favourite(&self.conn, &path_str).is_ok() {
            self.favourites = crate::db::get_favourites(&self.conn);
            self.status = format!("Added to favourites: {}", path.display());
        }
    }

    /// Removes a path from favourites.
    fn remove_favourite(&mut self, path: &str) {
        if crate::db::remove_favourite(&self.conn, path).is_ok() {
            self.favourites = crate::db::get_favourites(&self.conn);
            self.status = format!("Removed from favourites");
        }
    }

    /// Searches for files matching the query in the given directory (recursively).
    fn find_files(&self, dir: &Path, query: &str) -> Vec<PathBuf> {
        let mut results = Vec::new();
        let query_lower = query.to_lowercase();
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().to_lowercase();
                if name.contains(&query_lower) {
                    results.push(path.clone());
                }
                if path.is_dir() && results.len() < 500 {
                    results.extend(self.find_files(&path, query));
                }
            }
        }
        results
    }

    /// Renders one node of the sidebar folder tree: a collapsing header that
    /// lazily lists its subdirectories when expanded. Clicking a header
    /// toggles expand/collapse; navigating only happens when expanding.
    /// When `force_expand` is true, ancestor nodes are forced open (used
    /// after navigation to reveal the active path in the tree).
    fn show_dir_node(&mut self, ui: &mut egui::Ui, dir: &Path, active_path: &Path, force_expand: bool) {
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
        if force_expand && is_ancestor {
            header = header.open(Some(true));
        }
        let response = header.show(ui, |ui| {
            if let Ok(subdirs) = crate::fs_entry::list_subdirs(dir) {
                for subdir in subdirs {
                    self.show_dir_node(ui, &subdir, active_path, force_expand);
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
        // Navigate only when the folder is expanded (body visible), not when collapsing
        if response.header_response.clicked() && response.body_response.is_some() {
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

    /// Pastes the clipboard into the active tab's directory.
    ///
    /// Name collisions are checked up front with a cheap `Path::exists`
    /// (no recursive walk), preserving the original one-at-a-time
    /// duplicate-name prompt: the first colliding item stops the paste and
    /// opens `Dialog::DuplicateName`, exactly as before. Once there are no
    /// collisions left, the whole batch runs as a single background
    /// operation with a progress bar, rather than blocking the UI thread —
    /// see `progress::copy_items_bg`/`move_items_bg`.
    fn paste_clipboard(&mut self) {
        if self.clipboard.is_empty() {
            self.status = "Clipboard is empty".into();
            return;
        }
        let dest = self.active_tab_dir();
        let op = self.clipboard_op;

        let mut items: Vec<PathBuf> = Vec::new();
        for src in &self.clipboard {
            // Cutting into the same folder the item already lives in: no-op.
            if op == Some(ClipboardOp::Cut) && src.parent() == Some(dest.as_path()) {
                continue;
            }
            items.push(src.clone());
        }
        if items.is_empty() {
            self.status = "Nothing to paste".into();
            return;
        }

        for src in &items {
            let name = match src.file_name() {
                Some(n) => n,
                None => continue,
            };
            if dest.join(name).exists() {
                let stem = src
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Copy".to_string());
                let ext = src
                    .extension()
                    .map(|e| format!(".{}", e.to_string_lossy()))
                    .unwrap_or_default();
                self.dialog = Some(Dialog::DuplicateName {
                    src: src.clone(),
                    dest_dir: dest.clone(),
                    suggested: format!("{stem} (copy){ext}"),
                });
                return;
            }
        }

        self.background_op = Some(match op {
            Some(ClipboardOp::Copy) => progress::copy_items_bg(items, dest.clone()),
            _ => progress::move_items_bg(items, dest.clone()),
        });
        self.status = format!("Pasting into {}…", dest.display());
        if op == Some(ClipboardOp::Cut) {
            self.clipboard.clear();
        }
        self.panes[self.active_pane]
            .active_tab_mut()
            .clear_selection();
    }

    fn delete_selection(&mut self) {
        let paths = self.selected_paths();
        if paths.is_empty() {
            self.status = "Nothing selected".into();
            return;
        }
        self.panes[self.active_pane]
            .active_tab_mut()
            .clear_selection();
        self.status = format!("Deleting {} item(s)…", paths.len());
        self.background_op = Some(progress::delete_to_trash_bg(paths));
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

    /// Extracts the selected archive into the current directory.
    fn extract_here(&mut self) {
        let paths = self.selected_paths();
        if paths.len() != 1 {
            self.status = "Select exactly one archive to extract".into();
            return;
        }
        let archive = &paths[0];
        if !archive::is_archive(archive) {
            self.status = "Selected file is not a supported archive".into();
            return;
        }
        let dest = self.active_tab_dir();
        match archive::extract_archive(archive, &dest) {
            Ok(()) => {
                self.status = format!("Extracted into {}", dest.display());
                self.panes[self.active_pane].active_tab_mut().clear_selection();
                self.dirty = true;
            }
            Err(e) => self.status = format!("Extraction failed: {e}"),
        }
    }

    /// Extracts the selected archive to a user-chosen destination folder.
    fn extract_to(&mut self) {
        let paths = self.selected_paths();
        if paths.len() != 1 {
            self.status = "Select exactly one archive to extract".into();
            return;
        }
        let archive = &paths[0];
        if !archive::is_archive(archive) {
            self.status = "Selected file is not a supported archive".into();
            return;
        }
        let dialog = rfd::FileDialog::new()
            .set_title("Choose Extraction Destination")
            .pick_folder();
        if let Some(dest) = dialog {
            match archive::extract_archive(archive, &dest) {
                Ok(()) => {
                    self.status = format!("Extracted into {}", dest.display());
                    self.panes[self.active_pane].active_tab_mut().clear_selection();
                    self.dirty = true;
                }
                Err(e) => self.status = format!("Extraction failed: {e}"),
            }
        }
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
            Dialog::TabContext { .. } | Dialog::Find { .. } => Ok(String::new()),
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
                let font_id = egui::FontId::new(self.font_size, egui::FontFamily::Proportional);
                style
                    .text_styles
                    .insert(egui::TextStyle::Heading, font_id.clone());
                style.text_styles.insert(egui::TextStyle::Body, font_id);
                style.text_styles.insert(
                    egui::TextStyle::Monospace,
                    egui::FontId::new(self.font_size, egui::FontFamily::Monospace),
                );
            });
        }
        // Override dark theme colors for better contrast
        ctx.style_mut_of(egui::Theme::Dark, |style| {
            let visuals = &mut style.visuals;
            visuals.override_text_color = Some(egui::Color32::from_rgb(240, 240, 240));
            visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(220, 220, 220));
            visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(240, 240, 240));
            visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);
            visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);
            visuals.widgets.open.fg_stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);
            visuals.selection.stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(120, 180, 255));
        });

        if self.fonts_applied_family.as_deref() != Some(self.font_family.as_str()) {
            apply_fonts(&ctx, &self.font_family, &mut self.status);
            self.fonts_applied_family = Some(self.font_family.clone());
        }

        // Ctrl+scroll (or pinch): use egui's own whole-UI zoom rather than
        // hand-rolling font-size math. This scales spacing/row heights along
        // with text, so nothing clips, and egui already excludes this input
        // from the scroll areas' own wheel handling (see `zoom_delta`), so
        // there's no double-scroll.
        let zoom = ctx.input(|i| i.zoom_delta());
        if (zoom - 1.0).abs() > f32::EPSILON {
            let new_zoom = (ctx.zoom_factor() * zoom).clamp(0.5, 2.5);
            ctx.set_zoom_factor(new_zoom);
        }

        let screen = ctx.input(|i| i.viewport_rect()).size();
        if (screen - self.last_size).length() > 1.0 {
            self.last_size = screen;
            self.dirty = true;
        }
        if let Some(outer_rect) = ctx.input(|i| i.viewport().outer_rect) {
            let pos = (outer_rect.min.x, outer_rect.min.y);
            if self.last_pos != Some(pos) {
                self.last_pos = Some(pos);
                self.dirty = true;
            }
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
                } else if ctrl && i.key_pressed(egui::Key::F) {
                    let search_path = self.active_tab_dir();
                    self.dialog = Some(Dialog::Find {
                        query: String::new(),
                        results: Vec::new(),
                        search_path,
                    });
                }
            });
        }

        egui::Panel::left("folder_tree").show(ui, |ui| {
            ui.heading("Folders");

            egui::ScrollArea::vertical().show(ui, |ui| {
                let active_path = self.panes[self.active_pane].active_tab().path.clone();
                // Detect navigation: force-expand tree when active path changes
                let force_expand = self.prev_active_path.as_ref() != Some(&active_path);
                if force_expand {
                    self.prev_active_path = Some(active_path.clone());
                }

                // Favourites section
                if !self.favourites.is_empty() {
                    ui.label(egui::RichText::new("★ Favourites").strong());
                    let favourites = self.favourites.clone();
                    for fav_path in &favourites {
                        let path = std::path::Path::new(fav_path);
                        let label = path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| fav_path.clone());
                        let is_active = fav_path == &active_path.display().to_string();
                        let btn = ui.selectable_label(is_active, &label);
                        if btn.clicked() {
                            self.panes[self.active_pane]
                                .active_tab_mut()
                                .navigate_to(path.to_path_buf());
                            self.dirty = true;
                        }
                        // Right-click to remove from favourites
                        let fav_path_owned = fav_path.clone();
                        btn.context_menu(|ui| {
                            if ui.button("Remove from Favourites").clicked() {
                                self.remove_favourite(&fav_path_owned);
                                ui.close();
                            }
                        });
                    }
                    ui.separator();
                }

                for drive in tree::list_drives() {
                    self.show_dir_node(ui, &drive, &active_path, force_expand);
                }
                if !self.network_servers.is_empty() {
                    ui.separator();
                    ui.label(egui::RichText::new("Network").strong());
                    for server in &self.network_servers.clone() {
                        self.show_dir_node(ui, server, &active_path, force_expand);
                    }
                }
            });
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

                // Extract buttons: enabled only when exactly one archive is selected.
                let paths = self.selected_paths();
                let single_archive = paths.len() == 1 && archive::is_archive(&paths[0]);
                ui.add_enabled_ui(single_archive, |ui| {
                    if ui
                        .button("📦 Extract Here")
                        .on_hover_text("Extract archive into current folder")
                        .clicked()
                    {
                        self.extract_here();
                    }
                    if ui
                        .button("📦 Extract to...")
                        .on_hover_text("Extract archive to a chosen folder")
                        .clicked()
                    {
                        self.extract_to();
                    }
                });

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
                let current_path = self.active_tab_dir();
                let is_fav = crate::db::is_favourite(&self.conn, &current_path.display().to_string());
                let fav_label = if is_fav { "★ Un favourite" } else { "☆ Add to Favourites" };
                if ui
                    .button(fav_label)
                    .on_hover_text("Toggle favourite for current folder")
                    .clicked()
                {
                    if is_fav {
                        self.remove_favourite(&current_path.display().to_string());
                    } else {
                        self.add_favourite();
                    }
                }
                ui.separator();
                if ui
                    .button("🔍 Find")
                    .on_hover_text("Find files in current folder (Ctrl+F)")
                    .clicked()
                {
                    let search_path = self.active_tab_dir();
                    self.dialog = Some(Dialog::Find {
                        query: String::new(),
                        results: Vec::new(),
                        search_path,
                    });
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

                        ui.add_space(8.0);
                        ui.label(egui::RichText::new("Font").strong());

                        // Font family selection
                        ui.horizontal(|ui| {
                            ui.label("Family:");
                            let fonts = ["Inter", "Segoe UI", "Arial", "Helvetica", "Times New Roman", "Courier New"];
                            let current_idx = fonts.iter().position(|&f| f == self.font_family).unwrap_or(0);
                            let mut new_idx = current_idx;
                            egui::ComboBox::from_id_salt("font_family_combo")
                                .selected_text(&self.font_family)
                                .show_ui(ui, |ui| {
                                    for (i, font) in fonts.iter().enumerate() {
                                        if ui.selectable_label(i == current_idx, *font).clicked() {
                                            new_idx = i;
                                        }
                                    }
                                });
                            if new_idx != current_idx {
                                self.font_family = fonts[new_idx].to_string();
                                let _ = crate::db::set_font_family(&self.conn, &self.font_family);
                            }
                        });

                        // Font size slider
                        ui.horizontal(|ui| {
                            ui.label("Size:");
                            let response = ui.add(
                                egui::Slider::new(&mut self.font_size, 8.0..=24.0)
                                    .step_by(0.5)
                                    .suffix("px")
                            );
                            if response.changed() {
                                let _ = crate::db::set_font_size(&self.conn, self.font_size);
                            }
                        });
                    });
            }
            self.show_settings = settings_open;

            // Handle tab context menu separately (non-modal, not a text-dialog).
            if matches!(&self.dialog, Some(Dialog::TabContext { .. })) {
                self.show_tab_context_menu(&ctx);
            }

            // Progress modal for background operations
            if let Some(ref mut op) = self.background_op {
                let still_running = op.poll();
                let title = "Processing...";
                let progress_text = format!(
                    "{}/{} files — {}",
                    op.progress.files_done, op.progress.files_total, op.progress.current_file
                );
                let fraction = if op.progress.files_total > 0 {
                    op.progress.files_done as f32 / op.progress.files_total as f32
                } else {
                    0.0
                };

                egui::Window::new(title)
                    .title_bar(true)
                    .resizable(false)
                    .collapsible(false)
                    .show(&ctx, |ui| {
                        ui.label(&progress_text);
                        ui.add(egui::ProgressBar::new(fraction).animate(still_running));
                        if !still_running {
                            match &op.status {
                                OpStatus::Completed(msg) => {
                                    self.status = msg.clone();
                                }
                                OpStatus::Failed(msg) => {
                                    self.status = msg.clone();
                                }
                                _ => {}
                            }
                        }
                    });

                if !still_running {
                    self.background_op = None;
                    self.dirty = true;
                }
            }

            // Modal dialogs (rename / new folder / new file / duplicate name / find).
            if !matches!(&self.dialog, Some(Dialog::TabContext { .. })) {
                // Handle Find dialog separately (it has its own UI)
                let mut find_close = false;
                let mut find_navigate: Option<PathBuf> = None;
                let mut find_search_query: Option<String> = None;
                if let Some(Dialog::Find { query, results, search_path }) = &mut self.dialog {
                    let search_path_clone = search_path.clone();
                    // Show the dialog UI, capturing any actions needed
                    let mut dialog_ui = |ui: &mut egui::Ui, query: &mut String, results: &mut Vec<PathBuf>| {
                        ui.horizontal(|ui| {
                            ui.label("Search in:");
                            ui.label(search_path_clone.display().to_string());
                        });
                        ui.horizontal(|ui| {
                            ui.label("Find:");
                            let edit = ui.text_edit_singleline(query);
                            if edit.changed() && !query.is_empty() {
                                find_search_query = Some(query.clone());
                            }
                        });
                        ui.label(format!("{} result(s) found", results.len()));
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            for path in results.iter().take(100) {
                                let label = path.file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| path.display().to_string());
                                let path_str = path.display().to_string();
                                if ui.selectable_label(false, &label).clicked() {
                                    if let Some(parent) = path.parent() {
                                        find_navigate = Some(parent.to_path_buf());
                                    }
                                    find_close = true;
                                }
                                ui.label(egui::RichText::new(&path_str).weak().small());
                            }
                        });
                        if ui.button("Close").clicked() {
                            find_close = true;
                        }
                    };
                    egui::Window::new("Find Files")
                        .resizable(true)
                        .default_width(400.0)
                        .default_height(300.0)
                        .show(&ctx, |ui| {
                            dialog_ui(ui, query, results);
                        });
                }
                if find_close {
                    self.dialog = None;
                }
                if let Some(parent) = find_navigate {
                    self.panes[self.active_pane]
                        .active_tab_mut()
                        .navigate_to(parent);
                    self.dirty = true;
                }
                // Execute deferred search if the query changed
                if let Some(query) = find_search_query {
                    if let Some(Dialog::Find { search_path, .. }) = &self.dialog {
                        let sp = search_path.clone();
                        let found = self.find_files(&sp, &query);
                        if let Some(Dialog::Find { results, .. }) = &mut self.dialog {
                            *results = found;
                        }
                    }
                }
                if self.dialog.is_some() && !find_close {
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
                            Dialog::Find { .. } | Dialog::TabContext { .. } => unreachable!(),
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
            }

            ui.columns(2, |columns| {
                for pane_idx in 0..2 {
                    let is_active = pane_idx == self.active_pane;
                    columns[pane_idx].group(|ui| {
                        // Click on pane background to set as active
                        let pane_resp = ui.interact(ui.max_rect(), egui::Id::new(("pane_bg", pane_idx)), egui::Sense::click());
                        if pane_resp.clicked() {
                            self.active_pane = pane_idx;
                            self.dirty = true;
                        }

                        let pane = &mut self.panes[pane_idx];
                        let mut tab_clicked = None;
                        let mut tab_closed = None;
                        let mut tab_opened = false;
                        let mut tab_context_menu: Option<usize> = None;
                        let mut any_tab_hovered = false;
                        ui.horizontal(|ui| {
                            for (tab_idx, tab) in pane.tabs.iter().enumerate() {
                                let label = tab
                                    .path
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| tab.path.display().to_string());
                                let is_tab_active = tab_idx == pane.active_tab;
                                // Only orange fill for active tab of active pane
                                let is_orange = is_active && is_tab_active;
                                let tab_resp = ui
                                    .selectable_label(is_tab_active, &label);
                                // Paint active tab fill color (orange) with black text
                                if is_orange {
                                    let rect = tab_resp.rect;
                                    ui.painter().rect_filled(
                                        rect,
                                        2.0,
                                        egui::Color32::from_rgb(255, 165, 0),
                                    );
                                    // Overdraw text in black
                                    ui.painter().text(
                                        rect.center(),
                                        egui::Align2::CENTER_CENTER,
                                        &label,
                                        egui::FontId::proportional(ui.style().text_styles.get(&egui::TextStyle::Body).map_or(14.0, |f| f.size)),
                                        egui::Color32::BLACK,
                                    );
                                }
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
                                    any_tab_hovered = true;
                                }
                                // Show × close button on the tab's right edge when hovered
                                let is_hovered = self.tab_hover == Some((pane_idx, tab_idx));
                                if is_hovered {
                                    let rect = tab_resp.rect;
                                    let btn_size = 14.0;
                                    let btn_rect = egui::Rect::from_min_size(
                                        egui::pos2(rect.max.x - btn_size - 2.0, rect.center().y - btn_size / 2.0),
                                        egui::vec2(btn_size, btn_size),
                                    );
                                    let btn_resp = ui.interact(btn_rect, egui::Id::new(("tab_close", pane_idx, tab_idx)), egui::Sense::click());
                                    // Paint close button background
                                    ui.painter().rect_filled(btn_rect, 2.0, egui::Color32::from_rgb(180, 40, 40));
                                    ui.painter().text(
                                        btn_rect.center(),
                                        egui::Align2::CENTER_CENTER,
                                        "×",
                                        egui::FontId::proportional(btn_size - 2.0),
                                        egui::Color32::WHITE,
                                    );
                                    if btn_resp.clicked() {
                                        tab_closed = Some(tab_idx);
                                    }
                                }
                            }
                            if ui.button("+").clicked() {
                                tab_opened = true;
                            }
                        });
                        // Clear hover when no tab is being hovered
                        if !any_tab_hovered {
                            self.tab_hover = None;
                        }
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
                        // Sync address bar display when active pane changes
                        if pane_idx == self.active_pane {
                            let display = current_path.display().to_string();
                            if pane.address_bar != display {
                                // Only update if this pane's address bar isn't focused
                                if self.focused_address_pane != Some(pane_idx) {
                                    pane.address_bar = display;
                                }
                            }
                        }
                        ui.horizontal(|ui| {
                            ui.label("📁");
                            let address_id = egui::Id::new(("address_bar", pane_idx));
                            let address_resp = ui.add(
                                egui::TextEdit::singleline(&mut pane.address_bar)
                                    .id(address_id)
                                    .desired_width(f32::INFINITY)
                                    .hint_text("Type a path and press Enter..."),
                            );
                            // Track which pane's address bar has focus
                            if address_resp.has_focus() {
                                self.focused_address_pane = Some(pane_idx);
                            } else if self.focused_address_pane == Some(pane_idx) {
                                self.focused_address_pane = None;
                            }
                            if address_resp.lost_focus()
                                && ui.input(|i| i.key_pressed(egui::Key::Enter))
                            {
                                let target = PathBuf::from(pane.address_bar.trim());
                                if target.exists() {
                                    pane.active_tab_mut().navigate_to(target);
                                    self.active_pane = pane_idx;
                                    self.dirty = true;
                                } else {
                                    self.status = format!("Path not found: {}", pane.address_bar.trim());
                                }
                            }
                        });

                        // Search/filter field — per-tab, so each pane/tab
                        // filters independently instead of sharing one query.
                        ui.horizontal(|ui| {
                            ui.label("🔍");
                            let tab = pane.active_tab_mut();
                            let filter_active = !tab.filter.is_empty();
                            let accent = ui.visuals().selection.bg_fill;
                            let mut text_edit = egui::TextEdit::singleline(&mut tab.filter)
                                .hint_text("Filter files...")
                                .desired_width(200.0);
                            if filter_active {
                                // Highlight the box while a filter is narrowing the
                                // listing, so it's obvious why entries are missing.
                                text_edit = text_edit
                                    .background_color(accent.gamma_multiply(0.18))
                                    .text_color(ui.visuals().strong_text_color());
                            }
                            let search_resp = ui.add(text_edit);
                            if filter_active {
                                let rect = search_resp.rect.expand(1.0);
                                ui.painter().rect_stroke(
                                    rect,
                                    egui::CornerRadius::same(3),
                                    egui::Stroke::new(1.5, accent),
                                    egui::StrokeKind::Outside,
                                );
                            }
                            if search_resp.changed() {
                                // Clear selection when filter changes
                                tab.clear_selection();
                            }
                            if filter_active {
                                if ui.small_button("✕").on_hover_text("Clear filter").clicked() {
                                    tab.filter.clear();
                                }
                            }
                        });

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

                            ui.separator();
                            let current_mode = pane.active_tab().view_mode;
                            for (label, vm) in [
                                ("Details", ViewMode::Details),
                                ("List", ViewMode::List),
                                ("Icons", ViewMode::Icons),
                            ] {
                                if ui.selectable_label(current_mode == vm, label).clicked()
                                    && current_mode != vm
                                {
                                    pane.active_tab_mut().view_mode = vm;
                                    self.dirty = true;
                                }
                            }
                        });

                        match crate::fs_entry::list_dir(&current_path) {
                            Ok(mut entries) => {
                                // Apply this tab's search filter
                                let query = pane.active_tab().filter.clone();
                                search::filter_entries(&mut entries, &query);
                                let (sort_col, sort_asc) = {
                                    let tab = pane.active_tab();
                                    (tab.sort_col.clone(), tab.sort_asc)
                                };
                                crate::fs_entry::sort_entries(&mut entries, &sort_col, sort_asc);
                                let ctrl = ui.input(|i| i.modifiers.ctrl);
                                let shift = ui.input(|i| i.modifiers.shift);
                                let mode = pane.active_tab().view_mode;

                                let mut select_name: Option<String> = None;
                                let mut select_index: Option<usize> = None;
                                let mut nav_target: Option<PathBuf> = None;
                                let mut open_target: Option<PathBuf> = None;
                                let mut row_action: Option<RowAction> = None;

                                match mode {
                                    ViewMode::Details => {
                                        let col_w = pane.active_tab().col_widths;
                                        // Row/header height scale with the font-size setting
                                        // so larger text never clips inside a fixed row.
                                        let row_height = (self.font_size + 10.0).max(20.0);
                                        let header_height = row_height + 2.0;
                                        let mut sort_clicked: Option<String> = None;
                                        let mut live_widths: Option<Vec<f32>> = None;

                                        egui::ScrollArea::horizontal()
                                            .id_salt(format!("file_scroll_pane_{pane_idx}"))
                                            .show(ui, |ui| {
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
                                            .header(header_height, |mut header| {
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
                                                body.rows(row_height, entries.len(), |mut row| {
                                                    let entry = &entries[row.index()];
                                                    let row_idx = row.index();
                                                    let is_selected = pane
                                                        .active_tab()
                                                        .selected
                                                        .contains(&entry.name);

                                                    row.set_selected(is_selected);

                                                    row.col(|ui| {
                                                        let label = if entry.is_dir {
                                                            format!("\u{1F4C1} {}", entry.name)
                                                        } else {
                                                            entry.name.clone()
                                                        };
                                                        let name_resp = ui.selectable_label(is_selected, &label);
                                                        if name_resp.clicked() {
                                                            select_name = Some(entry.name.clone());
                                                            select_index = Some(row_idx);
                                                        }
                                                        if name_resp.double_clicked() {
                                                            if entry.is_dir {
                                                                nav_target = Some(entry.path.clone());
                                                            } else {
                                                                open_target = Some(entry.path.clone());
                                                            }
                                                        }
                                                        if name_resp.secondary_clicked() && !is_selected {
                                                            select_name = Some(entry.name.clone());
                                                            select_index = Some(row_idx);
                                                        }
                                                        name_resp.context_menu(|ui| {
                                                            show_entry_context_menu(ui, &mut row_action, &entry.path, entry.is_dir);
                                                        });
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
                                            }); // end ScrollArea::horizontal

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
                                    }
                                    ViewMode::List => {
                                        egui::ScrollArea::vertical()
                                            .id_salt(format!("file_list_pane_{pane_idx}"))
                                            .show(ui, |ui| {
                                                for (idx, entry) in entries.iter().enumerate() {
                                                    let is_selected = pane
                                                        .active_tab()
                                                        .selected
                                                        .contains(&entry.name);
                                                    let label = if entry.is_dir {
                                                        format!("\u{1F4C1} {}", entry.name)
                                                    } else {
                                                        entry.name.clone()
                                                    };
                                                    let resp = ui.selectable_label(is_selected, &label);
                                                    handle_entry_response(
                                                        &resp, entry, is_selected,
                                                        &mut select_name,
                                                        &mut select_index,
                                                        &mut nav_target,
                                                        &mut open_target,
                                                        idx,
                                                    );
                                                    resp.context_menu(|ui| {
                                                        show_entry_context_menu(ui, &mut row_action, &entry.path, entry.is_dir);
                                                    });
                                                }
                                            });
                                    }
                                    ViewMode::Icons => {
                                        egui::ScrollArea::vertical()
                                            .id_salt(format!("file_icons_pane_{pane_idx}"))
                                            .show(ui, |ui| {
                                                ui.horizontal_wrapped(|ui| {
                                                    for (idx, entry) in entries.iter().enumerate() {
                                                        let is_selected = pane
                                                            .active_tab()
                                                            .selected
                                                            .contains(&entry.name);
                                                        ui.allocate_ui(egui::vec2(76.0, 56.0), |ui| {
                                                            let icon = if entry.is_dir { "🗀" } else { "🗋" };
                                                            let resp = ui.selectable_label(
                                                                is_selected,
                                                                format!("{icon}\n{}", entry.name),
                                                            );
                                                            handle_entry_response(
                                                                &resp, entry, is_selected,
                                                                &mut select_name,
                                                                &mut select_index,
                                                                &mut nav_target,
                                                                &mut open_target,
                                                                idx,
                                                            );
                                                            resp.context_menu(|ui| {
                                                                show_entry_context_menu(ui, &mut row_action, &entry.path, entry.is_dir);
                                                            });
                                                        });
                                                    }
                                                });
                                            });
                                    }
                                }

                                if let Some(name) = select_name {
                                    if shift {
                                        // Range selection: select all entries between last selected and current
                                        if let Some(idx) = select_index {
                                            let anchor = self.last_selected_index.unwrap_or(idx);
                                            let start = anchor.min(idx);
                                            let end = anchor.max(idx);
                                            let range_names: Vec<String> = entries[start..=end]
                                                .iter()
                                                .map(|e| e.name.clone())
                                                .collect();
                                            pane.active_tab_mut().clear_selection();
                                            pane.active_tab_mut().select_range(&range_names);
                                        }
                                    } else if ctrl {
                                        pane.active_tab_mut().toggle_select(&name);
                                    } else {
                                        pane.active_tab_mut().select_only(&name);
                                    }
                                    // Update last selected index for Shift+click anchor
                                    if let Some(idx) = select_index {
                                        self.last_selected_index = Some(idx);
                                    }
                                    self.active_pane = pane_idx;
                                }
                                if let Some(target) = nav_target {
                                    pane.active_tab_mut().navigate_to(target);
                                    self.active_pane = pane_idx;
                                    self.dirty = true;
                                }
                                if let Some(target) = open_target {
                                    let _ = std::process::Command::new("cmd")
                                        .args(["/C", "start", "", &target.to_string_lossy()])
                                        .spawn();
                                }
                                if let Some(action) = row_action {
                                    self.active_pane = pane_idx;
                                    match action {
                                        RowAction::Copy => self.copy_selection(),
                                        RowAction::Cut => self.cut_selection(),
                                        RowAction::Paste => self.paste_clipboard(),
                                        RowAction::Rename => self.begin_rename(),
                                        RowAction::Delete => self.delete_selection(),
                                        RowAction::NewFolder => {
                                            self.dialog = Some(Dialog::NewFolder {
                                                name: String::new(),
                                            });
                                        }
                                        RowAction::NewFile => {
                                            self.dialog = Some(Dialog::NewFile {
                                                name: String::new(),
                                            });
                                        }
                                        RowAction::CopyName => self.copy_filename(&ctx),
                                        RowAction::CopyFolderPath => self.copy_folder_path(&ctx),
                                        RowAction::ExtractHere => self.extract_here(),
                                        RowAction::ExtractTo => self.extract_to(),
                                        RowAction::FavouriteFolder(path) => {
                                            let path_str = path.display().to_string();
                                            if crate::db::is_favourite(&self.conn, &path_str) {
                                                self.remove_favourite(&path_str);
                                            } else {
                                                if crate::db::add_favourite(&self.conn, &path_str).is_ok() {
                                                    self.favourites = crate::db::get_favourites(&self.conn);
                                                    self.status = format!("Added to favourites: {}", path.display());
                                                }
                                            }
                                        }
                                        RowAction::OpenWith(path) => {
                                            let _ = std::process::Command::new("cmd")
                                                .args(["/C", "rundll32.exe", "shell32.dll,OpenAs_RunDLL", &path.to_string_lossy()])
                                                .spawn();
                                        }
                                    }
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
    select_index: &mut Option<usize>,
    nav_target: &mut Option<PathBuf>,
    open_target: &mut Option<PathBuf>,
    index: usize,
) {
    if resp.double_clicked() {
        if entry.is_dir {
            *nav_target = Some(entry.path.clone());
        } else {
            *open_target = Some(entry.path.clone());
        }
    } else if resp.clicked() {
        *select_name = Some(entry.name.clone());
        *select_index = Some(index);
    }
}

/// Deferred action requested from a file entry's right-click context menu.
/// Applied after the pane's mutable borrow ends, same pattern as the other
/// deferred flags (`select_name`, `nav_target`) in the file listing.
enum RowAction {
    Copy,
    Cut,
    Paste,
    Rename,
    Delete,
    NewFolder,
    NewFile,
    CopyName,
    CopyFolderPath,
    ExtractHere,
    ExtractTo,
    FavouriteFolder(PathBuf),
    OpenWith(PathBuf),
}

/// Records a click on a file-entry widget (selection / navigate-into-folder)
/// and, on a right-click of a not-yet-selected entry, selects it first — so
/// the context menu that follows acts on the entry the user actually
/// right-clicked, not on whatever was selected before.
fn handle_entry_response(
    resp: &egui::Response,
    entry: &crate::fs_entry::FsEntry,
    is_selected: bool,
    select_name: &mut Option<String>,
    select_index: &mut Option<usize>,
    nav_target: &mut Option<PathBuf>,
    open_target: &mut Option<PathBuf>,
    index: usize,
) {
    register_entry_click(resp, entry, select_name, select_index, nav_target, open_target, index);
    if resp.secondary_clicked() && !is_selected {
        *select_name = Some(entry.name.clone());
        *select_index = Some(index);
    }
}

/// Shared right-click context menu content for a file entry, used by every
/// view mode. Uses egui's built-in `Response::context_menu`, which persists
/// its own open/close state across frames (unlike a hand-rolled `Area`).
fn show_entry_context_menu(
    ui: &mut egui::Ui,
    row_action: &mut Option<RowAction>,
    entry_path: &std::path::Path,
    is_dir: bool,
) {
    ui.set_min_width(140.0);
    if ui.button("Copy").clicked() {
        *row_action = Some(RowAction::Copy);
        ui.close();
    }
    if ui.button("Cut").clicked() {
        *row_action = Some(RowAction::Cut);
        ui.close();
    }
    if ui.button("Paste").clicked() {
        *row_action = Some(RowAction::Paste);
        ui.close();
    }
    ui.separator();
    if ui.button("Rename").clicked() {
        *row_action = Some(RowAction::Rename);
        ui.close();
    }
    if ui.button("Delete").clicked() {
        *row_action = Some(RowAction::Delete);
        ui.close();
    }
    ui.separator();
    if ui.button("New Folder").clicked() {
        *row_action = Some(RowAction::NewFolder);
        ui.close();
    }
    if ui.button("New File").clicked() {
        *row_action = Some(RowAction::NewFile);
        ui.close();
    }
    ui.separator();
    if ui.button("Extract Here").clicked() {
        *row_action = Some(RowAction::ExtractHere);
        ui.close();
    }
    if ui.button("Extract to...").clicked() {
        *row_action = Some(RowAction::ExtractTo);
        ui.close();
    }
    ui.separator();
    if ui.button("Copy Filename").clicked() {
        *row_action = Some(RowAction::CopyName);
        ui.close();
    }
    if ui.button("Copy Folder Path").clicked() {
        *row_action = Some(RowAction::CopyFolderPath);
        ui.close();
    }
    ui.separator();
    if ui.button("Open With...").clicked() {
        *row_action = Some(RowAction::OpenWith(entry_path.to_path_buf()));
        ui.close();
    }
    if is_dir {
        ui.separator();
        if ui.button("★ Add to Favourites").clicked() {
            *row_action = Some(RowAction::FavouriteFolder(entry_path.to_path_buf()));
            ui.close();
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
