use super::*;

/// Background fill for right-click context menus, deliberately distinct from
/// the page background (`panel_fill`, which egui otherwise matches exactly)
/// so the floating menu reads as its own surface in both themes.
pub(super) fn context_menu_fill(theme: egui::Theme) -> egui::Color32 {
    match theme {
        // Page: gray(27) — a blue-tinted dark slate stands out clearly.
        egui::Theme::Dark => egui::Color32::from_rgb(34, 43, 60),
        // Page: gray(248) — a blue-tinted light gray stands out clearly.
        egui::Theme::Light => egui::Color32::from_rgb(226, 235, 250),
    }
}

/// A toolbar command button with an accent-blue "ribbon" treatment so the
/// Color variant for toolbar buttons, so different action categories are
/// visually distinct at a glance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ButtonStyle {
    /// Default blue ribbon — builtin toolbar actions and launcher apps.
    Blue,
    /// Green/teal tint — custom "open with" actions.
    Green,
}

/// Styled toolbar command button with a ribbon-like 3D bevel effect so
/// action rows read as commands rather than page content. Keeps the 3D
/// bevel/hover-lift/press-in states via a scoped widget-style override.
pub(super) fn toolbar_button(
    ui: &mut egui::Ui,
    label: String,
    icon: Option<&egui::TextureHandle>,
    style: ButtonStyle,
) -> egui::Response {
    let dark = ui.visuals().dark_mode;
    let (face, hover_face, active_face, border, hover_border) = if dark {
        match style {
            ButtonStyle::Blue => (
                egui::Color32::from_rgb(45, 64, 84),
                egui::Color32::from_rgb(58, 82, 106),
                egui::Color32::from_rgb(30, 44, 58),
                egui::Color32::from_rgb(17, 25, 34),
                egui::Color32::from_rgb(104, 148, 190),
            ),
            ButtonStyle::Green => (
                egui::Color32::from_rgb(30, 72, 60),
                egui::Color32::from_rgb(42, 96, 80),
                egui::Color32::from_rgb(20, 52, 42),
                egui::Color32::from_rgb(14, 38, 30),
                egui::Color32::from_rgb(80, 180, 148),
            ),
        }
    } else {
        match style {
            ButtonStyle::Blue => (
                egui::Color32::from_rgb(232, 241, 250),
                egui::Color32::WHITE,
                egui::Color32::from_rgb(184, 212, 240),
                egui::Color32::from_rgb(163, 197, 229),
                egui::Color32::from_rgb(110, 165, 220),
            ),
            ButtonStyle::Green => (
                egui::Color32::from_rgb(220, 243, 235),
                egui::Color32::WHITE,
                egui::Color32::from_rgb(170, 220, 200),
                egui::Color32::from_rgb(140, 195, 175),
                egui::Color32::from_rgb(80, 160, 130),
            ),
        }
    };
    ui.scope(|ui| {
        let v = &mut ui.style_mut().visuals;
        v.widgets.inactive.bg_fill = face;
        v.widgets.inactive.weak_bg_fill = face;
        v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, border);
        v.widgets.hovered.bg_fill = hover_face;
        v.widgets.hovered.weak_bg_fill = hover_face;
        v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, hover_border);
        v.widgets.hovered.expansion = 1.0;
        v.widgets.active.bg_fill = active_face;
        v.widgets.active.weak_bg_fill = active_face;
        v.widgets.active.bg_stroke =
            egui::Stroke::new(1.0, if dark { border } else { hover_border });
        let btn = match icon {
            Some(tex) => egui::Button::image_and_text(
                egui::Image::new(egui::load::SizedTexture::new(
                    tex.id(),
                    egui::vec2(16.0, 16.0),
                )),
                label,
            ),
            None => egui::Button::new(label),
        };
        ui.add(btn)
    })
    .inner
}

/// Border stroke for right-click context menus, slightly stronger than the
/// default window stroke so the tinted surface keeps a crisp edge.
pub(super) fn context_menu_stroke(theme: egui::Theme) -> egui::Stroke {
    match theme {
        egui::Theme::Dark => egui::Stroke::new(1.0, egui::Color32::from_rgb(72, 88, 115)),
        egui::Theme::Light => egui::Stroke::new(1.0, egui::Color32::from_rgb(150, 170, 205)),
    }
}

/// Paths a right-click on `entry` should act on: the full multi-selection
/// when `entry` is already selected and more than one item is selected
/// (Explorer's own behavior), otherwise just `entry` itself.
pub(super) fn context_menu_paths(
    tab: &crate::tab::Tab,
    entry: &crate::fs_entry::FsEntry,
    is_selected: bool,
) -> Vec<PathBuf> {
    if is_selected && tab.selected.len() > 1 {
        tab.selected.iter().map(|n| tab.path.join(n)).collect()
    } else {
        vec![entry.path.clone()]
    }
}

/// Same as `Response::context_menu`, but paints the popup with the distinct
/// per-theme fill from `context_menu_fill` (egui's popup frame reads
/// `visuals.window_fill`, which defaults to the page background color).
pub(super) fn styled_context_menu<R>(
    resp: &egui::Response,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> Option<egui::InnerResponse<R>> {
    let theme = resp.ctx.theme();
    let fill = context_menu_fill(theme);
    let stroke = context_menu_stroke(theme);
    egui::Popup::context_menu(resp)
        .style(move |style: &mut egui::Style| {
            egui::containers::menu::menu_style(style);
            style.visuals.window_fill = fill;
            style.visuals.window_stroke = stroke;
        })
        .show(add_contents)
}

/// Shared right-click context menu content for a file entry, used by every
/// view mode. Uses egui's built-in `Response::context_menu`, which persists
/// its own open/close state across frames (unlike a hand-rolled `Area`).
pub(super) fn show_entry_context_menu(
    ui: &mut egui::Ui,
    row_action: &mut Option<RowAction>,
    entry_path: &std::path::Path,
    is_dir: bool,
    // Every path the "Windows Explorer" shell submenu should act on: just
    // `entry_path` for a right-click on an unselected/lone item, or the
    // full multi-selection when the click landed on a selected row —
    // otherwise a shell command like "Combine files in Foxit PDF" only
    // ever sees the single row that was clicked.
    selection_paths: &[std::path::PathBuf],
    shell_menu_hidden: &std::collections::HashSet<String>,
    shell_menu_cache: &mut Option<(Vec<std::path::PathBuf>, Vec<crate::shell_menu::ShellMenuItem>)>,
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
    if selection_paths.len() > 1 {
        if ui.button("Bulk Rename...").clicked() {
            *row_action = Some(RowAction::BulkRename);
            ui.close();
        }
    }
    if ui.button("Delete").clicked() {
        *row_action = Some(RowAction::Delete);
        ui.close();
    }
    if !is_dir {
        if ui.button("\u{1F4CC} Pin as Tab").clicked() {
            *row_action = Some(RowAction::PinAsTab);
            ui.close();
        }
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
    if archive::is_archive(entry_path) {
        ui.separator();
        if ui.button("Extract Here").clicked() {
            *row_action = Some(RowAction::ExtractHere);
            ui.close();
        }
        if ui.button("Extract to...").clicked() {
            *row_action = Some(RowAction::ExtractTo);
            ui.close();
        }
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
    ui.separator();
    if ui.button("Properties").clicked() {
        *row_action = Some(RowAction::Properties(entry_path.to_path_buf()));
        ui.close();
    }
    if is_dir {
        if ui.button("★ Add to Favourites").clicked() {
            *row_action = Some(RowAction::FavouriteFolder(entry_path.to_path_buf()));
            ui.close();
        }
    }

    // Shell handlers are loaded only when this submenu is actually opened.
    ui.separator();
    ui.menu_button("Windows Explorer", |ui| {
        if shell_menu_cache.as_ref().is_none_or(|(paths, _)| paths.as_slice() != selection_paths) {
            *shell_menu_cache = Some((selection_paths.to_vec(), crate::shell_menu::query_items(selection_paths)));
        }
        ui.set_min_width(180.0);
        render_shell_items(ui, row_action, selection_paths, &shell_menu_cache.as_ref().unwrap().1, shell_menu_hidden);
    });
}

/// Recursively render shell menu items into an egui sub-menu, skipping any
/// item whose label is in `shell_menu_hidden` (configured under Settings).
pub(super) fn render_shell_items(
    ui: &mut egui::Ui,
    row_action: &mut Option<RowAction>,
    selection_paths: &[std::path::PathBuf],
    items: &[crate::shell_menu::ShellMenuItem],
    shell_menu_hidden: &std::collections::HashSet<String>,
) {
    for item in items {
        if item.separator {
            ui.separator();
            continue;
        }
        if shell_menu_hidden.contains(&item.label) {
            continue;
        }
        if item.disabled {
            ui.add_enabled(false, egui::Button::new(&item.label));
            continue;
        }
        if !item.sub_items.is_empty() {
            let label = item.label.clone();
            let sub_items = item.sub_items.clone();
            let paths = selection_paths.to_vec();
            let ra = std::rc::Rc::new(std::cell::RefCell::new(None::<RowAction>));
            let ra_clone = ra.clone();
            ui.menu_button(&label, |ui| {
                render_shell_items(ui, &mut *ra_clone.borrow_mut(), &paths, &sub_items, shell_menu_hidden);
            });
            if let Some(action) = ra.borrow_mut().take() {
                *row_action = Some(action);
                ui.close();
            }
        } else if ui.button(&item.label).clicked() {
            *row_action = Some(RowAction::ShellCommand {
                id: item.id,
                paths: selection_paths.to_vec(),
            });
            ui.close();
        }
    }
}

/// Parses the newline-separated `shell_menu_hidden` setting into a lookup
/// set, trimming blank lines.
pub(super) fn parse_shell_menu_hidden(raw: &str) -> std::collections::HashSet<String> {
    raw.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

