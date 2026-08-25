use crate::actions::{Action, ActionRef};
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
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

/// Formats a file size in bytes into a human-readable string (KB, MB, GB, etc.).
fn format_file_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

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
    /// Renaming a tab's display label (independent of its folder).
    RenameTab { pane_idx: usize, tab_idx: usize, name: String },
    /// Find dialog for searching files. Results keep their full metadata so
    /// the results table can display and sort by name/folder/date/size. The
    /// name/folder filters narrow the displayed list without discarding hits;
    /// `include_folders` toggles whether directories appear at all.
    Find {
        query: String,
        results: Vec<crate::fs_entry::FsEntry>,
        search_path: PathBuf,
        sort_col: String,
        sort_asc: bool,
        name_filter: String,
        folder_filter: String,
        include_folders: bool,
    },
    /// Create a new user profile.
    NewUser { name: String },
    /// Help / user manual.
    Help,
    /// Confirm delete: paths ready to be deleted, waiting for user confirmation.
    ConfirmDelete { paths: Vec<PathBuf> },
}

/// Payload carried while file entries are dragged between panes/tabs: the
/// selected full paths, the folder they came from (so same-folder drops are
/// ignored), and the pane that started the drag (so its selection can be
/// cleared once the transfer is queued).
#[derive(Clone)]
struct DragFiles {
    paths: Vec<PathBuf>,
    from_dir: PathBuf,
    from_pane: usize,
}

pub struct FileManApp {
    conn: Connection,
    current_user_id: i64,
    /// Cached list of user profiles, refreshed on switch/create.
    users: Vec<crate::user::User>,
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
    font_size: f32,
    /// Index of the last selected entry (anchor for Shift+click range selection).
    last_selected_index: Option<usize>,
    /// Previous active path for detecting navigation changes (tree auto-expand).
    prev_active_path: Option<PathBuf>,
    font_family: String,
    /// The font_family that was last handed to `ctx.set_fonts`, so we only
    /// rebuild the font atlas when the setting actually changes.
    fonts_applied_family: Option<String>,
    /// True for the single frame after `set_fonts` — egui swaps in the new
    /// definitions at the start of the following pass, so custom families
    /// must not be referenced until this clears.
    fonts_pending_apply: bool,
    /// Frames remaining to keep centering the folder tree on the active
    /// folder after a navigation. The tree's expanded layout settles over a
    /// couple of passes, so one `scroll_to_me` call isn't enough.
    tree_scroll_frames: u32,
    /// Frames remaining to keep collapsing branches that are off the active
    /// path after a navigation (Explorer-style: only the current branch
    /// stays open).
    tree_collapse_frames: u32,
    /// Last value of `status` seen, so a change can trigger a toast.
    last_status: String,
    /// Active toast message and when it appeared (auto-hides after ~3s).
    toast: Option<(String, std::time::Instant)>,
    /// Where to anchor the tab context menu (the right-click point).
    tab_menu_pos: Option<egui::Pos2>,
    /// Currently selected page in the settings dialog.
    settings_page: SettingsPage,
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
    /// In-flight background directory listing per pane (only the active
    /// tab of each pane needs a live listing job at a time).
    listing_jobs: [Option<ListingJob>; 2],
    /// Directories a just-started background copy/move/delete affects, so
    /// their tabs can be marked stale once the operation completes.
    background_op_dirs: Vec<PathBuf>,
    /// Fraction of the central panel's width given to the left pane.
    split_ratio: f32,
    /// Width in points of the sidebar folder-tree panel. Driven manually
    /// (like `split_ratio`) rather than via `egui::Panel`'s own resize
    /// handling, which was found not to persist the released width.
    tree_width: f32,
    /// Device name (e.g. `\\.\DISPLAY1`) of the monitor the window was last
    /// observed on, refreshed whenever the window's position changes.
    last_monitor_name: Option<String>,
    /// Effective shortcut map (default < global < per-user), reloaded on
    /// user switch or after a rebind.
    shortcut_map: HashMap<crate::actions::KeyCombo, ActionRef>,
    /// Effective toolbar layout (per-user, falling back to global).
    toolbar_actions: Vec<ActionRef>,
    /// This user's "open with `<exe>`" custom actions.
    custom_actions: Vec<crate::actions::CustomAction>,
    /// Loaded exe icons for custom actions, keyed by exe path. `None` means
    /// extraction failed (no icon) and should not be retried every frame.
    custom_icons: HashMap<String, Option<egui::TextureHandle>>,
    /// Shell-associated icon textures for files in the listings, keyed by
    /// `icon_cache::file_icon_cache_key` (extension, or full path for
    /// exe-like types). `None` values are failed lookups, cached so they
    /// aren't retried every frame.
    file_icons: HashMap<String, Option<egui::TextureHandle>>,
    /// Set while the Settings "Shortcuts" tab is waiting for the next key
    /// event to bind to this action.
    capturing_shortcut_for: Option<Action>,
    /// Draft label for the "Custom Actions" add-new-action form.
    new_custom_action_label: String,
    /// Draft executable path for the "Custom Actions" add-new-action form.
    new_custom_action_exe: Option<PathBuf>,
    /// Background recursive-search job for the Find dialog. Streams matching
    /// entries one by one; a `Disconnected` receive means the walk finished.
    find_job: Option<mpsc::Receiver<crate::fs_entry::FsEntry>>,
    /// Whether each pane's tab strip runs horizontally or is stacked vertically.
    tab_orientation: TabOrientation,
    /// Width of the vertical tab sidebar, user-adjustable via its drag handle.
    tab_strip_width: f32,
    /// Whether this window's taskbar overlay badge (SPEC §11) has been applied yet.
    taskbar_badge_applied: bool,
    /// This process's open-order slot (SPEC §11), assigned in `main` before
    /// the window was created; used to color this window's taskbar icon.
    instance_slot: usize,
    /// Whether the rotating bottom-left tips card is shown (persisted per
    /// user as `tips_enabled`).
    tips_enabled: bool,
    /// State for the tips card: current tip, rotation timing and session
    /// visibility.
    tips: crate::tips::TipsCard,
    /// Pane body rects captured during rendering, for drag & drop
    /// hit-testing. Refreshed every frame.
    dnd_pane_rects: [Option<egui::Rect>; 2],
    /// Tab-header rects captured during rendering — `((pane, tab), rect,
    /// is_active)` — so a dragged item hovering an inactive tab can open it.
    dnd_tab_rects: Vec<((usize, usize), egui::Rect, bool)>,
}

/// Resolves the device name of the monitor the window currently sits on, via
/// the raw HWND `eframe::Frame` exposes on Windows. `None` on any other
/// platform, or if the handle/monitor lookup fails.
fn current_monitor_name(frame: &eframe::Frame) -> Option<String> {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let handle = frame.window_handle().ok()?;
        if let RawWindowHandle::Win32(h) = handle.as_raw() {
            let hwnd = windows::Win32::Foundation::HWND(h.hwnd.get() as *mut _);
            return crate::monitor::monitor_name_for_hwnd(hwnd);
        }
        None
    }
    #[cfg(not(windows))]
    {
        let _ = frame;
        None
    }
}

/// The mouse cursor's position in window-client egui points, read straight
/// from the OS rather than from egui's own pointer tracking.
///
/// A native OS file drag (dropped from Explorer/WinRAR) never generates the
/// mouse-move messages winit turns into egui pointer events — Windows' OLE
/// drag loop owns the cursor for the whole drag, so `dropped_files` always
/// arrives with `i.pointer.hover_pos()` stuck at wherever the mouse was
/// before the external drag started. `GetCursorPos` sidesteps that by asking
/// Windows for the cursor's current screen position directly.
fn native_cursor_pos(frame: &eframe::Frame, ctx: &egui::Context) -> Option<egui::Pos2> {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        use windows::Win32::Foundation::POINT;
        use windows::Win32::Graphics::Gdi::ScreenToClient;
        use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
        let handle = frame.window_handle().ok()?;
        let RawWindowHandle::Win32(h) = handle.as_raw() else {
            return None;
        };
        let hwnd = windows::Win32::Foundation::HWND(h.hwnd.get() as *mut _);
        let mut point = POINT::default();
        unsafe {
            if GetCursorPos(&mut point).is_err() {
                return None;
            }
            let _ = ScreenToClient(hwnd, &mut point);
        }
        let ppp = ctx.pixels_per_point();
        Some(egui::pos2(point.x as f32 / ppp, point.y as f32 / ppp))
    }
    #[cfg(not(windows))]
    {
        let _ = (frame, ctx);
        None
    }
}

/// A directory listing running on a background thread, polled each frame.
struct ListingJob {
    dir: PathBuf,
    rx: mpsc::Receiver<std::io::Result<Vec<crate::fs_entry::FsEntry>>>,
}

fn spawn_listing_job(dir: PathBuf) -> ListingJob {
    let (tx, rx) = mpsc::channel();
    let job_dir = dir.clone();
    thread::spawn(move || {
        let _ = tx.send(crate::fs_entry::list_dir(&job_dir));
    });
    ListingJob { dir, rx }
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
    // Dedicated bold family so widgets can ask for a genuinely heavier
    // weight — `RichText::strong()` only changes color. Falls back to the
    // regular weight when no system bold file exists.
    let bold_source = match family {
        "Segoe UI" => Some("segoeuib.ttf"),
        "Arial" => Some("arialbd.ttf"),
        "Times New Roman" => Some("timesbd.ttf"),
        "Courier New" => Some("courbd.ttf"),
        _ => None,
    }
    .and_then(|name| {
        let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".to_string());
        std::fs::read(PathBuf::from(windir).join("Fonts").join(name)).ok()
    });
    let bold_font = match bold_source {
        Some(bytes) => {
            fonts.font_data.insert(
                "app_bold_face".to_owned(),
                std::sync::Arc::new(egui::FontData::from_owned(bytes)),
            );
            "app_bold_face".to_owned()
        }
        None => active.clone(),
    };
    for family_key in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .get_mut(&family_key)
            .unwrap()
            .insert(0, active.clone());
    }
    fonts.families.insert(
        egui::FontFamily::Name(APP_BOLD_FAMILY.into()),
        vec![bold_font],
    );
    ctx.set_fonts(fonts);
}

/// Font family (registered in `apply_fonts`) carrying the bundled Segoe UI
/// Bold, for widgets that need a genuinely heavier weight.
const APP_BOLD_FAMILY: &str = "app_bold";

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

/// How a pane's tab strip is laid out: tabs side by side in a row, or
/// stacked one per line. Persisted per user as `tab_orientation`.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum TabOrientation {
    Horizontal,
    #[default]
    Vertical,
}

impl TabOrientation {
    fn parse(raw: &str) -> Self {
        match raw {
            "vertical" => Self::Vertical,
            _ => Self::Horizontal,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Horizontal => "horizontal",
            Self::Vertical => "vertical",
        }
    }
}

/// Which page of the Office-style settings dialog is showing.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum SettingsPage {
    #[default]
    Appearance,
    Shortcuts,
    Toolbar,
    CustomActions,
    ViewMode,
    Advanced,
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
    pub fn new(
        conn: Connection,
        current_user_id: i64,
        loaded: Option<session::LoadedSession>,
        instance_slot: usize,
        startup_dir: Option<PathBuf>,
    ) -> Self {
        let (panes, active_pane) = match loaded {
            Some(s) if !s.panes.is_empty() => ensure_two_panes(s.panes, s.active_pane),
            _ => ensure_two_panes(Vec::new(), 0),
        };
        let theme_pref = crate::config::get(&conn, current_user_id, "theme")
            .map(|raw| parse_theme_pref(&raw))
            .unwrap_or_default();
        let font_size = crate::config::get(&conn, current_user_id, "font_size")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(14.0);
        let font_family = crate::config::get(&conn, current_user_id, "font_family")
            .unwrap_or_else(|| "Segoe UI".to_string());
        let tab_orientation = crate::config::get(&conn, current_user_id, "tab_orientation")
            .map(|raw| TabOrientation::parse(&raw))
            .unwrap_or_default();
        let tab_strip_width = crate::config::get(&conn, current_user_id, "tab_strip_width")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(150.0);
        let tips_enabled = crate::config::get(&conn, current_user_id, crate::tips::KEY_TIPS_ENABLED)
            .map(|raw| raw != "false")
            .unwrap_or(true);
        let favourites = crate::db::get_favourites(&conn, current_user_id);
        let split_ratio = crate::db::get_split_ratio(&conn, current_user_id).unwrap_or(0.5);
        let tree_width = crate::db::get_tree_width(&conn, current_user_id).unwrap_or(200.0);
        let users = crate::user::list_users(&conn);
        let _ = crate::actions::init_tables(&conn);
        let shortcut_map = crate::actions::load_shortcut_map(&conn, current_user_id);
        let toolbar_actions = crate::actions::load_toolbar(&conn, current_user_id);
        let custom_actions = crate::actions::list_custom_actions(&conn, current_user_id);
        let mut panes = panes;
        if let Some(dir) = startup_dir {
            // Launched as the default folder explorer with a clicked folder.
            let first = &mut panes[0].tabs[0];
            first.path = dir;
            first.listing_dirty = true;
        }
        FileManApp {
            conn,
            current_user_id,
            users,
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
            font_size,
            last_selected_index: None,
            prev_active_path: None,
            font_family,
            fonts_applied_family: None,
            fonts_pending_apply: false,
            tree_scroll_frames: 0,
            tree_collapse_frames: 0,
            last_status: String::new(),
            toast: None,
            tab_menu_pos: None,
            settings_page: SettingsPage::default(),
            last_pos: None,
            background_op: None,
            focused_address_pane: None,
            network_servers: tree::list_network_servers(),
            favourites,
            listing_jobs: [None, None],
            background_op_dirs: Vec::new(),
            split_ratio,
            tree_width,
            last_monitor_name: None,
            shortcut_map,
            toolbar_actions,
            custom_actions,
            custom_icons: HashMap::new(),
            file_icons: HashMap::new(),
            capturing_shortcut_for: None,
            new_custom_action_label: String::new(),
            new_custom_action_exe: None,
            find_job: None,
            tab_orientation,
            tab_strip_width,
            taskbar_badge_applied: false,
            instance_slot,
            tips_enabled,
            tips: crate::tips::TipsCard::new(),
            dnd_pane_rects: [None, None],
            dnd_tab_rects: Vec::new(),
        }
    }

    /// Polls/spawns the background listing job for `pane_idx`'s active tab:
    /// picks up a finished job's result (discarding it if the tab navigated
    /// away mid-flight), then starts a new job if the tab is dirty and no
    /// job is already in flight. Requests a repaint while a job is pending
    /// so results land without waiting for user input.
    fn poll_listing(&mut self, pane_idx: usize, ctx: &egui::Context) {
        let active_idx = self.panes[pane_idx].active_tab;
        let dir = self.panes[pane_idx].tabs[active_idx].path.clone();

        if let Some(job) = &self.listing_jobs[pane_idx] {
            if let Ok(result) = job.rx.try_recv() {
                if job.dir == dir {
                    let tab = &mut self.panes[pane_idx].tabs[active_idx];
                    match result {
                        Ok(entries) => {
                            tab.listing = entries;
                            tab.listing_error = None;
                        }
                        Err(e) => tab.listing_error = Some(e.to_string()),
                    }
                }
                self.listing_jobs[pane_idx] = None;
            }
        }

        let tab = &mut self.panes[pane_idx].tabs[active_idx];
        if self.listing_jobs[pane_idx].is_none() && tab.listing_dirty {
            tab.listing_dirty = false;
            self.listing_jobs[pane_idx] = Some(spawn_listing_job(dir));
        }

        if self.listing_jobs[pane_idx].is_some() {
            ctx.request_repaint();
        }
    }

    /// Marks every tab (in both panes) whose path equals `dir` as needing a
    /// fresh listing. Called after any operation that mutates a directory's
    /// contents so the UI picks up the change without a manual refresh.
    fn mark_dir_dirty(&mut self, dir: &Path) {
        for pane in &mut self.panes {
            for tab in &mut pane.tabs {
                if tab.path == dir {
                    tab.listing_dirty = true;
                }
            }
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
            monitor_name: self.last_monitor_name.clone(),
        };
        let _ = session::save_session(&self.conn, self.current_user_id, &window, &self.panes, self.active_pane);
        let _ = crate::db::set_split_ratio(&self.conn, self.current_user_id, self.split_ratio);
        let _ = crate::db::set_tree_width(&self.conn, self.current_user_id, self.tree_width);
    }

    fn active_tab_dir(&self) -> PathBuf {
        self.panes[self.active_pane].active_tab().path.clone()
    }

    /// Adds the current active folder to favourites.
    fn add_favourite(&mut self) {
        let path = self.active_tab_dir();
        let path_str = path.display().to_string();
        if crate::db::add_favourite(&self.conn, self.current_user_id, &path_str).is_ok() {
            self.favourites = crate::db::get_favourites(&self.conn, self.current_user_id);
            self.status = format!("Added to favourites: {}", path.display());
        }
    }

    /// Removes a path from favourites.
    fn remove_favourite(&mut self, path: &str) {
        if crate::db::remove_favourite(&self.conn, self.current_user_id, path).is_ok() {
            self.favourites = crate::db::get_favourites(&self.conn, self.current_user_id);
            self.status = format!("Removed from favourites");
        }
    }

    /// Persists current state, switches to `user_id`'s session (loading its
    /// panes/split-ratio, or a fresh two-pane default if it has none yet),
    /// and reloads every per-user setting (theme, font, favourites).
    fn switch_user(&mut self, user_id: i64) {
        if user_id == self.current_user_id {
            return;
        }
        self.persist();

        self.current_user_id = user_id;
        let loaded = session::load_session(&self.conn, user_id).ok().flatten();
        let (panes, active_pane) = match loaded {
            Some(s) if !s.panes.is_empty() => ensure_two_panes(s.panes, s.active_pane),
            _ => ensure_two_panes(Vec::new(), 0),
        };
        self.panes = panes;
        self.active_pane = active_pane;
        self.split_ratio = crate::db::get_split_ratio(&self.conn, user_id).unwrap_or(0.5);
        self.tree_width = crate::db::get_tree_width(&self.conn, user_id).unwrap_or(200.0);
        self.theme_pref = crate::config::get(&self.conn, user_id, "theme")
            .map(|raw| parse_theme_pref(&raw))
            .unwrap_or_default();
        self.font_size = crate::config::get(&self.conn, user_id, "font_size")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(14.0);
        self.font_family = crate::config::get(&self.conn, user_id, "font_family")
            .unwrap_or_else(|| "Segoe UI".to_string());
        self.tab_orientation = crate::config::get(&self.conn, user_id, "tab_orientation")
            .map(|raw| TabOrientation::parse(&raw))
            .unwrap_or_default();
        self.tab_strip_width = crate::config::get(&self.conn, user_id, "tab_strip_width")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(150.0);
        self.favourites = crate::db::get_favourites(&self.conn, user_id);
        self.shortcut_map = crate::actions::load_shortcut_map(&self.conn, user_id);
        self.toolbar_actions = crate::actions::load_toolbar(&self.conn, user_id);
        self.custom_actions = crate::actions::list_custom_actions(&self.conn, user_id);
        self.listing_jobs = [None, None];
        self.find_job = None;
        self.status = String::new();
    }

    /// The single entry point every shortcut and toolbar button routes
    /// through: runs the built-in action, or launches a custom "open with"
    /// executable against the current selection.
    fn dispatch(&mut self, ctx: &egui::Context, action_ref: ActionRef) {
        match action_ref {
            ActionRef::Builtin(action) => match action {
                Action::Copy => self.copy_selection(ctx),
                Action::Cut => self.cut_selection(ctx),
                Action::Paste => self.paste_clipboard(),
                Action::Delete => self.delete_selection(),
                Action::Rename => self.begin_rename(),
                Action::NewFolder => {
                    self.dialog = Some(Dialog::NewFolder { name: String::new() });
                }
                Action::NewFile => {
                    self.dialog = Some(Dialog::NewFile { name: String::new() });
                }
                Action::CopyFilename => self.copy_filename(ctx),
                Action::CopyFolderPath => self.copy_folder_path(ctx),
                Action::ExtractHere => self.extract_here(),
                Action::ExtractTo => self.extract_to(),
                Action::ToggleFavourite => self.toggle_favourite_current(),
                Action::GoBack => {
                    let pane = &mut self.panes[self.active_pane];
                    if pane.active_tab().locked {
                        self.status = "Tab is pinned — unpin it to navigate".to_string();
                    } else if pane.active_tab_mut().go_back() {
                        self.dirty = true;
                    }
                }
                Action::GoForward => {
                    let pane = &mut self.panes[self.active_pane];
                    if pane.active_tab().locked {
                        self.status = "Tab is pinned — unpin it to navigate".to_string();
                    } else if pane.active_tab_mut().go_forward() {
                        self.dirty = true;
                    }
                }
                Action::GoUp => {
                    let current = self.active_tab_dir();
                    if let Some(parent) = current.parent() {
                        if self.try_navigate_active(self.active_pane, parent.to_path_buf()) {
                            self.dirty = true;
                        }
                    }
                }
                Action::NewTab => {
                    let current = self.active_tab_dir();
                    self.panes[self.active_pane].open_tab(current);
                    self.dirty = true;
                }
                Action::CloseTab => {
                    let pane = &mut self.panes[self.active_pane];
                    let idx = pane.active_tab;
                    pane.close_tab(idx);
                    self.dirty = true;
                }
                Action::Refresh => {
                    self.panes[self.active_pane]
                        .active_tab_mut()
                        .listing_dirty = true;
                }
                Action::Find => {
                    let search_path = self.active_tab_dir();
                    self.dialog = Some(Dialog::Find {
                        query: String::new(),
                        results: Vec::new(),
                        search_path,
                        sort_col: "name".to_string(),
                        sort_asc: true,
                        name_filter: String::new(),
                        folder_filter: String::new(),
                        include_folders: true,
                    });
                }
                Action::ToggleSettings => self.show_settings = !self.show_settings,
            },
            ActionRef::Custom(id) => {
                if let Some(custom) = self.custom_actions.iter().find(|c| c.id == id) {
                    let mut cmd = std::process::Command::new(&custom.exe_path);
                    if let Some(path) = self.selected_paths().into_iter().next() {
                        cmd.arg(path);
                    }
                    let _ = cmd.spawn();
                }
            }
        }
    }

    /// Adds/removes the current active folder from favourites, matching the
    /// star-icon toggle button's behavior.
    fn toggle_favourite_current(&mut self) {
        let path = self.active_tab_dir().display().to_string();
        if crate::db::is_favourite(&self.conn, self.current_user_id, &path) {
            self.remove_favourite(&path);
        } else {
            self.add_favourite();
        }
    }

    /// Spawns a background recursive search under `dir` for `query`. Results
    /// stream in through `find_job` and are polled every frame; the job ends
    /// when the channel disconnects (walk finished).
    fn start_find_search(&mut self, dir: PathBuf, query: String) {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || crate::search::recursive_search(dir, query, tx));
        self.find_job = Some(rx);
    }

    /// Navigates the active tab of `pane_idx`, refusing (with a status-bar
    /// hint) when that tab is pinned.
    fn try_navigate_active(&mut self, pane_idx: usize, path: PathBuf) -> bool {
        const PINNED: &str = "Tab is pinned — unpin it to navigate";
        if self.panes[pane_idx].active_tab().locked {
            self.status = PINNED.to_string();
            return false;
        }
        self.panes[pane_idx].active_tab_mut().try_navigate(path);
        true
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
        // Windows paths are case-insensitive, but a typed address-bar path or
        // an old session save may not match `list_drives()`'s uppercase
        // drive letters byte-for-byte — compare lowercased to avoid silently
        // breaking ancestor matches (and thus force-expand) on case alone.
        let dir_l = PathBuf::from(dir.to_string_lossy().to_lowercase());
        let active_l = PathBuf::from(active_path.to_string_lossy().to_lowercase());
        let is_active = dir_l == active_l;
        let is_ancestor = active_l.starts_with(&dir_l);
        // The active folder gets a genuinely bold weight (bundled Segoe UI
        // Bold — `RichText::strong()` only changes color, which read faint),
        // an explicit high-contrast text color, and an accent chip behind it.
        let header_text = if is_active {
            let mut text = egui::RichText::new(label)
                .color(ui.visuals().strong_text_color())
                .background_color(if ui.visuals().dark_mode {
                    egui::Color32::from_rgb(9, 74, 140)
                } else {
                    egui::Color32::from_rgb(206, 231, 255)
                });
            if !self.fonts_pending_apply {
                // Safe to reference the bold family once its binding has been
                // through a full pass (see `fonts_pending_apply`).
                text = text.font(egui::FontId::new(
                    self.font_size,
                    egui::FontFamily::Name(APP_BOLD_FAMILY.into()),
                ));
            }
            text
        } else {
            egui::RichText::new(label)
        };
        let mut header = egui::CollapsingHeader::new(header_text)
            .id_salt(format!("tree_{}", dir.display()));
        if force_expand && is_ancestor {
            header = header.open(Some(true));
        } else if self.tree_collapse_frames > 0 && !is_ancestor {
            // Explorer-style: while the post-navigation window is open,
            // collapse every branch that isn't on the active path. `open`
            // persists through CollapsingState, so this sticks afterwards.
            header = header.open(Some(false));
        }
        let response = header.show(ui, |ui| {
            if let Ok(subdirs) = crate::fs_entry::list_subdirs(dir) {
                for subdir in subdirs {
                    self.show_dir_node(ui, &subdir, active_path, force_expand);
                }
            }
        });
        if is_active {
            // Keep centering the active folder while the post-navigation
            // scroll window is open (see `tree_scroll_frames`).
            if self.tree_scroll_frames > 0 {
                response.header_response.scroll_to_me(Some(egui::Align::Center));
            }
            // Row-wide selection tint behind the accent chip.
            let rect = response.header_response.rect;
            ui.painter().rect_filled(
                rect,
                4.0,
                ui.visuals().selection.bg_fill.gamma_multiply(0.45),
            );
        }
        // Navigate only when the folder is expanded (body visible), not when collapsing
        if response.header_response.clicked() && response.body_response.is_some() {
            if self.try_navigate_active(self.active_pane, dir.to_path_buf()) {
                self.dirty = true;
            }
        }
    }

    fn selected_paths(&self) -> Vec<PathBuf> {
        let tab = self.panes[self.active_pane].active_tab();
        tab.selected
            .iter()
            .map(|name| tab.path.join(name))
            .collect()
    }

    fn copy_selection(&mut self, ctx: &egui::Context) {
        let paths = self.selected_paths();
        if paths.is_empty() {
            self.status = "Nothing selected".into();
            return;
        }
        self.clipboard = paths;
        self.clipboard_op = Some(ClipboardOp::Copy);
        self.publish_clipboard_paths(ctx);
        self.status = format!("Copied {} item(s)", self.clipboard.len());
    }

    /// Publishes the current file clipboard as newline-joined path text on
    /// the OS clipboard. Two reasons: users can paste the paths into other
    /// apps, and egui-winit only emits `Event::Paste` (our Ctrl+V trigger)
    /// when the OS clipboard holds non-empty text — without this, pasting
    /// after an in-app copy would silently do nothing on an empty OS
    /// clipboard.
    fn publish_clipboard_paths(&self, ctx: &egui::Context) {
        let text = self
            .clipboard
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("\n");
        Self::set_clipboard_text(ctx, &text);
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
        self.status = "File path copied".into();
    }

    /// Copies the current folder path to the system clipboard.
    fn copy_folder_path(&mut self, ctx: &egui::Context) {
        let dir = self.active_tab_dir();
        let text = dir.to_string_lossy();
        Self::set_clipboard_text(ctx, &text);
        self.status = "Folder path copied".into();
    }

    /// Writes text to the OS clipboard via egui's output.
    fn set_clipboard_text(ctx: &egui::Context, text: &str) {
        ctx.copy_text(text.to_string());
    }

    fn cut_selection(&mut self, ctx: &egui::Context) {
        let paths = self.selected_paths();
        if paths.is_empty() {
            self.status = "Nothing selected".into();
            return;
        }
        self.clipboard = paths;
        self.clipboard_op = Some(ClipboardOp::Cut);
        self.publish_clipboard_paths(ctx);
        self.status = format!("Cut {} item(s)", self.clipboard.len());
    }

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

        self.transfer_items(items, dest, op);
    }

    /// Shared tail of clipboard-paste and drag & drop: checks name
    /// collisions up front with a cheap `Path::exists` (no recursive walk),
    /// preserving the original one-at-a-time duplicate-name prompt — the
    /// first colliding item stops the transfer and opens
    /// `Dialog::DuplicateName`. Once there are no collisions left, the whole
    /// batch runs as a single background operation with a progress bar,
    /// rather than blocking the UI thread — see
    /// `progress::copy_items_bg`/`move_items_bg`.
    fn transfer_items(&mut self, items: Vec<PathBuf>, dest: PathBuf, op: Option<ClipboardOp>) {
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

        self.background_op_dirs = vec![dest.clone()];
        if op == Some(ClipboardOp::Cut) {
            for src in &items {
                if let Some(parent) = src.parent() {
                    self.background_op_dirs.push(parent.to_path_buf());
                }
            }
        }
        self.background_op = Some(match op {
            Some(ClipboardOp::Copy) => progress::copy_items_bg(items, dest.clone()),
            _ => progress::move_items_bg(items, dest.clone()),
        });
        self.status = match op {
            Some(ClipboardOp::Cut) => format!("Moving into {}…", dest.display()),
            _ => format!("Copying into {}…", dest.display()),
        };
        if op == Some(ClipboardOp::Cut) {
            self.clipboard.clear();
        }
        self.panes[self.active_pane]
            .active_tab_mut()
            .clear_selection();
    }

    /// Per-frame handling of an in-flight file drag (started by dragging a
    /// listing row): hovering an inactive tab opens it browser-style so the
    /// drop can complete inside the newly opened tab; the pane under the
    /// pointer gets drop-target feedback; on release the dragged items are
    /// copied into the target pane's active folder — or MOVED when Shift is
    /// held. Releasing outside any pane (or pressing Escape) cancels.
    fn process_file_drag_drop(&mut self, ctx: &egui::Context) {
        let Some(payload) = egui::DragAndDrop::payload::<DragFiles>(ctx).map(|p| (*p).clone())
        else {
            return;
        };
        let (pos, released, shift) = ctx.input(|i| {
            (i.pointer.interact_pos(), i.pointer.primary_released(), i.modifiers.shift)
        });
        let Some(pos) = pos else { return };

        // Hovering a tab header opens that tab immediately, so the user can
        // finish the drop inside the tab that just came to the front.
        if !released {
            for &((pane_idx, tab_idx), rect, is_active) in &self.dnd_tab_rects {
                if !is_active && rect.contains(pos) && self.panes[pane_idx].active_tab != tab_idx {
                    self.panes[pane_idx].active_tab = tab_idx;
                    self.active_pane = pane_idx;
                    self.dirty = true;
                    break;
                }
            }
        }

        let target_pane = self
            .dnd_pane_rects
            .iter()
            .position(|r| r.is_some_and(|rect| rect.contains(pos)));

        if !released {
            // Drop-target feedback: brand-orange border around the receiving
            // pane, plus a copy/move/forbidden cursor.
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("dnd_highlight"),
            ));
            if let Some(rect) = target_pane.and_then(|p| self.dnd_pane_rects[p]) {
                painter.rect_stroke(
                    rect.expand(-1.0),
                    6.0,
                    egui::Stroke::new(2.5, egui::Color32::from_rgb(255, 165, 0)),
                    egui::StrokeKind::Inside,
                );
            }
            ctx.set_cursor_icon(if target_pane.is_none() {
                egui::CursorIcon::NoDrop
            } else if shift {
                egui::CursorIcon::Move
            } else {
                egui::CursorIcon::Copy
            });
            return;
        }

        // Release: transfer into the target pane's active folder — or cancel
        // when dropped outside every pane. (The plugin clears the payload at
        // end-of-pass anyway; clearing here keeps things explicit.)
        egui::DragAndDrop::clear_payload(ctx);
        let Some(target_pane) = target_pane else { return };
        let dest = self.panes[target_pane].active_tab().path.clone();
        if dest == payload.from_dir {
            self.status = "Source and destination are the same folder".to_string();
            return;
        }
        let op = if shift { ClipboardOp::Cut } else { ClipboardOp::Copy };
        self.clipboard = payload.paths.clone();
        self.clipboard_op = Some(op);
        self.transfer_items(payload.paths, dest, Some(op));
        // The dragged items left the source folder: forget its selection.
        if let Some(src_pane) = self.panes.get_mut(payload.from_pane) {
            src_pane.active_tab_mut().clear_selection();
        }
    }

    /// Handles files/folders dragged in from another application (e.g.
    /// Explorer, WinRAR): drops them (always copied — the source isn't ours
    /// to move from) into whichever pane/tab the pointer was actually over.
    fn process_external_file_drop(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        if dropped.is_empty() {
            return;
        }
        let paths: Vec<PathBuf> = dropped
            .into_iter()
            .map(|f| f.path().to_path_buf())
            .filter(|p| !p.as_os_str().is_empty())
            .collect();
        if paths.is_empty() {
            return;
        }
        // egui's own pointer position is stale here — see `native_cursor_pos`.
        let pos = native_cursor_pos(frame, ctx)
            .or_else(|| ctx.input(|i| i.pointer.interact_pos().or(i.pointer.hover_pos())));

        // Dropping directly on a tab header targets that tab specifically
        // (and brings it to front), matching the in-app drag & drop behavior.
        let tab_hit = pos.and_then(|pos| {
            self.dnd_tab_rects
                .iter()
                .find(|(_, rect, _)| rect.contains(pos))
                .map(|&((pane_idx, tab_idx), ..)| (pane_idx, tab_idx))
        });

        let (target_pane, dest) = if let Some((pane_idx, tab_idx)) = tab_hit {
            self.panes[pane_idx].active_tab = tab_idx;
            self.active_pane = pane_idx;
            self.dirty = true;
            (pane_idx, self.panes[pane_idx].tabs[tab_idx].path.clone())
        } else {
            let target_pane = pos
                .and_then(|pos| {
                    self.dnd_pane_rects
                        .iter()
                        .position(|r| r.is_some_and(|rect| rect.contains(pos)))
                })
                .unwrap_or(self.active_pane);
            (target_pane, self.panes[target_pane].active_tab().path.clone())
        };
        self.active_pane = target_pane;
        self.transfer_items(paths, dest, Some(ClipboardOp::Copy));
    }

    fn delete_selection(&mut self) {
        let paths = self.selected_paths();
        if paths.is_empty() {
            self.status = "Nothing selected".into();
            return;
        }
        self.dialog = Some(Dialog::ConfirmDelete { paths });
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
                self.mark_dir_dirty(&dest);
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
                    self.mark_dir_dirty(&dest);
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
        let mut dirty_dir: Option<PathBuf> = None;
        let result = match &dialog {
            Dialog::Rename { path, name } => fs_ops::rename_item(path, name)
                .map(|_| format!("Renamed to {name}"))
                .map_err(|err| format!("Rename failed: {err}")),
            Dialog::NewFolder { name } => {
                let names: Vec<&str> = name.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
                if names.is_empty() {
                    Err("Enter at least one folder name".to_string())
                } else {
                    let mut created = 0;
                    let mut errors = Vec::new();
                    for n in &names {
                        match fs_ops::create_folder(&parent, n) {
                            Ok(_) => created += 1,
                            Err(err) => errors.push(format!("{n}: {err}")),
                        }
                    }
                    if errors.is_empty() {
                        Ok(format!("Created {created} folder(s)"))
                    } else {
                        Err(format!(
                            "Created {created}/{}; {}",
                            names.len(),
                            errors.join("; ")
                        ))
                    }
                }
            }
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
            Dialog::NewUser { name } => {
                if name.trim().is_empty() {
                    Err("User name cannot be empty".to_string())
                } else {
                    match crate::user::create_user(&self.conn, name.trim()) {
                        Ok(id) => {
                            self.users = crate::user::list_users(&self.conn);
                            self.switch_user(id);
                            Ok(format!("Created user {}", name.trim()))
                        }
                        Err(err) => Err(format!("Create user failed: {err}")),
                    }
                }
            }
            Dialog::RenameTab { pane_idx, tab_idx, name } => {
                if let Some(tab) = self
                    .panes
                    .get_mut(*pane_idx)
                    .and_then(|p| p.tabs.get_mut(*tab_idx))
                {
                    tab.custom_name =
                        if name.trim().is_empty() { None } else { Some(name.trim().to_string()) };
                }
                Ok(String::new())
            }
            Dialog::TabContext { .. } | Dialog::Find { .. } | Dialog::Help
            | Dialog::ConfirmDelete { .. } => Ok(String::new()),
        };
        if result.is_ok() {
            dirty_dir = match &dialog {
                Dialog::Rename { path, .. } => path.parent().map(|p| p.to_path_buf()),
                Dialog::NewFolder { .. } | Dialog::NewFile { .. } => Some(parent.clone()),
                Dialog::DuplicateName { dest_dir, .. } => Some(dest_dir.clone()),
                Dialog::TabContext { .. } | Dialog::Find { .. } | Dialog::NewUser { .. } | Dialog::Help
                | Dialog::ConfirmDelete { .. } | Dialog::RenameTab { .. } => None,
            };
        }
        if let Some(dir) = dirty_dir {
            self.mark_dir_dirty(&dir);
        }
        self.status = match result {
            Ok(msg) if msg.is_empty() => self.status.clone(),
            Ok(msg) => msg,
            Err(msg) => msg,
        };
    }

    /// Settings page: theme, font family and size.
    fn settings_page_appearance(&mut self, ui: &mut egui::Ui) {
        settings_group_label(ui, "Theme");
        let mut pref = self.theme_pref;
        pref.radio_buttons(ui);
        if pref != self.theme_pref {
            self.theme_pref = pref;
            let _ = crate::config::set(
                &self.conn,
                crate::config::Scope::User(self.current_user_id),
                "theme",
                theme_pref_str(pref),
            );
        }

        ui.add_space(14.0);
        settings_group_label(ui, "Text");
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Font family").weak());
            ui.add_space(8.0);
            let fonts = [
                "Inter",
                "Segoe UI",
                "Arial",
                "Helvetica",
                "Times New Roman",
                "Courier New",
            ];
            let current_idx = fonts
                .iter()
                .position(|&f| f == self.font_family)
                .unwrap_or(0);
            let mut new_idx = current_idx;
            egui::ComboBox::from_id_salt("font_family_combo")
                .selected_text(&self.font_family)
                .width(180.0)
                .show_ui(ui, |ui| {
                    for (i, font) in fonts.iter().enumerate() {
                        if ui.selectable_label(i == current_idx, *font).clicked() {
                            new_idx = i;
                        }
                    }
                });
            if new_idx != current_idx {
                self.font_family = fonts[new_idx].to_string();
                let _ = crate::config::set(
                    &self.conn,
                    crate::config::Scope::User(self.current_user_id),
                    "font_family",
                    &self.font_family,
                );
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Size").weak());
            ui.add_space(8.0);
            // The global 3D-button fill is near-white so it reads against
            // the grey panel behind toolbar buttons; against this dialog's
            // white background it makes the slider handle disappear except
            // for its outline. Give the handle a fill that actually shows.
            let response = ui
                .scope(|ui| {
                    let v = &mut ui.style_mut().visuals.widgets;
                    let handle_fill = egui::Color32::from_rgb(150, 150, 156);
                    v.inactive.bg_fill = handle_fill;
                    v.inactive.weak_bg_fill = handle_fill;
                    v.hovered.bg_fill = egui::Color32::from_rgb(120, 120, 128);
                    v.hovered.weak_bg_fill = egui::Color32::from_rgb(120, 120, 128);
                    v.active.bg_fill = egui::Color32::from_rgb(90, 90, 96);
                    v.active.weak_bg_fill = egui::Color32::from_rgb(90, 90, 96);
                    ui.add(
                        egui::Slider::new(&mut self.font_size, 8.0..=24.0)
                            .step_by(0.5)
                            .suffix("px"),
                    )
                })
                .inner;
            if response.changed() {
                let _ = crate::config::set(
                    &self.conn,
                    crate::config::Scope::User(self.current_user_id),
                    "font_size",
                    &self.font_size.to_string(),
                );
            }
        });
        ui.add_space(14.0);
        settings_group_label(ui, "Tab Layout");
        let mut orientation = self.tab_orientation;
        ui.horizontal(|ui| {
            ui.radio_value(&mut orientation, TabOrientation::Horizontal, "Horizontal");
            ui.radio_value(&mut orientation, TabOrientation::Vertical, "Vertical");
        });
        if orientation != self.tab_orientation {
            self.tab_orientation = orientation;
            let _ = crate::config::set(
                &self.conn,
                crate::config::Scope::User(self.current_user_id),
                "tab_orientation",
                orientation.as_str(),
            );
        }

        ui.add_space(14.0);
        settings_group_label(ui, "Tips");
        let mut tips_on = self.tips_enabled;
        if ui
            .checkbox(&mut tips_on, "Show rotating feature tips")
            .changed()
        {
            self.tips_enabled = tips_on;
            self.tips.set_visible(tips_on);
            let _ = crate::config::set(
                &self.conn,
                crate::config::Scope::User(self.current_user_id),
                crate::tips::KEY_TIPS_ENABLED,
                if tips_on { "true" } else { "false" },
            );
        }
        ui.label(
            egui::RichText::new("Small hints about FileMan's functions, shown near the bottom-left corner.")
                .weak()
                .small(),
        );

        ui.add_space(10.0);
        ui.label(
            egui::RichText::new(
                "Changes apply immediately and are remembered per user.",
            )
            .weak()
            .small(),
        );
    }

    /// Settings page: keyboard shortcut rebinding table.
    fn settings_page_shortcuts(&mut self, ui: &mut egui::Ui) {
        ui.label(
            egui::RichText::new(
                "Click Rebind, then press the new key combination \
                 (Escape cancels).",
            )
            .weak()
            .small(),
        );
        ui.add_space(8.0);
        egui::Grid::new("shortcuts_grid")
            .num_columns(3)
            .spacing([16.0, 5.0])
            .striped(true)
            .show(ui, |ui| {
                for action in Action::ALL {
                    let combo = self
                        .shortcut_map
                        .iter()
                        .find(|(_, a)| **a == ActionRef::Builtin(action))
                        .map(|(c, _)| c.to_string())
                        .unwrap_or_else(|| "(none)".to_string());
                    ui.label(action.label());
                    ui.label(egui::RichText::new(&combo).weak());
                    let capturing = self.capturing_shortcut_for == Some(action);
                    let rebind_label = if capturing { "Press a key…" } else { "Rebind" };
                    if ui.button(rebind_label).clicked() {
                        self.capturing_shortcut_for = Some(action);
                    }
                    ui.end_row();
                }
            });
    }

    /// Settings page: which buttons appear on the main toolbar row.
    fn settings_page_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.label(
            egui::RichText::new(
                "Rearrange the main button row. Custom actions live on the \
                 second row automatically — see Custom Actions.",
            )
            .weak()
            .small(),
        );
        ui.add_space(8.0);

        let all_refs: Vec<ActionRef> = Action::ALL.into_iter().map(ActionRef::Builtin).collect();
        let mut layout_changed = false;
        let mut move_up: Option<usize> = None;
        let mut move_down: Option<usize> = None;
        for (idx, action_ref) in self.toolbar_actions.clone().iter().enumerate() {
            ui.horizontal(|ui| {
                if ui.small_button("▲").clicked() && idx > 0 {
                    move_up = Some(idx);
                }
                if ui.small_button("▼").clicked() && idx + 1 < self.toolbar_actions.len() {
                    move_down = Some(idx);
                }
                let mut included = true;
                if ui
                    .checkbox(&mut included, action_ref.label(&self.custom_actions))
                    .clicked()
                    && !included
                {
                    self.toolbar_actions.retain(|a| a != action_ref);
                    layout_changed = true;
                }
            });
        }
        if let Some(idx) = move_up {
            self.toolbar_actions.swap(idx, idx - 1);
            layout_changed = true;
        }
        if let Some(idx) = move_down {
            self.toolbar_actions.swap(idx, idx + 1);
            layout_changed = true;
        }

        ui.add_space(10.0);
        settings_group_label(ui, "Available buttons");
        ui.horizontal_wrapped(|ui| {
            for action_ref in &all_refs {
                if self.toolbar_actions.contains(action_ref) {
                    continue;
                }
                if ui
                    .small_button(format!("+ {}", action_ref.label(&self.custom_actions)))
                    .clicked()
                {
                    self.toolbar_actions.push(*action_ref);
                    layout_changed = true;
                }
            }
        });

        if layout_changed {
            let _ = crate::actions::set_layout(
                &self.conn,
                crate::actions::Scope::User(self.current_user_id),
                &self.toolbar_actions,
            );
        }
    }

    /// Settings page: manage user-defined "open with" actions that render as
    /// icon buttons on the toolbar's second row.
    fn settings_page_custom_actions(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        ui.label(
            egui::RichText::new(
                "Each action launches an application of your choice with the \
                 selected file as its argument. Buttons appear on the second \
                 toolbar row using the app's own icon.",
            )
            .weak()
            .small(),
        );
        ui.add_space(10.0);

        let mut remove: Option<i64> = None;
        for custom in self.custom_actions.clone() {
            if !self.custom_icons.contains_key(&custom.exe_path) {
                let tex =
                    crate::icon_cache::load_icon_texture(ctx, &custom.exe_path);
                self.custom_icons.insert(custom.exe_path.clone(), tex);
            }
            let icon = self.custom_icons.get(&custom.exe_path).cloned().flatten();
            ui.horizontal(|ui| {
                match &icon {
                    Some(tex) => {
                        ui.add(
                            egui::Image::new(egui::load::SizedTexture::new(
                                tex.id(),
                                egui::vec2(20.0, 20.0),
                            )),
                        );
                    }
                    None => {
                        ui.label(egui::RichText::new("⚙").weak());
                    }
                }
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(&custom.label).strong());
                    ui.label(
                        egui::RichText::new(&custom.exe_path).weak().small(),
                    );
                });
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        if ui.small_button("Remove").clicked() {
                            remove = Some(custom.id);
                        }
                    },
                );
            });
            ui.add_space(2.0);
            ui.separator();
        }
        if let Some(id) = remove {
            let _ = crate::actions::remove_custom_action(&self.conn, id);
            self.custom_actions =
                crate::actions::list_custom_actions(&self.conn, self.current_user_id);
            self.status = "Custom action removed".into();
        }

        // Add-action form.
        ui.add_space(12.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(egui::RichText::new("Add a custom action").strong());
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Name").weak());
                ui.add_space(8.0);
                let name_edit = ui.add_sized(
                    [ui.available_width(), 0.0],
                    egui::TextEdit::singleline(&mut self.new_custom_action_label),
                );
                if name_edit.changed() {
                    self.dirty = true;
                }
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Program").weak());
                ui.add_space(8.0);
                if ui.button("Browse…").clicked() {
                    self.new_custom_action_exe = rfd::FileDialog::new()
                        .set_title("Choose Executable")
                        .pick_file();
                }
                match &self.new_custom_action_exe {
                    Some(exe) => {
                        ui.label(
                            egui::RichText::new(exe.display().to_string()).weak(),
                        );
                    }
                    None => {
                        ui.label(egui::RichText::new("No program selected").weak());
                    }
                }
            });
            ui.add_space(8.0);
            let can_add = !self.new_custom_action_label.trim().is_empty()
                && self.new_custom_action_exe.is_some();
            ui.add_enabled_ui(can_add, |ui| {
                let add_btn = egui::Button::new("Add")
                    .fill(ui.visuals().selection.bg_fill);
                if ui.add(add_btn).clicked() {
                    if let Some(exe) = self.new_custom_action_exe.take() {
                        let label =
                            std::mem::take(&mut self.new_custom_action_label);
                        let _ = crate::actions::add_custom_action(
                            &self.conn,
                            self.current_user_id,
                            &label,
                            &exe.display().to_string(),
                        );
                        self.custom_actions = crate::actions::list_custom_actions(
                            &self.conn,
                            self.current_user_id,
                        );
                        self.status = format!("Added custom action \"{label}\"");
                    }
                }
            });
        });
    }

    /// Settings page: default listing view mode.
    fn settings_page_view_mode(&mut self, ui: &mut egui::Ui) {
        settings_group_label(ui, "Listing Layout");
        ui.label(egui::RichText::new(
            "Changes apply to the active tab immediately.",
        )
        .weak()
        .small());
        ui.add_space(6.0);
        let current_mode = self.panes[self.active_pane].active_tab().view_mode;
        ui.horizontal(|ui| {
            for (label, vm) in [
                ("Details", ViewMode::Details),
                ("List", ViewMode::List),
                ("Icons", ViewMode::Icons),
            ] {
                if ui.selectable_label(current_mode == vm, label).clicked()
                    && current_mode != vm
                {
                    self.panes[self.active_pane].active_tab_mut().view_mode = vm;
                    self.dirty = true;
                }
            }
        });
    }

    /// Settings page: system integration toggles.
    fn settings_page_advanced(&mut self, ui: &mut egui::Ui) {
        settings_group_label(ui, "Default Folder Explorer");
        if crate::win_default::is_default() {
            ui.label("FileMan is currently the default folder explorer on this PC.");
            if ui.button("Restore Windows default").clicked() {
                self.status = match crate::win_default::clear_default() {
                    Ok(()) => "Restored Windows Explorer as the folder default".into(),
                    Err(e) => format!("Restore failed: {e}"),
                };
            }
        } else {
            ui.label("Folders currently open in Windows Explorer.");
            if ui.button("Make FileMan the default").clicked() {
                self.status = match crate::win_default::set_default() {
                    Ok(()) => "FileMan is now the default folder explorer (already-open windows are unaffected)".into(),
                    Err(e) => format!("Setup failed: {e}"),
                };
            }
        }

        ui.add_space(14.0);
        settings_group_label(ui, "Migrate Settings");
        ui.label(
            egui::RichText::new(
                "Copy all settings — theme, fonts, tab layout, shortcuts, \
                 toolbar and custom actions — to another user on this PC or \
                 to FileMan on another machine via a JSON file.",
            )
            .weak()
            .small(),
        );
        ui.add_space(6.0);
        let user_name = self
            .users
            .iter()
            .find(|u| u.id == self.current_user_id)
            .map(|u| u.name.clone())
            .unwrap_or_else(|| "user".to_string());
        ui.horizontal(|ui| {
            if ui.button("Export settings…").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Export settings")
                    .set_file_name(format!("fileman-settings-{user_name}.json"))
                    .add_filter("FileMan settings", &["json"])
                    .save_file()
                {
                    let file = crate::migrate::collect(
                        &self.conn,
                        self.current_user_id,
                        Some(user_name.clone()),
                    );
                    match crate::migrate::write_to_path(&file, &path) {
                        Ok(()) => {
                            self.status = format!("Settings exported to {}", path.display())
                        }
                        Err(e) => self.status = e,
                    }
                }
            }
            if ui.button("Import settings…").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Import settings")
                    .add_filter("FileMan settings", &["json"])
                    .pick_file()
                {
                    match crate::migrate::read_from_path(&path).and_then(|f| {
                        crate::migrate::import_into(&self.conn, self.current_user_id, &f)
                    }) {
                        Ok(summary) => {
                            self.status = summary.describe();
                            self.reload_settings_from_db();
                        }
                        Err(e) => self.status = e,
                    }
                }
            }
        });

        ui.add_space(14.0);
        settings_group_label(ui, "About");
        ui.label(format!("FileMan v{}", env!("CARGO_PKG_VERSION")));
    }

    /// Re-reads every setting from the database after an import so the UI
    /// reflects the new values immediately.
    fn reload_settings_from_db(&mut self) {
        let uid = self.current_user_id;
        self.theme_pref = crate::config::get(&self.conn, uid, "theme")
            .map(|raw| parse_theme_pref(&raw))
            .unwrap_or_default();
        self.font_size = crate::config::get(&self.conn, uid, "font_size")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(14.0);
        self.font_family = crate::config::get(&self.conn, uid, "font_family")
            .unwrap_or_else(|| "Segoe UI".to_string());
        self.tab_orientation = crate::config::get(&self.conn, uid, "tab_orientation")
            .map(|raw| TabOrientation::parse(&raw))
            .unwrap_or_default();
        self.tab_strip_width = crate::config::get(&self.conn, uid, "tab_strip_width")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(150.0);
        self.tips_enabled = crate::config::get(&self.conn, uid, crate::tips::KEY_TIPS_ENABLED)
            .map(|raw| raw != "false")
            .unwrap_or(true);
        self.shortcut_map = crate::actions::load_shortcut_map(&self.conn, uid);
        self.toolbar_actions = crate::actions::load_toolbar(&self.conn, uid);
        self.custom_actions = crate::actions::list_custom_actions(&self.conn, uid);
        self.custom_icons.clear();
        // Force apply_fonts to run again even if the family string matches.
        self.fonts_applied_family = None;
    }

    /// Renders the settings dialog: Office-style left navigation rail plus a
    /// scrolling content pane for the selected category.
    fn show_settings_window(&mut self, ctx: &egui::Context) {
        const NAV_WIDTH: f32 = 168.0;
        let mut open = self.show_settings;
        if !open {
            return;
        }
        // Size is pinned to a percentage of the available screen every time
        // the dialog opens — never restored from stale persisted state —
        // so the content area is always generous.
        let avail = ctx.input(|i| i.viewport_rect());
        let size = egui::vec2(
            (avail.width() * 0.82).clamp(760.0, 1280.0),
            (avail.height() * 0.86).clamp(540.0, 940.0),
        );
        egui::Window::new("Settings")
            .id(egui::Id::new("settings_window_v3"))
            .open(&mut open)
            .fixed_size(size)
            .show(ctx, |ui| {
                // egui measures children before parents, so "available
                // height" queries can't see the minimums we set below.
                // Compute the interior height once and pin BOTH the root and
                // the nav/content row to it explicitly.
                let inner_h = (size.y - 52.0).max(320.0);
                ui.set_min_height(inner_h);
                ui.horizontal(|ui| {
                    ui.set_min_height(inner_h);
                    // ---- Navigation rail ----
                    ui.vertical(|ui| {
                        ui.set_min_width(NAV_WIDTH);
                        ui.set_max_width(NAV_WIDTH);
                        ui.add_space(2.0);
                        for (page, name) in [
                            (SettingsPage::Appearance, "Appearance"),
                            (SettingsPage::Shortcuts, "Keyboard Shortcuts"),
                            (SettingsPage::Toolbar, "Toolbar"),
                            (SettingsPage::CustomActions, "Custom Actions"),
                            (SettingsPage::ViewMode, "View"),
                            (SettingsPage::Advanced, "Advanced"),
                        ] {
                            let selected = self.settings_page == page;
                            let (rect, resp) = ui.allocate_exact_size(
                                egui::vec2(NAV_WIDTH - 12.0, 27.0),
                                egui::Sense::click(),
                            );
                            if resp.clicked() {
                                self.settings_page = page;
                            }
                            let visuals = ui.visuals();
                            let bg = if selected {
                                visuals.selection.bg_fill
                            } else if resp.hovered() {
                                visuals.widgets.hovered.bg_fill
                            } else {
                                egui::Color32::TRANSPARENT
                            };
                            ui.painter().rect_filled(rect, 4.0, bg);
                            let fg = if selected {
                                if visuals.dark_mode {
                                    egui::Color32::WHITE
                                } else {
                                    egui::Color32::BLACK
                                }
                            } else {
                                visuals.text_color()
                            };
                            // Crisp vector icon — independent of font/emoji
                            // coverage, so it renders identically everywhere.
                            let icon_rect = egui::Rect::from_center_size(
                                egui::pos2(rect.left() + 16.0, rect.center().y),
                                egui::vec2(17.0, 17.0),
                            );
                            paint_nav_icon(ui.painter(), icon_rect, page, fg);
                            let galley = ui.painter().layout_no_wrap(
                                name.to_owned(),
                                egui::FontId::proportional(self.font_size),
                                fg,
                            );
                            ui.painter().galley(
                                egui::pos2(
                                    icon_rect.right() + 8.0,
                                    rect.center().y - galley.size().y / 2.0,
                                ),
                                galley,
                                fg,
                            );
                        }
                        // App version pinned to the bottom of the nav rail.
                        // Sourced from Cargo.toml ([package] version) at
                        // compile time — never a hardcoded literal here, so
                        // it can't drift from the installer (which reads the
                        // same value from the built exe's version resource).
                        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                            ui.add_space(8.0);
                            ui.label(
                                egui::RichText::new(format!(
                                    "FileMan v{}",
                                    env!("CARGO_PKG_VERSION")
                                ))
                                .weak()
                                .small(),
                            );
                            ui.separator();
                        });
                    });
                    ui.separator();

                    // ---- Content pane ----
                    ui.with_layout(
                        egui::Layout::top_down_justified(egui::Align::Min),
                        |ui| {
                            egui::ScrollArea::vertical()
                                .id_salt("settings_content_scroll")
                                .auto_shrink(false)
                                .show(ui, |ui| {
                                    match self.settings_page {
                                        SettingsPage::Appearance => {
                                            settings_header(ui, "Appearance",
                                                "Personalize how FileMan looks.");
                                            self.settings_page_appearance(ui);
                                        }
                                        SettingsPage::Shortcuts => {
                                            settings_header(ui, "Keyboard Shortcuts",
                                                "Customize the key combinations for commands.");
                                            self.settings_page_shortcuts(ui);
                                        }
                                        SettingsPage::Toolbar => {
                                            settings_header(ui, "Toolbar",
                                                "Choose which buttons appear on the main row.");
                                            self.settings_page_toolbar(ui);
                                        }
                                        SettingsPage::CustomActions => {
                                            settings_header(ui, "Custom Actions",
                                                "Open files with your favourite applications.");
                                            self.settings_page_custom_actions(ctx, ui);
                                        }
                                        SettingsPage::ViewMode => {
                                            settings_header(ui, "View",
                                                "Choose the default listing layout.");
                                            self.settings_page_view_mode(ui);
                                        }
                                        SettingsPage::Advanced => {
                                            settings_header(ui, "Advanced",
                                                "System integration and application info.");
                                            self.settings_page_advanced(ui);
                                        }
                                    }
                                });
                        },
                    );
                });
            });
        self.show_settings = open;
    }

    fn show_tab_context_menu(&mut self, ctx: &egui::Context) {        // Keep the dialog alive across frames (no `take`) so the menu stays
        // on screen until an action is picked, Escape is pressed, or the
        // underlying tab no longer exists.
        let Some(Dialog::TabContext { pane_idx, tab_idx }) = self.dialog else {
            return;
        };
        let tab_valid = self
            .panes
            .get(pane_idx)
            .and_then(|p| p.tabs.get(tab_idx))
            .is_some();
        if !tab_valid {
            self.dialog = None;
            return;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.dialog = None;
            return;
        }
        {
            let path = self.panes[pane_idx].tabs[tab_idx].path.clone();
            let locked = self.panes[pane_idx].tabs[tab_idx].locked;
            let label = self.panes[pane_idx].tabs[tab_idx].display_label();
            let theme = ctx.theme();
            let mut win = egui::Window::new(&label)
                .title_bar(false)
                .resizable(false)
                .collapsible(false)
                .frame(
                    egui::Frame::new()
                        .inner_margin(10.0)
                        .corner_radius(6.0)
                        .fill(context_menu_fill(theme))
                        .stroke(context_menu_stroke(theme))
                        .shadow(egui::Shadow {
                            offset: [6, 10],
                            blur: 8,
                            spread: 0,
                            color: egui::Color32::from_black_alpha(96),
                        }),
                );
            // Anchor the menu at the right-click point, like a native
            // context menu.
            if let Some(pos) = self.tab_menu_pos.take() {
                win = win.fixed_pos(pos);
            }
            win.show(&ctx, |ui| {
                    if ui.button("Duplicate Tab").clicked() {
                        self.panes[pane_idx].open_tab(path.clone());
                        self.dirty = true;
                        self.dialog = None;
                    }
                    // Move the tab across the split; disabled for a pane's
                    // last tab so no pane is ever left empty.
                    let can_move = self.panes[pane_idx].tabs.len() > 1;
                    if ui
                        .add_enabled(can_move, egui::Button::new("Move to Other Pane"))
                        .clicked()
                    {
                        let mut tab = self.panes[pane_idx].tabs.remove(tab_idx);
                        {
                            let src = &mut self.panes[pane_idx];
                            if src.active_tab >= src.tabs.len() {
                                src.active_tab = src.active_tab.saturating_sub(1);
                            }
                        }
                        tab.listing_dirty = true;
                        self.panes[1 - pane_idx].tabs.push(tab);
                        let new_idx = self.panes[1 - pane_idx].tabs.len() - 1;
                        self.panes[1 - pane_idx].active_tab = new_idx;
                        self.active_pane = 1 - pane_idx;
                        self.dirty = true;
                        self.dialog = None;
                    }
                    if ui
                        .add_enabled(
                            !locked,
                            egui::Button::new("Close Tab"),
                        )
                        .on_disabled_hover_text("Unpin the tab first")
                        .clicked()
                    {
                        self.panes[pane_idx].close_tab(tab_idx);
                        self.dirty = true;
                        self.dialog = None;
                    }
                    if ui.button(if locked { "Unpin Tab" } else { "Pin Tab" }).clicked() {
                        self.panes[pane_idx].tabs[tab_idx].locked = !locked;
                        self.dirty = true;
                        self.dialog = None;
                    }
                    // Renaming is allowed even for pinned tabs — it only
                    // changes the label, not the folder.
                    if ui.button("Rename Tab").clicked() {
                        self.dialog = Some(Dialog::RenameTab {
                            pane_idx,
                            tab_idx,
                            name: label.clone(),
                        });
                    }
                });
        }
    }

    /// One file-browser pane: click-to-focus background, the tab strip
    /// (horizontal row or vertical sidebar), and the pane content.
    fn show_pane_body(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, pane_idx: usize, is_active: bool) {
        // Record this pane's extent for the drag & drop hit-test pass.
        self.dnd_pane_rects[pane_idx] = Some(ui.max_rect());

        // Click on pane background to set as active
        let pane_resp = ui.interact(ui.max_rect(), egui::Id::new(("pane_bg", pane_idx)), egui::Sense::click());
        if pane_resp.clicked() {
            self.active_pane = pane_idx;
            self.dirty = true;
        }

        let result = self.show_tab_strip(ui, pane_idx, is_active);

        // Show tab context menu via dialog
        if let Some(idx) = result.context_menu {
            self.tab_menu_pos = result.menu_pos;
            self.dialog = Some(Dialog::TabContext { pane_idx, tab_idx: idx });
        }
        {
            let pane = &mut self.panes[pane_idx];
            if let Some(idx) = result.clicked {
                pane.active_tab = idx;
            }
            if let Some(idx) = result.closed {
                if pane.tabs[idx].locked {
                    self.status =
                        "Tab is pinned — unpin it before closing (right-click the tab)".to_string();
                } else {
                    pane.close_tab(idx);
                    self.dirty = true;
                }
            }
            if result.opened {
                let current_path = pane.active_tab().path.clone();
                pane.open_tab(current_path);
                self.dirty = true;
            }
        }

        match result.content_rect {
            Some(rect) => {
                ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                    self.show_pane_content(ui, ctx, pane_idx);
                });
            }
            None => self.show_pane_content(ui, ctx, pane_idx),
        }
    }

    /// Renders a pane's tab strip: either the classic row above the content,
    /// or a fixed-width sidebar column on the left with the content beside it.
    /// Returns the rect to draw the pane content in; `None` means "continue
    /// in the normal flow below the strip".
    fn show_tab_strip(&mut self, ui: &mut egui::Ui, pane_idx: usize, is_active: bool) -> TabStripResult {
        let mut clicked = None;
        let mut closed = None;
        let mut opened = false;
        let mut context_menu = None;
        let mut menu_pos: Option<egui::Pos2> = None;
        let mut hover: Option<(usize, usize)> = None;
        match self.tab_orientation {
            TabOrientation::Horizontal => {
                let mut tab_rects: Vec<((usize, usize), egui::Rect, bool)> = Vec::new();
                ui.horizontal(|ui| {
                    let pane = &mut self.panes[pane_idx];
                    for (tab_idx, tab) in pane.tabs.iter().enumerate() {
                        let label = tab.display_label();
                        let is_tab_active = tab_idx == pane.active_tab;
                        let ev = tab_strip_item(
                            ui,
                            &label,
                            (pane_idx, tab_idx),
                            is_tab_active,
                            is_active,
                            tab.locked,
                            &mut hover,
                            None,
                        );
                        tab_rects.push(((pane_idx, tab_idx), ev.rect, is_tab_active));
                        clicked = clicked.or(ev.clicked.then_some(tab_idx));
                        context_menu = context_menu.or(ev.secondary_clicked.then_some(tab_idx));
                        closed = closed.or(ev.close_clicked.then_some(tab_idx));
                        menu_pos = menu_pos.or(ev.secondary_pos);
                    }
                    if ui.button("+").clicked() {
                        opened = true;
                    }
                });
                self.dnd_tab_rects.extend(tab_rects);
                TabStripResult { clicked, closed, opened, context_menu, content_rect: None, menu_pos }
            }
            TabOrientation::Vertical => {
                const GAP: f32 = 10.0;
                const HANDLE_W: f32 = 6.0;
                const MIN_STRIP_W: f32 = 90.0;
                let avail = ui.available_rect_before_wrap();
                // Keep the handle inside the pane even when the stored width
                // was saved against a larger window.
                let max_w = (avail.width() - HANDLE_W - GAP).max(MIN_STRIP_W);
                self.tab_strip_width = self.tab_strip_width.clamp(MIN_STRIP_W, max_w);

                let strip_rect = egui::Rect::from_min_max(
                    avail.min,
                    egui::pos2(avail.min.x + self.tab_strip_width, avail.max.y),
                );
                let handle_rect = egui::Rect::from_min_size(
                    egui::pos2(strip_rect.max.x, avail.min.y),
                    egui::vec2(HANDLE_W, avail.height()),
                );
                let content_rect = egui::Rect::from_two_pos(
                    egui::pos2(handle_rect.max.x + GAP * 0.5, avail.min.y),
                    avail.max,
                );

                let mut tab_rects: Vec<((usize, usize), egui::Rect, bool)> = Vec::new();
                ui.scope_builder(egui::UiBuilder::new().max_rect(strip_rect), |ui| {
                    let pane = &mut self.panes[pane_idx];
                    let row_w = ui.available_width();
                    // Single-line height by default; a row only grows to two
                    // lines when its label actually needs wrapping.
                    let single_h = ui.spacing().interact_size.y.max(22.0);
                    let double_h = single_h.max(self.font_size * 2.0 + 10.0);
                    let text_w = (row_w - 6.0 - 20.0).max(1.0);
                    for (tab_idx, tab) in pane.tabs.iter().enumerate() {
                        let label = tab.display_label();
                        let needs_two_lines = ui
                            .painter()
                            .layout_no_wrap(
                                label.clone(),
                                egui::FontId::proportional(self.font_size),
                                egui::Color32::WHITE,
                            )
                            .size()
                            .x
                            > text_w;
                        let row_h = if needs_two_lines { double_h } else { single_h };
                        let is_tab_active = tab_idx == pane.active_tab;
                        let ev = tab_strip_item(
                            ui,
                            &label,
                            (pane_idx, tab_idx),
                            is_tab_active,
                            is_active,
                            tab.locked,
                            &mut hover,
                            Some(egui::vec2(row_w, row_h)),
                        );
                        tab_rects.push(((pane_idx, tab_idx), ev.rect, is_tab_active));
                        clicked = clicked.or(ev.clicked.then_some(tab_idx));
                        context_menu = context_menu.or(ev.secondary_clicked.then_some(tab_idx));
                        closed = closed.or(ev.close_clicked.then_some(tab_idx));
                        menu_pos = menu_pos.or(ev.secondary_pos);
                    }
                    ui.add_space(2.0);
                    if ui
                        .add_sized([row_w, single_h], egui::Button::new("+"))
                        .clicked()
                    {
                        opened = true;
                    }
                });
                self.dnd_tab_rects.extend(tab_rects);

                let drag_resp = ui.interact(
                    handle_rect,
                    egui::Id::new(("tab_strip_handle", pane_idx)),
                    egui::Sense::drag(),
                );
                if drag_resp.hovered() || drag_resp.dragged() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                }
                if drag_resp.dragged() {
                    self.tab_strip_width =
                        (self.tab_strip_width + drag_resp.drag_delta().x).clamp(MIN_STRIP_W, max_w);
                }
                if drag_resp.drag_stopped() {
                    let _ = crate::config::set(
                        &self.conn,
                        crate::config::Scope::User(self.current_user_id),
                        "tab_strip_width",
                        &self.tab_strip_width.to_string(),
                    );
                    self.dirty = true;
                }

                // Thin divider line normally; a wide highlighted bar with grip
                // dots while the user is about to drag (or is dragging), so the
                // handle is discoverable.
                if drag_resp.hovered() || drag_resp.dragged() {
                    ui.painter().rect_filled(
                        handle_rect,
                        3.0,
                        ui.visuals().widgets.active.bg_fill,
                    );
                    let grip_color = ui.visuals().widgets.noninteractive.fg_stroke.color;
                    let center_y = handle_rect.center().y;
                    for i in -2..=2_i32 {
                        ui.painter().circle_filled(
                            egui::pos2(handle_rect.center().x, center_y + i as f32 * 6.0),
                            1.5,
                            grip_color,
                        );
                    }
                } else {
                    ui.painter().vline(
                        strip_rect.max.x + GAP * 0.5,
                        avail.y_range(),
                        ui.visuals().widgets.noninteractive.bg_stroke,
                    );
                }
                TabStripResult { clicked, closed, opened, context_menu, content_rect: Some(content_rect), menu_pos }
            }
        }
    }

    /// Everything below a pane's tab strip: address bar, filter, navigation /
    /// view-mode toolbar, and the file listing.
    fn show_pane_content(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, pane_idx: usize) {
        let pane = &mut self.panes[pane_idx];

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
        // Explorer-style framed address field.
        egui::Frame::new()
            .fill(ui.visuals().window_fill())
            .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
            .corner_radius(4.0)
            .inner_margin(egui::Margin::same(3))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("📁");
                    let address_id = egui::Id::new(("address_bar", pane_idx));
                    let address_resp = ui.add(
                        egui::TextEdit::singleline(&mut pane.address_bar)
                            .id(address_id)
                            .desired_width(f32::INFINITY)
                            .hint_text("Type a path and press Enter...")
                            .frame(
                                egui::Frame::new()
                                    .fill(egui::Color32::TRANSPARENT)
                                    .stroke(egui::Stroke::NONE),
                            ),
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
                            if pane.active_tab_mut().try_navigate(target) {
                                self.active_pane = pane_idx;
                                self.dirty = true;
                            } else {
                                self.status =
                                    "Tab is pinned — unpin it to navigate".to_string();
                            }
                        } else {
                            self.status = format!("Path not found: {}", pane.address_bar.trim());
                        }
                    }
                });
            });

        ui.horizontal(|ui| {
            if ui.button("⬅").on_hover_text("Back").clicked() {
                if pane.active_tab().locked {
                    self.status = "Tab is pinned — unpin it to navigate".to_string();
                } else if pane.active_tab_mut().go_back() {
                    self.dirty = true;
                }
            }
            if ui.button("➡").on_hover_text("Forward").clicked() {
                if pane.active_tab().locked {
                    self.status = "Tab is pinned — unpin it to navigate".to_string();
                } else if pane.active_tab_mut().go_forward() {
                    self.dirty = true;
                }
            }
            if ui.button("⬆").on_hover_text("Up").clicked() {
                if let Some(parent) = current_path.parent() {
                    if pane.active_tab_mut().try_navigate(parent.to_path_buf()) {
                        self.dirty = true;
                    } else {
                        self.status = "Tab is pinned — unpin it to navigate".to_string();
                    }
                }
            }

            ui.separator();
            let tab = pane.active_tab_mut();
            let filter_active = !tab.filter.is_empty();
            let accent = ui.visuals().selection.bg_fill;
            let mut text_edit = egui::TextEdit::singleline(&mut tab.filter)
                .id(egui::Id::new(("filter_input", pane_idx)))
                .hint_text("Filter...")
                .desired_width(160.0);
            if filter_active {
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
                tab.clear_selection();
            }
            if filter_active {
                if ui.small_button(egui::RichText::new("×").color(egui::Color32::from_rgb(196, 43, 28))).on_hover_text("Clear filter").clicked() {
                    tab.filter.clear();
                }
            }
        });

        let listing_result: Result<Vec<crate::fs_entry::FsEntry>, String> =
            match &pane.active_tab().listing_error {
                Some(err) => Err(err.clone()),
                None => Ok(pane.active_tab().listing.clone()),
            };
        match listing_result {
            Ok(mut entries) => {
                // Apply this tab's search filter
                let query = pane.active_tab().filter.clone();
                search::filter_entries(&mut entries, &query);
                let (sort_col, sort_asc) = {
                    let tab = pane.active_tab();
                    (tab.sort_col.clone(), tab.sort_asc)
                };
                crate::fs_entry::sort_entries(&mut entries, &sort_col, sort_asc);
                // Lazily extract+cache the shell-associated app icon for
                // each file in this listing; slots align 1:1 with `entries`
                // (None for folders and unresolvable types).
                let entry_icons =
                    crate::icon_cache::ensure_entry_icons(&mut self.file_icons, ctx, &entries);
                let ctrl = ui.input(|i| i.modifiers.ctrl);
                let shift = ui.input(|i| i.modifiers.shift);
                let mode = pane.active_tab().view_mode;
                // Maximum-contrast listing text: pure white on dark, pure
                // black on light — the theme defaults are softer than ideal
                // for long stretches of filenames.
                let listing_text = if ui.visuals().dark_mode {
                    egui::Color32::WHITE
                } else {
                    egui::Color32::BLACK
                };

                let mut select_name: Option<String> = None;
                let mut select_index: Option<usize> = None;
                let mut nav_target: Option<PathBuf> = None;
                let mut open_target: Option<PathBuf> = None;
                let mut row_action: Option<RowAction> = None;
                let mut drag_start: Option<String> = None;

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
                                    .resizable(true)
                                    .sense(egui::Sense::click_and_drag())
                                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                                    .column(egui_extras::Column::initial(col_w[0]).clip(true))
                                    .column(egui_extras::Column::initial(col_w[1]).clip(true))
                                    .column(egui_extras::Column::initial(col_w[2]).clip(true))
                                    .column(egui_extras::Column::initial(col_w[3]).clip(true))
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
                                                // Folders keep their emoji glyph;
                                                // files show the associated app
                                                // icon for their type, falling
                                                // back to bare text when none.
                                                ui.horizontal(|ui| {
                                                    if entry.is_dir {
                                                        ui.label(egui::RichText::new("\u{1F4C1}").color(listing_text));
                                                    } else if let Some(tex) = &entry_icons[row_idx] {
                                                        ui.add(egui::Image::new(
                                                            egui::load::SizedTexture::new(
                                                                tex.id(),
                                                                egui::vec2(16.0, 16.0),
                                                            ),
                                                        ));
                                                    }
                                                    ui.add(
                                                        egui::Label::new(
                                                            egui::RichText::new(entry.name.as_str())
                                                                .color(listing_text),
                                                        )
                                                        .selectable(false),
                                                    );
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
                                                ui.label(egui::RichText::new(text).color(listing_text));
                                            });
                                            row.col(|ui| {
                                                let size_text = if entry.is_dir {
                                                    String::new()
                                                } else {
                                                    format_file_size(entry.size)
                                                };
                                                ui.label(egui::RichText::new(size_text).color(listing_text));
                                            });
                                            row.col(|ui| {
                                                if entry.archive {
                                                    ui.label(egui::RichText::new("A").color(listing_text));
                                                }
                                            });

                                            let row_resp = row.response();
                                            if row_resp.clicked() {
                                                select_name = Some(entry.name.clone());
                                                select_index = Some(row_idx);
                                            }
                                            if row_resp.double_clicked() {
                                                if entry.is_dir {
                                                    nav_target = Some(entry.path.clone());
                                                } else {
                                                    open_target = Some(entry.path.clone());
                                                }
                                            }
                                            if row_resp.secondary_clicked() && !is_selected {
                                                select_name = Some(entry.name.clone());
                                                select_index = Some(row_idx);
                                            }
                                            // The table carries click_and_drag
                                            // sense, so a row drag starts a
                                            // copy/move gesture without
                                            // interfering with clicks.
                                            if row_resp.drag_started() {
                                                drag_start = Some(entry.name.clone());
                                            }
                                            styled_context_menu(&row_resp, |ui| {
                                                show_entry_context_menu(ui, &mut row_action, &entry.path, entry.is_dir);
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
                                    let resp = ui
                                        .horizontal(|ui| {
                                            if entry.is_dir {
                                                ui.label(egui::RichText::new("\u{1F4C1}").color(listing_text));
                                            } else if let Some(tex) = &entry_icons[idx] {
                                                ui.add(egui::Image::new(
                                                    egui::load::SizedTexture::new(
                                                        tex.id(),
                                                        egui::vec2(16.0, 16.0),
                                                    ),
                                                ));
                                            }
                                            ui.selectable_label(
                                                is_selected,
                                                egui::RichText::new(entry.name.as_str())
                                                    .color(listing_text),
                                            )
                                        })
                                        .inner;
                                    handle_entry_response(
                                        &resp, entry, is_selected,
                                        &mut select_name,
                                        &mut select_index,
                                        &mut nav_target,
                                        &mut open_target,
                                        idx,
                                    );
                                    let drag_zone = ui.interact(
                                        resp.rect,
                                        egui::Id::new(("entry_dnd", pane_idx, idx)),
                                        egui::Sense::drag(),
                                    );
                                    if drag_zone.drag_started() {
                                        drag_start = Some(entry.name.clone());
                                    }
                                    styled_context_menu(&resp, |ui| {
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
                                        ui.allocate_ui(egui::vec2(76.0, 72.0), |ui| {
                                            // Tile: associated app icon (or the
                                            // generic glyph) above the filename.
                                            // The union of both responses drives
                                            // selection/opening so clicking either
                                            // part works.
                                            let resp = ui
                                                .vertical_centered(|ui| {
                                                    let img_resp = if entry.is_dir {
                                                        ui.label(egui::RichText::new("🗀").color(listing_text))
                                                    } else if let Some(tex) = &entry_icons[idx] {
                                                        ui.add(egui::Image::new(
                                                            egui::load::SizedTexture::new(
                                                                tex.id(),
                                                                egui::vec2(32.0, 32.0),
                                                            ),
                                                        ))
                                                    } else {
                                                        ui.label(egui::RichText::new("🗋").color(listing_text))
                                                    };
                                                    let text_resp = ui.selectable_label(
                                                        is_selected,
                                                        egui::RichText::new(entry.name.as_str())
                                                            .color(listing_text),
                                                    );
                                                    img_resp | text_resp
                                                })
                                                .inner;
                                            handle_entry_response(
                                                &resp, entry, is_selected,
                                                &mut select_name,
                                                &mut select_index,
                                                &mut nav_target,
                                                &mut open_target,
                                                idx,
                                            );
                                            let drag_zone = ui.interact(
                                                resp.rect,
                                                egui::Id::new(("entry_dnd", pane_idx, idx)),
                                                egui::Sense::drag(),
                                            );
                                            if drag_zone.drag_started() {
                                                drag_start = Some(entry.name.clone());
                                            }
                                            styled_context_menu(&resp, |ui| {
                                                show_entry_context_menu(ui, &mut row_action, &entry.path, entry.is_dir);
                                            });
                                        });
                                    }
                                });
                            });
                    }
                }

                // A row drag starts a copy/move gesture: make sure the
                // dragged entry is part of the selection, then publish the
                // selected paths as the drag payload. Modifiers are ignored
                // at drag START on purpose — Shift+drag means "move" at drop
                // time, not range-select.
                if let Some(name) = drag_start.take() {
                    if !pane.active_tab().selected.contains(&name) {
                        pane.active_tab_mut().select_only(&name);
                    }
                    self.last_selected_index = None;
                    self.active_pane = pane_idx;
                    let tab = pane.active_tab();
                    egui::DragAndDrop::set_payload(
                        ctx,
                        DragFiles {
                            paths: tab.selected.iter().map(|n| tab.path.join(n)).collect(),
                            from_dir: tab.path.clone(),
                            from_pane: pane_idx,
                        },
                    );
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
                    let pinned = pane.active_tab().locked;
                    if pinned {
                        // A pinned tab never moves: open the folder in a new
                        // tab placed right beside it instead.
                        let insert_at = pane.active_tab + 1;
                        pane.tabs.insert(insert_at, crate::tab::Tab::new(target));
                        pane.active_tab = insert_at;
                        self.active_pane = pane_idx;
                        self.dirty = true;
                    } else if pane.active_tab_mut().try_navigate(target) {
                        self.active_pane = pane_idx;
                        self.dirty = true;
                    }
                }
                if let Some(target) = open_target {
                    let _ = std::process::Command::new("cmd")
                        .args(["/C", "start", "", &target.to_string_lossy()])
                        .spawn();
                }
                if let Some(action) = row_action {
                    self.active_pane = pane_idx;
                    match action {
                        RowAction::Copy => self.copy_selection(ctx),
                        RowAction::Cut => self.cut_selection(ctx),
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
                        RowAction::CopyName => self.copy_filename(ctx),
                        RowAction::CopyFolderPath => self.copy_folder_path(ctx),
                        RowAction::ExtractHere => self.extract_here(),
                        RowAction::ExtractTo => self.extract_to(),
                        RowAction::FavouriteFolder(path) => {
                            let path_str = path.display().to_string();
                            if crate::db::is_favourite(&self.conn, self.current_user_id, &path_str) {
                                self.remove_favourite(&path_str);
                            } else {
                                if crate::db::add_favourite(&self.conn, self.current_user_id, &path_str).is_ok() {
                                    self.favourites = crate::db::get_favourites(&self.conn, self.current_user_id);
                                    self.status = format!("Added to favourites: {}", path.display());
                                }
                            }
                        }
                        RowAction::OpenWith(path) => {
                            let _ = std::process::Command::new("cmd")
                                .args(["/C", "rundll32.exe", "shell32.dll,OpenAs_RunDLL", &path.to_string_lossy()])
                                .spawn();
                        }
                        RowAction::OpenInExplorer(path) => {
                            let _ = std::process::Command::new("explorer").arg(&path).spawn();
                        }
                    }
                }
            }
            Err(err) => {
                ui.colored_label(egui::Color32::RED, format!("Error: {err}"));
            }
        }
    }
}

/// egui-winit intercepts Ctrl+C/X/V at the winit layer and converts them
/// into `Event::Copy`/`Event::Cut`/`Event::Paste` — the underlying
/// `Event::Key` for C/X/V is never emitted, so `KeyCombo::matches_input`
/// can never see those presses. Map a clipboard event back to the combo of
/// the currently-held modifiers plus C/X/V, so the shortcut map (and the
/// rebind capture) treat them like any other key combination.
fn clipboard_event_combo(i: &egui::InputState) -> Option<crate::actions::KeyCombo> {
    let key = i.events.iter().rev().find_map(|e| match e {
        egui::Event::Copy => Some(egui::Key::C),
        egui::Event::Cut => Some(egui::Key::X),
        egui::Event::Paste(_) => Some(egui::Key::V),
        _ => None,
    })?;
    Some(crate::actions::KeyCombo::new(
        i.modifiers.ctrl,
        i.modifiers.shift,
        i.modifiers.alt,
        key,
    ))
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
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        ctx.set_theme(self.theme_pref);
        for theme in [egui::Theme::Dark, egui::Theme::Light] {
            ctx.style_mut_of(theme, |style| {
                // Compact, Windows command-bar density.
                style.spacing.item_spacing = egui::vec2(8.0, 4.0);
                style.spacing.button_padding = egui::vec2(8.0, 4.0);
                style.spacing.menu_margin = egui::Margin::same(4);
                // egui's default 3px resize grab radius is too thin to hit
                // reliably on the folder tree panel's edge, which (unlike the
                // custom pane divider) has no permanent visible handle.
                style.interaction.resize_grab_radius_side = 8.0;
                let font_id = egui::FontId::new(self.font_size, egui::FontFamily::Proportional);
                style
                    .text_styles
                    .insert(egui::TextStyle::Heading, font_id.clone());
                style.text_styles.insert(egui::TextStyle::Body, font_id);
                style.text_styles.insert(
                    egui::TextStyle::Monospace,
                    egui::FontId::new(self.font_size, egui::FontFamily::Monospace),
                );
                // Windows 11-style gently rounded widgets everywhere.
                for state in [
                    &mut style.visuals.widgets.noninteractive,
                    &mut style.visuals.widgets.inactive,
                    &mut style.visuals.widgets.hovered,
                    &mut style.visuals.widgets.active,
                    &mut style.visuals.widgets.open,
                ] {
                    state.corner_radius = egui::CornerRadius::same(4);
                }
                style.visuals.window_corner_radius = egui::CornerRadius::same(8);
                style.visuals.menu_corner_radius = egui::CornerRadius::same(6);
            });
        }
        // Windows 11-inspired palettes: dark chrome (#202020 panels, #2B2B2B
        // floating surfaces) and light chrome (#F3F3F3 panels, white cards),
        // with the system accent blue for selections.
        ctx.style_mut_of(egui::Theme::Dark, |style| {
            let v = &mut style.visuals;
            v.panel_fill = egui::Color32::from_rgb(32, 32, 32);
            v.window_fill = egui::Color32::from_rgb(43, 43, 43);
            v.extreme_bg_color = egui::Color32::from_rgb(25, 25, 25);
            v.window_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(63, 63, 63));
            v.selection.bg_fill = egui::Color32::from_rgb(0, 120, 212);
            v.selection.stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(76, 194, 255));
            v.hyperlink_color = egui::Color32::from_rgb(76, 194, 255);
            v.override_text_color = Some(egui::Color32::from_rgb(240, 240, 240));
            v.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(220, 220, 220));
            v.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(240, 240, 240));
            v.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);
            v.widgets.active.fg_stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);
            v.widgets.open.fg_stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);
            // 3D button treatment: raised resting face with a dark bevel
            // edge, a brighter border + 1px lift on hover, and a sunken
            // darker fill while pressed.
            v.widgets.inactive.bg_fill = egui::Color32::from_rgb(50, 50, 54);
            v.widgets.inactive.weak_bg_fill = egui::Color32::from_rgb(50, 50, 54);
            v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(16, 16, 18));
            v.widgets.hovered.bg_fill = egui::Color32::from_rgb(60, 60, 66);
            v.widgets.hovered.weak_bg_fill = egui::Color32::from_rgb(60, 60, 66);
            v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(110, 110, 120));
            v.widgets.hovered.expansion = 1.0;
            v.widgets.active.bg_fill = egui::Color32::from_rgb(28, 28, 31);
            v.widgets.active.weak_bg_fill = egui::Color32::from_rgb(28, 28, 31);
            v.widgets.active.bg_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(8, 8, 10));
        });
        ctx.style_mut_of(egui::Theme::Light, |style| {
            let v = &mut style.visuals;
            v.panel_fill = egui::Color32::from_rgb(243, 243, 243);
            v.window_fill = egui::Color32::from_rgb(255, 255, 255);
            v.extreme_bg_color = egui::Color32::from_rgb(255, 255, 255);
            v.window_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(203, 203, 203));
            v.selection.bg_fill = egui::Color32::from_rgb(206, 231, 255);
            v.hyperlink_color = egui::Color32::from_rgb(0, 95, 184);
            // 3D button treatment (light theme): white face, grey bevel,
            // hover lift, pressed-in grey.
            v.widgets.inactive.bg_fill = egui::Color32::from_rgb(252, 252, 253);
            v.widgets.inactive.weak_bg_fill = egui::Color32::from_rgb(252, 252, 253);
            v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(173, 173, 178));
            v.widgets.hovered.bg_fill = egui::Color32::WHITE;
            v.widgets.hovered.weak_bg_fill = egui::Color32::WHITE;
            v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(120, 120, 128));
            v.widgets.hovered.expansion = 1.0;
            v.widgets.active.bg_fill = egui::Color32::from_rgb(222, 222, 226);
            v.widgets.active.weak_bg_fill = egui::Color32::from_rgb(222, 222, 226);
            v.widgets.active.bg_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(150, 150, 156));
        });

        if self.fonts_applied_family.as_deref() != Some(self.font_family.as_str()) {
            apply_fonts(&ctx, &self.font_family, &mut self.status);
            self.fonts_applied_family = Some(self.font_family.clone());
            self.fonts_pending_apply = true;
        } else if self.fonts_pending_apply {
            self.fonts_pending_apply = false;
        }

        if !self.taskbar_badge_applied {
            self.taskbar_badge_applied = true;
            crate::taskbar::apply_instance_icon(frame, self.instance_slot);
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
                self.last_monitor_name = current_monitor_name(frame);
                self.dirty = true;
            }
        }

        // While the Settings "Shortcuts" tab is waiting for a rebind, the
        // next key event is captured here instead of dispatching normally.
        if let Some(action) = self.capturing_shortcut_for {
            // Pressing e.g. Ctrl+Shift+C streams separate Key events for
            // Ctrl and Shift themselves before C arrives — so scan BACKWARD,
            // ignore bare modifier-key events, and bind the last real key.
            // Escape cancels the rebind.
            let cancelled =
                ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Key { key: egui::Key::Escape, pressed: true, .. })));
            let combo = ctx.input(|i| {
                // Ctrl+C/X/V never arrive as `Event::Key` — egui-winit
                // converts them into clipboard events (see
                // `clipboard_event_combo`) — so check those first,
                // otherwise those combos could never be (re)bound.
                clipboard_event_combo(i).or_else(|| {
                    i.events.iter().rev().find_map(|e| match e {
                        egui::Event::Key { key, pressed: true, repeat: false, modifiers, .. } => {
                            // `Key::Copy`/`Cut`/`Paste` only arrive from
                            // dedicated hardware keys here — don't let them
                            // shadow the actual key being pressed.
                            let is_synthetic = matches!(
                                key,
                                egui::Key::Copy | egui::Key::Cut | egui::Key::Paste
                            );
                            if is_synthetic {
                                None
                            } else {
                                Some(crate::actions::KeyCombo::new(
                                    modifiers.ctrl,
                                    modifiers.shift,
                                    modifiers.alt,
                                    *key,
                                ))
                            }
                        }
                        _ => None,
                    })
                })
            });
            if cancelled {
                self.capturing_shortcut_for = None;
                self.status = "Rebind cancelled".to_string();
            } else if let Some(combo) = combo {
                match crate::actions::set_binding(
                    &self.conn,
                    crate::actions::Scope::User(self.current_user_id),
                    combo,
                    ActionRef::Builtin(action),
                ) {
                    Ok(None) => {
                        self.shortcut_map = crate::actions::load_shortcut_map(&self.conn, self.current_user_id);
                        self.status = format!("Bound {combo} to {}", action.label());
                    }
                    Ok(Some(conflict)) => {
                        self.status = format!("{combo} is already bound to {}", conflict.label(&self.custom_actions));
                    }
                    Err(_) => {}
                }
                self.capturing_shortcut_for = None;
            }
        }

        // Global shortcuts, driven by the rebindable shortcut map (disabled
        // while a modal dialog is open so typing in the name field doesn't
        // trigger them, or while capturing a new binding, or while a text
        // field has focus so typing doesn't trigger shortcuts).
        let text_focused = ctx.memory(|m| {
            m.focused().is_some_and(|id| {
                id == egui::Id::new(("address_bar", self.active_pane))
                    || id == egui::Id::new(("filter_input", self.active_pane))
            })
        });
        if self.dialog.is_none()
            && self.capturing_shortcut_for.is_none()
            && !text_focused
        {
            let triggered = ctx.input(|i| {
                self.shortcut_map
                    .iter()
                    .find(|(combo, _)| combo.matches_input(i))
                    .map(|(_, action)| *action)
                    // Ctrl+C/X/V arrive as clipboard events, not key events
                    // (see `clipboard_event_combo`) — resolve them through
                    // the same map so the defaults and any user rebinds of
                    // those combos keep working.
                    .or_else(|| {
                        clipboard_event_combo(i)
                            .and_then(|combo| self.shortcut_map.get(&combo).copied())
                    })
            });
            if let Some(action) = triggered {
                self.dispatch(&ctx, action);
            }
        }

        // Fresh hit-test geometry for the drag & drop pass below; the pane
        // bodies and tab strips refill these as they render this frame.
        self.dnd_pane_rects = [None, None];
        self.dnd_tab_rects.clear();

        for pane_idx in 0..2 {
            self.poll_listing(pane_idx, &ctx);
        }

        let tree_total_rect = ui.available_rect_before_wrap();
        let tree_divider_w = 6.0;
        let tree_min_w = 120.0;
        let tree_max_w = (tree_total_rect.width() - tree_divider_w - 200.0).max(tree_min_w);
        self.tree_width = self.tree_width.clamp(tree_min_w, tree_max_w);

        egui::Panel::left("folder_tree")
            .resizable(false)
            .exact_size(self.tree_width)
            .show(ui, |ui| {
            ui.heading("Folders");

            egui::ScrollArea::both()
                .id_salt("folder_tree_scroll")
                .auto_shrink(false)
                .show(ui, |ui| {
                let active_path = self.panes[self.active_pane].active_tab().path.clone();
                // Detect navigation: force-expand tree when active path changes
                let force_expand = self.prev_active_path.as_ref() != Some(&active_path);
                if force_expand {
                    self.prev_active_path = Some(active_path.clone());
                    // Keep centering for several passes while the newly
                    // expanded branches settle their layout.
                    self.tree_scroll_frames = 8;
                    // Same window for collapsing branches off the active path.
                    self.tree_collapse_frames = 8;
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
                            if self.try_navigate_active(self.active_pane, path.to_path_buf()) {
                                self.dirty = true;
                            }
                        }
                        // Right-click to remove from favourites
                        let fav_path_owned = fav_path.clone();
                        styled_context_menu(&btn, |ui| {
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
                let mut network_roots = self.network_servers.clone();
                if let Some(active_unc_root) = tree::unc_share_root(&active_path) {
                    let already_covered = network_roots.iter().any(|r| {
                        r.to_string_lossy().to_lowercase() == active_unc_root.to_string_lossy().to_lowercase()
                    });
                    if !already_covered {
                        network_roots.push(active_unc_root);
                    }
                }
                if !network_roots.is_empty() {
                    ui.separator();
                    ui.label(egui::RichText::new("Network").strong());
                    for server in &network_roots {
                        self.show_dir_node(ui, server, &active_path, force_expand);
                    }
                }
                self.tree_scroll_frames = self.tree_scroll_frames.saturating_sub(1);
                self.tree_collapse_frames = self.tree_collapse_frames.saturating_sub(1);
            });
        });

        {
            let divider_x = tree_total_rect.min.x + self.tree_width;
            let divider_rect = egui::Rect::from_min_size(
                egui::pos2(divider_x - tree_divider_w / 2.0, tree_total_rect.min.y),
                egui::vec2(tree_divider_w, tree_total_rect.height()),
            );
            let divider_resp = ui.interact(
                divider_rect,
                egui::Id::new("tree_divider"),
                egui::Sense::drag(),
            );
            if divider_resp.hovered() || divider_resp.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
            if divider_resp.dragged() {
                self.tree_width = (self.tree_width + divider_resp.drag_delta().x)
                    .clamp(tree_min_w, tree_max_w);
                self.dirty = true;
            }
        }

        egui::CentralPanel::default().show(ui, |ui| {
            ui.horizontal(|ui| {
                // The favourite button's label/hover flips on whether the
                // current folder is already favourited. Every other action
                // renders the same regardless of state.
                let current_path = self.active_tab_dir();
                let is_fav = crate::db::is_favourite(&self.conn, self.current_user_id, &current_path.display().to_string());

                let toolbar_actions = self.toolbar_actions.clone();
                let mut clicked: Option<ActionRef> = None;
                for action_ref in &toolbar_actions {
                    match action_ref {
                        // Custom actions get their own second row below the
                        // main toolbar; ignore any stale references in older
                        // saved layouts.
                        ActionRef::Custom(_) => {}
                        ActionRef::Builtin(action) => {
                            let (label, hover, enabled) = match action {
                                Action::Copy => ("📋 Copy", "Copy selection (Ctrl+C)", true),
                                Action::Cut => ("Cut", "Cut selection (Ctrl+X)", true),
                                Action::Paste => ("Paste", "Paste clipboard (Ctrl+V)", true),
                                Action::Delete => ("🗑 Delete", "Send selection to Recycle Bin", true),
                                Action::Rename => ("Rename", "Rename the selected item", true),
                                Action::CopyFilename => ("Copy Filename", "Copy full path of selected file", true),
                                Action::CopyFolderPath => ("Copy Folder Path", "Copy current folder path", true),
                                Action::ToggleFavourite => {
                                    if is_fav {
                                        ("★ Un favourite", "Toggle favourite for current folder", true)
                                    } else {
                                        ("☆ Add to Favourites", "Toggle favourite for current folder", true)
                                    }
                                }
                                Action::Find => ("🔍 Find", "Find files in current folder (Ctrl+F)", true),
                                Action::NewFolder => ("🗀 New Folder", "Create a new folder here", true),
                                Action::NewFile => ("🗋 New File", "Create a new file here", true),
                                Action::GoBack => ("⬅ Back", "Go back", true),
                                Action::GoForward => ("➡ Forward", "Go forward", true),
                                Action::GoUp => ("⬆ Up", "Go up one folder", true),
                                Action::NewTab => ("+ Tab", "Open a new tab", true),
                                Action::CloseTab => ("Close Tab", "Close the active tab", true),
                                Action::Refresh => ("🔄 Refresh", "Reload the current folder (F5)", true),
                                Action::ToggleSettings => ("⚙ Settings", "Preferences", true),
                                _ => continue,
                            };
                            ui.add_enabled_ui(enabled, |ui| {
                                if toolbar_button(ui, label.to_owned(), None)
                                    .on_hover_text(hover)
                                    .clicked()
                                {
                                    clicked = Some(*action_ref);
                                }
                            });
                        }
                    }
                }
                if let Some(action) = clicked {
                    self.dispatch(&ctx, action);
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button("❓ Help")
                        .on_hover_text("User Manual")
                        .clicked()
                    {
                        self.dialog = Some(Dialog::Help);
                    }
                    if ui
                        .button("⚙ Settings")
                        .on_hover_text("Preferences")
                        .clicked()
                    {
                        self.show_settings = true;
                    }

                    let current_name = self
                        .users
                        .iter()
                        .find(|u| u.id == self.current_user_id)
                        .map(|u| u.name.clone())
                        .unwrap_or_else(|| "User".to_string());
                    let mut switch_to: Option<i64> = None;
                    let mut open_new_user = false;
                    egui::ComboBox::from_id_salt("user_combo")
                        .selected_text(format!("👤 {current_name}"))
                        .show_ui(ui, |ui| {
                            for user in &self.users {
                                if ui
                                    .selectable_label(user.id == self.current_user_id, &user.name)
                                    .clicked()
                                {
                                    switch_to = Some(user.id);
                                }
                            }
                            ui.separator();
                            if ui.button("New User…").clicked() {
                                open_new_user = true;
                            }
                        });
                    if let Some(id) = switch_to {
                        self.switch_user(id);
                    }
                    if open_new_user {
                        self.dialog = Some(Dialog::NewUser { name: String::new() });
                    }
                });
            });

            // Second toolbar line: one button per user-defined custom
            // "open with" action, launching the chosen application with the
            // selected file as its argument. Each button shows the icon
            // extracted from the target executable.
            if !self.custom_actions.is_empty() {
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    let mut launch: Option<i64> = None;
                    for custom in &self.custom_actions {
                        if !self.custom_icons.contains_key(&custom.exe_path) {
                            let tex =
                                crate::icon_cache::load_icon_texture(&ctx.clone(), &custom.exe_path);
                            self.custom_icons.insert(custom.exe_path.clone(), tex);
                        }
                        let icon = self.custom_icons.get(&custom.exe_path).cloned().flatten();
                        if toolbar_button(ui, custom.label.clone(), icon.as_ref())
                            .on_hover_text(format!(
                                "Open the selection with {}",
                                custom.exe_path
                            ))
                            .clicked()
                        {
                            launch = Some(custom.id);
                        }
                    }
                    if let Some(id) = launch {
                        self.dispatch(&ctx, ActionRef::Custom(id));
                    }
                });
            }

            // Office-style settings dialog (nav rail + content pages).
            self.show_settings_window(&ctx);

            // Handle tab context menu separately (non-modal, not a text-dialog).
            if matches!(&self.dialog, Some(Dialog::TabContext { .. })) {
                self.show_tab_context_menu(&ctx);
            }

            // Progress modal for background operations
            let mut dismiss_op = false;
            if let Some(ref mut op) = self.background_op {
                let still_running = op.poll();
                let title = if still_running {
                    "Processing..."
                } else {
                    match &op.status {
                        OpStatus::Completed(_) => "Done",
                        OpStatus::Failed(_) => "Error",
                        _ => "Done",
                    }
                };
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
                    .auto_sized()
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(&ctx, |ui| {
                        ui.set_width(360.0);
                        ui.label(&progress_text);
                        ui.add(egui::ProgressBar::new(fraction).animate(still_running));
                        if !still_running {
                            match &op.status {
                                OpStatus::Completed(msg) => {
                                    self.status = msg.clone();
                                }
                                OpStatus::Failed(msg) => {
                                    ui.add_space(6.0);
                                    ui.label(egui::RichText::new("Error:").strong());
                                    egui::ScrollArea::vertical()
                                        .id_salt("error_scroll")
                                        .max_height(120.0)
                                        .show(ui, |ui| {
                                            ui.add(
                                                egui::Label::new(
                                                    egui::RichText::new(msg.as_str())
                                                        .monospace(),
                                                )
                                                .selectable(true),
                                            );
                                        });
                                }
                                _ => {}
                            }
                            ui.add_space(4.0);
                            ui.allocate_ui_with_layout(
                                egui::vec2(ui.available_width(), 24.0),
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button("Close").clicked() {
                                        dismiss_op = true;
                                    }
                                },
                            );
                        }
                    });

                if dismiss_op {
                    self.background_op = None;
                    self.dirty = true;
                    for dir in std::mem::take(&mut self.background_op_dirs) {
                        self.mark_dir_dirty(&dir);
                    }
                }
            }

            // Modal dialogs (rename / new folder / new file / duplicate name / find).
            if !matches!(&self.dialog, Some(Dialog::TabContext { .. })) {
                // Handle Find dialog separately (it has its own UI)
                let mut find_close = false;
                let mut find_row_action: Option<FindRowAction> = None;
                let mut find_trigger: Option<String> = None;
                if let Some(Dialog::Find {
                    query,
                    results,
                    search_path,
                    sort_col,
                    sort_asc,
                    name_filter,
                    folder_filter,
                    include_folders,
                }) = &mut self.dialog
                {
                    let search_path_clone = search_path.clone();
                    let searching = self.find_job.is_some();
                    // Show the dialog UI, capturing any actions needed
                    let mut dialog_ui = |ui: &mut egui::Ui,
                                         query: &mut String,
                                         results: &mut Vec<crate::fs_entry::FsEntry>,
                                         sort_col: &mut String,
                                         sort_asc: &mut bool,
                                         name_filter: &mut String,
                                         folder_filter: &mut String,
                                         include_folders: &mut bool| {
                        ui.horizontal(|ui| {
                            ui.label("Search in:");
                            ui.label(search_path_clone.display().to_string());
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button("✕ Close").clicked() {
                                    find_close = true;
                                }
                            });
                        });
                        ui.horizontal(|ui| {
                            ui.label("Find:");
                            let edit = ui.text_edit_singleline(query);
                            let enter_pressed =
                                edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            let search_clicked = ui.button("Search").clicked();
                            if (enter_pressed || search_clicked) && !query.is_empty() {
                                find_trigger = Some(query.clone());
                            }
                        });

                        // Apply the current sort before rendering (cheap even
                        // for large result sets, and keeps headers in sync).
                        {
                            let col = sort_col.clone();
                            let asc = *sort_asc;
                            match col.as_str() {
                                "folder" => results.sort_by(|a, b| {
                                    let fa = a.path.parent().map(|p| p.to_string_lossy().to_lowercase()).unwrap_or_default();
                                    let fb = b.path.parent().map(|p| p.to_string_lossy().to_lowercase()).unwrap_or_default();
                                    let ord = fa.cmp(&fb).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
                                    if asc { ord } else { ord.reverse() }
                                }),
                                other => crate::fs_entry::sort_entries(results, other, asc),
                            }
                        }

                        // Narrow the displayed list without discarding hits:
                        // case-insensitive substring filters on the entry name
                        // and its parent folder, plus the folder toggle.
                        let nf = name_filter.to_lowercase();
                        let ff = folder_filter.to_lowercase();
                        let view: Vec<&crate::fs_entry::FsEntry> = results
                            .iter()
                            .filter(|e| *include_folders || !e.is_dir)
                            .filter(|e| nf.is_empty() || e.name.to_lowercase().contains(&nf))
                            .filter(|e| {
                                ff.is_empty()
                                    || e.path
                                        .parent()
                                        .map(|p| p.to_string_lossy().to_lowercase().contains(&ff))
                                        .unwrap_or(false)
                            })
                            .collect();

                        let filters_active =
                            !nf.is_empty() || !ff.is_empty() || !*include_folders;
                        if query.is_empty() && results.is_empty() && !searching {
                            ui.label(egui::RichText::new("Type a file or folder name to search for.").weak());
                        } else if searching {
                            ui.horizontal(|ui| {
                                ui.add(egui::Spinner::new());
                                ui.label(format!("Searching… {} found so far", results.len()));
                            });
                        } else if filters_active {
                            ui.label(format!("Showing {} of {} result(s)", view.len(), results.len()));
                        } else {
                            ui.label(format!("{} result(s) found", results.len()));
                        }

                        // Filter row above the table.
                        ui.horizontal(|ui| {
                            ui.label("Name:");
                            ui.add(
                                egui::TextEdit::singleline(name_filter)
                                    .desired_width(130.0)
                                    .hint_text("filter names"),
                            );
                            ui.label("Folder:");
                            ui.add(
                                egui::TextEdit::singleline(folder_filter)
                                    .desired_width(150.0)
                                    .hint_text("filter paths"),
                            );
                            ui.checkbox(include_folders, "Include folders");
                        });
                        if filters_active && view.is_empty() && !results.is_empty() {
                            ui.label(egui::RichText::new("No entries match the current filters.").weak());
                        }
                        if !results.is_empty() {
                            ui.label(
                                egui::RichText::new("Right-click an entry for actions; double-click to open it.")
                                    .weak()
                                    .small(),
                            );
                        }

                        let mut sort_clicked: Option<String> = None;
                        egui_extras::TableBuilder::new(ui)
                            .id_salt("find_results_table")
                            .striped(true)
                            .resizable(true)
                            .sense(egui::Sense::click())
                            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                            .vscroll(true)
                            .max_scroll_height(320.0)
                            .column(egui_extras::Column::initial(170.0).resizable(true).clip(true))
                            .column(egui_extras::Column::remainder().clip(true))
                            .column(egui_extras::Column::initial(110.0).resizable(true).clip(true))
                            .column(egui_extras::Column::initial(70.0).resizable(true).clip(true))
                            .header(22.0, |mut header| {
                                header.col(|ui| {
                                    sort_header(ui, "Name", "name", sort_col, *sort_asc, &mut sort_clicked);
                                });
                                header.col(|ui| {
                                    sort_header(ui, "Folder", "folder", sort_col, *sort_asc, &mut sort_clicked);
                                });
                                header.col(|ui| {
                                    sort_header(ui, "Modified", "modified", sort_col, *sort_asc, &mut sort_clicked);
                                });
                                header.col(|ui| {
                                    sort_header(ui, "Size", "size", sort_col, *sort_asc, &mut sort_clicked);
                                });
                            })
                            .body(|body| {
                                body.rows(20.0, view.len(), |mut row| {
                                    let entry = &view[row.index()];
                                    row.col(|ui| {
                                        let label = if entry.is_dir {
                                            format!("\u{1F4C1} {}", entry.name)
                                        } else {
                                            entry.name.clone()
                                        };
                                        ui.add(egui::Label::new(&label).selectable(false));
                                    });
                                    row.col(|ui| {
                                        let folder = entry
                                            .path
                                            .parent()
                                            .map(|p| p.display().to_string())
                                            .unwrap_or_default();
                                        ui.label(folder);
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
                                            format_file_size(entry.size)
                                        };
                                        ui.label(size_text);
                                    });
                                    // Double-click opens the item directly;
                                    // right-click offers all actions without
                                    // closing the dialog.
                                    if row.response().double_clicked() {
                                        find_row_action = Some(FindRowAction::Open(entry.path.clone()));
                                    }
                                    styled_context_menu(&row.response(), |ui| {
                                        if ui.button("Open").clicked() {
                                            find_row_action = Some(FindRowAction::Open(entry.path.clone()));
                                            ui.close();
                                        }
                                        if ui.button("Open Containing Folder").clicked() {
                                            if let Some(parent) = entry.path.parent() {
                                                find_row_action = Some(FindRowAction::Reveal(parent.to_path_buf()));
                                            }
                                            ui.close();
                                        }
                                        ui.separator();
                                        if ui.button("Copy Full Path").clicked() {
                                            find_row_action = Some(FindRowAction::CopyPath(
                                                entry.path.display().to_string(),
                                            ));
                                            ui.close();
                                        }
                                        if ui.button("Copy Filename").clicked() {
                                            find_row_action = Some(FindRowAction::CopyName(entry.name.clone()));
                                            ui.close();
                                        }
                                    });
                                });
                            });
                        if let Some(col) = sort_clicked {
                            if *sort_col == col {
                                *sort_asc = !*sort_asc;
                            } else {
                                *sort_col = col;
                                *sort_asc = true;
                            }
                        }
                    };
                    egui::Window::new("Find Files")
                        .resizable(true)
                        .default_width(560.0)
                        .default_height(420.0)
                        .show(&ctx, |ui| {
                            dialog_ui(
                                ui,
                                query,
                                results,
                                sort_col,
                                sort_asc,
                                name_filter,
                                folder_filter,
                                include_folders,
                            );
                        });
                }
                if find_close {
                    self.dialog = None;
                    self.find_job = None;
                }
                if let Some(action) = find_row_action {
                    match action {
                        FindRowAction::Open(path) => {
                            // `start` uses the shell, so files open with their
                            // default app and folders open in Explorer.
                            let _ = std::process::Command::new("cmd")
                                .args(["/C", "start", "", &path.to_string_lossy()])
                                .spawn();
                        }
                        FindRowAction::Reveal(parent) => {
                            if self.try_navigate_active(self.active_pane, parent) {
                                self.dirty = true;
                            }
                        }
                        FindRowAction::CopyPath(text) => {
                            Self::set_clipboard_text(&ctx, &text);
                            self.status = "Copied path to clipboard".into();
                        }
                        FindRowAction::CopyName(name) => {
                            Self::set_clipboard_text(&ctx, &name);
                            self.status = "Copied filename to clipboard".into();
                        }
                    }
                }
                if let Some(query) = find_trigger {
                    let mut search_path = None;
                    if let Some(Dialog::Find { search_path: sp, results, .. }) = &mut self.dialog {
                        // A fresh search replaces the previous one's results.
                        results.clear();
                        search_path = Some(sp.clone());
                    }
                    if let Some(sp) = search_path {
                        self.start_find_search(sp, query);
                    }
                }
                // Poll the background search job. Matches stream in one at a
                // time, so drain everything currently queued into the dialog's
                // results and keep the job alive until the channel disconnects
                // (the walk finished). Repaint while waiting so hits appear
                // live.
                if let Some(rx) = &self.find_job {
                    let mut finished = false;
                    let mut hits: Vec<crate::fs_entry::FsEntry> = Vec::new();
                    loop {
                        match rx.try_recv() {
                            Ok(entry) => hits.push(entry),
                            Err(mpsc::TryRecvError::Empty) => break,
                            Err(mpsc::TryRecvError::Disconnected) => {
                                finished = true;
                                break;
                            }
                        }
                    }
                    if !hits.is_empty() || finished {
                        if let Some(Dialog::Find { results, .. }) = &mut self.dialog {
                            results.extend(hits);
                        }
                    }
                    if finished {
                        self.find_job = None;
                    } else {
                        ctx.request_repaint();
                    }
                }
                let is_help = matches!(&self.dialog, Some(Dialog::Help));
                let is_confirm_delete = matches!(&self.dialog, Some(Dialog::ConfirmDelete { .. }));
                if is_help {
                    let mut close = false;
                    // Handle Esc key to close.
                    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                        close = true;
                    }
                    let avail = ctx.input(|i| i.viewport_rect());
                    let size = egui::vec2(
                        (avail.width() * 0.65).clamp(420.0, 720.0),
                        (avail.height() * 0.78).clamp(360.0, 680.0),
                    );
                    egui::Window::new("Help")
                        .id(egui::Id::new("help_window"))
                        .title_bar(true)
                        .resizable(true)
                        .collapsible(false)
                        .fixed_size(size)
                        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                        .show(&ctx, |ui| {
                            egui::ScrollArea::vertical()
                                .id_salt("help_scroll")
                                .auto_shrink(false)
                                .show(ui, |ui| {
                                    help_content(ui);
                                });
                            ui.separator();
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button("Close").clicked() {
                                    close = true;
                                }
                            });
                        });
                    if close {
                        self.dialog = None;
                    }
                }
                if is_confirm_delete {
                    let mut close = false;
                    let mut confirm = false;
                    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                        close = true;
                    }
                    if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                        confirm = true;
                    }
                    let count = if let Some(Dialog::ConfirmDelete { paths }) = &self.dialog {
                        paths.len()
                    } else {
                        0
                    };
                    egui::Window::new("Confirm Delete")
                        .id(egui::Id::new("confirm_delete_window"))
                        .title_bar(true)
                        .resizable(false)
                        .collapsible(false)
                        .fixed_size(egui::vec2(380.0, 0.0))
                        // Modal-style placement: pinned to the screen centre
                        // rather than egui's cascading default position.
                        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                        .show(&ctx, |ui| {
                            ui.label(format!(
                                "Are you sure you want to delete {count} item(s)?"
                            ));
                            ui.add_space(8.0);
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button("Cancel").clicked() {
                                    close = true;
                                }
                                let delete_resp = ui.button("Delete");
                                if delete_resp.clicked() {
                                    confirm = true;
                                }
                                // Default focus on the affirmative button so
                                // Enter/Space confirm immediately (Esc still
                                // cancels). Seed it only while nothing holds
                                // focus, so Tab away stays respected.
                                if ctx.memory(|m| m.focused().is_none()) {
                                    delete_resp.request_focus();
                                }
                            });
                        });
                    if confirm {
                        if let Some(Dialog::ConfirmDelete { paths }) = self.dialog.take() {
                            self.panes[self.active_pane]
                                .active_tab_mut()
                                .clear_selection();
                            self.status = format!("Deleting {} item(s)…", paths.len());
                            self.background_op_dirs = vec![self.active_tab_dir()];
                            self.background_op =
                                Some(progress::delete_to_trash_bg(paths));
                        }
                    } else if close {
                        self.dialog = None;
                    }
                }
                let is_find = matches!(&self.dialog, Some(Dialog::Find { .. }));
                if self.dialog.is_some() && !find_close && !is_find && !is_help && !is_confirm_delete {
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
                        let multiline = matches!(dialog, Dialog::NewFolder { .. });
                        let (title, name) = match dialog {
                            Dialog::Rename { name, .. } => ("Rename", name),
                            Dialog::NewFolder { name } => ("New Folder", name),
                            Dialog::NewFile { name } => ("New File", name),
                            Dialog::DuplicateName { suggested, .. } => {
                                ("Duplicate Name", suggested)
                            }
                            Dialog::NewUser { name } => ("New User", name),
                            Dialog::RenameTab { name, .. } => ("Rename Tab", name),
                            Dialog::Find { .. } | Dialog::TabContext { .. } | Dialog::Help
                            | Dialog::ConfirmDelete { .. } => unreachable!(),
                        };
                        egui::Window::new(title)
                            // Modal-style placement: pinned to screen centre.
                            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                            .show(&ctx, |ui| {
                                if let Some(ref label) = src_label {
                                    ui.label(label.as_str());
                                }
                                if multiline {
                                    ui.label("One folder per line:");
                                }
                                let edit = if multiline {
                                    ui.add(
                                        egui::TextEdit::multiline(name)
                                            .desired_rows(4)
                                            .desired_width(260.0),
                                    )
                                } else {
                                    ui.text_edit_singleline(name)
                                };
                                // Default keyboard focus goes to the input
                                // box — but seed it ONLY while nothing else
                                // holds focus (i.e. on open). Re-requesting
                                // every frame would fight egui's own Tab
                                // navigation and focus could never reach
                                // the OK/Cancel buttons.
                                if ctx.memory(|m| m.focused().is_none()) {
                                    edit.request_focus();
                                }
                                // Enter submits single-line dialogs; a
                                // multiline folder-name box needs Enter to
                                // insert newlines, so it only submits via OK.
                                commit = !multiline
                                    && edit.lost_focus()
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

            {
                let total_rect = ui.available_rect_before_wrap();
                let divider_w = 6.0;
                let left_w = ((total_rect.width() - divider_w) * self.split_ratio).max(0.0);
                let left_rect = egui::Rect::from_min_size(
                    total_rect.min,
                    egui::vec2(left_w, total_rect.height()),
                );
                let divider_rect = egui::Rect::from_min_size(
                    egui::pos2(left_rect.max.x, total_rect.min.y),
                    egui::vec2(divider_w, total_rect.height()),
                );
                let right_rect = egui::Rect::from_min_size(
                    egui::pos2(divider_rect.max.x, total_rect.min.y),
                    egui::vec2(
                        (total_rect.width() - left_w - divider_w).max(0.0),
                        total_rect.height(),
                    ),
                );
                let pane_rects = [left_rect, right_rect];

                let divider_resp = ui.interact(
                    divider_rect,
                    egui::Id::new("pane_divider"),
                    egui::Sense::drag(),
                );
                let divider_color = if divider_resp.dragged() || divider_resp.hovered() {
                    ui.visuals().widgets.active.bg_fill
                } else {
                    ui.visuals().widgets.inactive.bg_fill
                };
                ui.painter().rect_filled(divider_rect, 0.0, divider_color);

                // Draw drag handle grip dots
                let grip_color = ui.visuals().widgets.noninteractive.fg_stroke.color;
                let grip_radius = 1.5;
                let grip_spacing = 6.0;
                let grip_count = 5;
                let center_x = divider_rect.center().x;
                let center_y = divider_rect.center().y;
                let total_height = grip_count as f32 * grip_spacing;
                let start_y = center_y - total_height / 2.0;
                for i in 0..grip_count {
                    let y = start_y + i as f32 * grip_spacing + grip_spacing / 2.0;
                    ui.painter().circle_filled(
                        egui::pos2(center_x, y),
                        grip_radius,
                        grip_color,
                    );
                }

                // Change cursor to resize when hovering
                if divider_resp.hovered() || divider_resp.dragged() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                }
                if divider_resp.dragged() && total_rect.width() > 0.0 {
                    let delta = divider_resp.drag_delta().x;
                    self.split_ratio =
                        (self.split_ratio + delta / total_rect.width()).clamp(0.15, 0.85);
                    self.dirty = true;
                }

                for pane_idx in 0..2 {
                    let is_active = pane_idx == self.active_pane;
                    let pane_rect = pane_rects[pane_idx];
                    ui.scope_builder(egui::UiBuilder::new().max_rect(pane_rect), |ui| {
                        ui.group(|ui| {
                            self.show_pane_body(ui, &ctx, pane_idx, is_active);
                        });
                    });
                }

                ui.allocate_rect(total_rect, egui::Sense::hover());
            }
        });

        // In-flight file drag & drop: tab-opening, drop-target feedback and
        // the actual copy/move on release. Runs after the panes have laid
        // out so this frame's rects are current.
        self.process_file_drag_drop(&ctx);
        self.process_external_file_drop(&ctx, frame);

        // Toast notifications: whenever the status message changes, surface
        // it as a transient banner near the top of the window for ~3 seconds
        // instead of a permanent line under the toolbar.
        if self.status != self.last_status {
            self.last_status = self.status.clone();
            if !self.status.is_empty() {
                self.toast = Some((self.status.clone(), std::time::Instant::now()));
            }
        }
        if let Some((msg, shown_at)) = &self.toast {
            const TOAST_SECS: u64 = 3;
            let elapsed = shown_at.elapsed();
            if elapsed >= std::time::Duration::from_secs(TOAST_SECS) {
                self.toast = None;
            } else {
                let dark = ui.visuals().dark_mode;
                let (fill, stroke, text) = if dark {
                    (
                        egui::Color32::from_rgba_premultiplied(180, 50, 50, 242),
                        egui::Color32::from_rgb(255, 120, 120),
                        egui::Color32::from_rgb(255, 220, 220),
                    )
                } else {
                    (
                        egui::Color32::from_rgba_premultiplied(255, 220, 220, 248),
                        egui::Color32::from_rgb(200, 60, 60),
                        egui::Color32::from_rgb(120, 0, 0),
                    )
                };
                let font =
                    egui::FontId::proportional(self.font_size);
                let painter = ctx.layer_painter(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("status_toast"),
                ));
                let galley = painter.layout_no_wrap(msg.clone(), font, text);
                let pad = 12.0;
                let size = galley.size() + egui::vec2(pad * 2.0, pad * 1.2);
                let screen = ctx.input(|i| i.viewport_rect());
                let pos = egui::pos2(
                    (screen.center().x - size.x / 2.0).max(screen.left() + 8.0),
                    screen.top() + 14.0,
                );
                painter.rect_filled(
                    egui::Rect::from_min_size(pos, size),
                    6.0,
                    fill,
                );
                painter.rect_stroke(
                    egui::Rect::from_min_size(pos, size),
                    6.0,
                    egui::Stroke::new(1.0, stroke),
                    egui::StrokeKind::Outside,
                );
                painter.galley(
                    egui::pos2(pos.x + pad, pos.y + size.y / 2.0 - galley.size().y / 2.0),
                    galley,
                    text,
                );
                // Wake up exactly when it should disappear.
                ctx.request_repaint_after(
                    std::time::Duration::from_secs(TOAST_SECS) - elapsed,
                );
            }
        }

        // Rotating tips card pinned to the bottom-left corner. "Turn off"
        // persists the disable; ✕ only hides until the next launch.
        if self.tips_enabled {
            match self.tips.draw(&ctx, self.font_size) {
                crate::tips::TipAction::Disable => {
                    self.tips_enabled = false;
                    let _ = crate::config::set(
                        &self.conn,
                        crate::config::Scope::User(self.current_user_id),
                        crate::tips::KEY_TIPS_ENABLED,
                        "false",
                    );
                }
                _ => {}
            }
        }

        if self.dirty {
            self.persist();
            self.dirty = false;
        }
    }
}

/// Large title + one-line description that opens a settings page, followed
/// by a hairline separator — mirrors the Office options-page header.
fn settings_header(ui: &mut egui::Ui, title: &str, desc: &str) {
    ui.add_space(2.0);
    ui.label(egui::RichText::new(title).strong().size(17.0));
    ui.label(egui::RichText::new(desc).weak().small());
    ui.add_space(8.0);
    ui.separator();
    ui.add_space(6.0);
}

/// Small bold group caption inside a settings page.
fn settings_group_label(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).strong());
}

/// Draws a settings-nav icon with plain epaint shapes so it never depends on
/// emoji/font coverage. All coordinates are fractions of the icon rect.
fn paint_nav_icon(
    painter: &egui::Painter,
    r: egui::Rect,
    page: SettingsPage,
    color: egui::Color32,
) {
    let stroke = egui::Stroke::new(1.4, color);
    let c = r.center();
    let rad = r.width() * 0.42;
    let p = |fx: f32, fy: f32| egui::pos2(r.left() + r.width() * fx, r.top() + r.height() * fy);
    match page {
        SettingsPage::Appearance => {
            // Half-filled circle: light/dark theme toggle.
            painter.circle_stroke(c, rad, stroke);
            // Right half as a filled fan (no circle_sector helper here).
            let fill_r = rad - 1.2;
            let steps = 20;
            let mut pts = vec![c];
            for k in 0..=steps {
                let a = -std::f32::consts::FRAC_PI_2
                    + std::f32::consts::PI * (k as f32 / steps as f32);
                pts.push(c + fill_r * egui::vec2(a.cos(), a.sin()));
            }
            painter.add(egui::Shape::convex_polygon(
                pts,
                color,
                egui::Stroke::NONE,
            ));
        }
        SettingsPage::Shortcuts => {
            // Keyboard: outlined body, space bar, two keys.
            painter.rect_stroke(
                r.shrink2(egui::vec2(0.5, 3.5)),
                3.0,
                stroke,
                egui::StrokeKind::Inside,
            );
            painter.rect_filled(
                egui::Rect::from_min_size(p(0.22, 0.62), egui::vec2(r.width() * 0.56, 2.4)),
                1.0,
                color,
            );
            painter.circle_filled(p(0.33, 0.38), 1.4, color);
            painter.circle_filled(p(0.52, 0.38), 1.4, color);
            painter.circle_filled(p(0.71, 0.38), 1.4, color);
        }
        SettingsPage::Toolbar => {
            // Three descending bars.
            for (i, w) in [0.86f32, 0.62, 0.40].into_iter().enumerate() {
                let y = r.top() + 3.0 + i as f32 * 4.6;
                painter.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(r.left() + 2.0, y),
                        egui::vec2((r.width() - 4.0) * w, 2.6),
                    ),
                    1.2,
                    color,
                );
            }
        }
        SettingsPage::CustomActions => {
            // Lightning bolt zigzag.
            let pts = [
                p(0.66, 0.02),
                p(0.30, 0.55),
                p(0.54, 0.55),
                p(0.36, 0.98),
                p(0.74, 0.42),
                p(0.50, 0.42),
                p(0.72, 0.02),
            ];
            painter.add(egui::Shape::line(pts.to_vec(), stroke));
        }
        SettingsPage::ViewMode => {
            // Three horizontal lines (list layout icon).
            for (i, w) in [1.0f32, 0.75, 0.5].into_iter().enumerate() {
                let y = r.top() + 3.0 + i as f32 * 4.6;
                painter.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(r.left() + 2.0, y),
                        egui::vec2((r.width() - 4.0) * w, 2.6),
                    ),
                    1.2,
                    color,
                );
            }
        }
        SettingsPage::Advanced => {
            // Gear: ring + teeth stubs + hub.
            painter.circle_stroke(c, rad - 2.2, stroke);
            for k in 0..8u32 {
                let a = k as f32 * std::f32::consts::TAU / 8.0;
                let dx = a.cos();
                let dy = a.sin();
                painter.line_segment(
                    [
                        c + rad * 0.62 * egui::vec2(dx, dy),
                        c + (rad - 0.6) * egui::vec2(dx, dy),
                    ],
                    stroke,
                );
            }
            painter.circle_filled(c, 2.2, color);
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
    OpenInExplorer(PathBuf),
}

/// Deferred action requested from a Find-results row. Applied after the
/// dialog borrow ends, same pattern as `RowAction` in the file listing.
enum FindRowAction {
    /// Launch the item itself with its system default handler.
    Open(PathBuf),
    /// Navigate the active pane to the item's containing folder.
    Reveal(PathBuf),
    CopyPath(String),
    CopyName(String),
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

/// Click events gathered from one tab-strip item.
struct TabItemEvents {
    clicked: bool,
    secondary_clicked: bool,
    close_clicked: bool,
    /// Pointer position of the secondary click, for anchoring the menu.
    secondary_pos: Option<egui::Pos2>,
    /// The tab header's on-screen rect — reused by the drag & drop pass to
    /// detect "dragged item hovers this tab".
    rect: egui::Rect,
}

/// What a tab strip's rendering reported back to the pane: which tab was
/// activated / right-clicked / closed, whether "+" was pressed, and — in
/// vertical (sidebar) mode — the rect left over for the pane content.
struct TabStripResult {
    clicked: Option<usize>,
    closed: Option<usize>,
    opened: bool,
    context_menu: Option<usize>,
    content_rect: Option<egui::Rect>,
    menu_pos: Option<egui::Pos2>,
}

/// Paints `label` inside `rect`, word-wrapping to the available width and
/// clipping after two lines so long names can never spill out of a tab row.
fn paint_wrapped_label(
    painter: &egui::Painter,
    rect: egui::Rect,
    label: &str,
    font: egui::FontId,
    color: egui::Color32,
) {
    const PAD_L: f32 = 6.0;
    const PAD_R: f32 = 20.0; // keep clear of the hover "×" button
    const PAD_Y: f32 = 2.0;
    let text_rect = egui::Rect::from_min_max(
        egui::pos2(rect.left() + PAD_L, rect.top() + PAD_Y),
        egui::pos2(rect.right() - PAD_R, rect.bottom() - PAD_Y),
    );
    if text_rect.width() <= 8.0 || text_rect.height() <= 8.0 {
        return;
    }
    let galley = painter.layout(label.to_owned(), font, color, text_rect.width());
    // Vertically center whatever fit (one or two lines); the clip rect hides
    // anything beyond the two-line budget.
    let pos = egui::pos2(text_rect.left(), text_rect.center().y - galley.size().y / 2.0);
    painter.with_clip_rect(text_rect).galley(pos, galley, color);
}

/// Draws one tab in a pane's tab strip (either inline in the horizontal row
/// or stretched to the sidebar width), including the orange active-tab
/// highlight and the hover "×" close button. Pure widget code — takes no
/// `self`, so it can run while the pane list is mutably borrowed.
fn tab_strip_item(
    ui: &mut egui::Ui,
    label: &str,
    tab_pos: (usize, usize),
    is_tab_active: bool,
    is_active_pane: bool,
    locked: bool,
    tab_hover: &mut Option<(usize, usize)>,
    size: Option<egui::Vec2>,
) -> TabItemEvents {
    // Vertical sidebar rows are fully custom surfaces sized to exactly the
    // strip width — a long label can never stretch the row into the list
    // area, because the interactive rect is fixed before painting and the
    // text is wrapped/clipped afterwards.
    let tab_resp = match size {
        Some(row_size) => {
            let row_rect = egui::Rect::from_min_size(ui.cursor().min, row_size);
            let resp = ui.interact(
                row_rect,
                egui::Id::new(("vtab_row", tab_pos.0, tab_pos.1)),
                egui::Sense::click(),
            );
            ui.advance_cursor_after_rect(row_rect);
            resp
        }
        None => ui.selectable_label(is_tab_active, label),
    };
    let rect = tab_resp.rect;
    // Explorer/Windows-11 tab treatment: the active tab is a raised card
    // (lighter surface, top-rounded, connected to the content below) with a
    // thin orange accent strip on the active pane; inactive tabs are a faint
    // wash so they read as tabs without heavy boxes.
    if is_tab_active {
        let fill = if is_active_pane {
            ui.visuals().window_fill()
        } else {
            ui.visuals().extreme_bg_color
        };
        ui.painter().rect_filled(
            rect,
            egui::CornerRadius {
                nw: 6,
                ne: 6,
                sw: 0,
                se: 0,
            },
            fill,
        );
        ui.painter().rect_stroke(
            rect,
            egui::CornerRadius {
                nw: 6,
                ne: 6,
                sw: 0,
                se: 0,
            },
            ui.visuals().widgets.noninteractive.bg_stroke,
            egui::StrokeKind::Inside,
        );
        if is_active_pane {
            // Brand accent strip along the card's top edge.
            ui.painter().rect_filled(
                egui::Rect::from_min_max(rect.left_top(), egui::pos2(rect.max.x, rect.top() + 2.0)),
                0.0,
                egui::Color32::from_rgb(255, 165, 0),
            );
        }
        // The opaque card covers the label, so redraw it (left-aligned,
        // wrapping to at most two lines).
        paint_wrapped_label(
            ui.painter(),
            rect,
            label,
            egui::FontId::proportional(
                ui.style()
                    .text_styles
                    .get(&egui::TextStyle::Body)
                    .map_or(14.0, |f| f.size),
            ),
            ui.visuals().strong_text_color(),
        );
    } else {
        // Inactive tabs sit in a solid grey container. The fill is opaque,
        // so the label is repainted on top at full strength (black in light
        // mode, near-white in dark) instead of relying on the washed-out
        // widget text underneath.
        let dark = ui.visuals().dark_mode;
        let (fill, hovered_fill, text) = if dark {
            (
                egui::Color32::from_rgb(56, 56, 56),
                egui::Color32::from_rgb(68, 68, 68),
                egui::Color32::from_rgb(240, 240, 240),
            )
        } else {
            (
                egui::Color32::from_rgb(225, 225, 225),
                egui::Color32::from_rgb(213, 213, 213),
                egui::Color32::BLACK,
            )
        };
        ui.painter().rect_filled(
            rect,
            egui::CornerRadius {
                nw: 6,
                ne: 6,
                sw: 0,
                se: 0,
            },
            if tab_resp.contains_pointer() { hovered_fill } else { fill },
        );
        paint_wrapped_label(
            ui.painter(),
            rect,
            label,
            egui::FontId::proportional(
                ui.style()
                    .text_styles
                    .get(&egui::TextStyle::Body)
                    .map_or(14.0, |f| f.size),
            ),
            text,
        );
    }
    if tab_resp.contains_pointer() {
        *tab_hover = Some(tab_pos);
    }
    // Pinned tabs wear a small gold padlock in the top-right corner.
    if locked {
        let gold = egui::Color32::from_rgb(255, 193, 7);
        let cx = rect.right() - 8.0;
        let cy = rect.top() + 6.5;
        ui.painter()
            .circle_stroke(egui::pos2(cx, cy), 2.4, egui::Stroke::new(1.4, gold));
        ui.painter().rect_filled(
            egui::Rect::from_center_size(
                egui::pos2(cx, cy + 3.6),
                egui::vec2(6.2, 4.6),
            ),
            1.0,
            gold,
        );
    }
    let hovered = *tab_hover == Some(tab_pos);
    let mut close_clicked = false;
    // Show × close button on the tab's trailing edge when hovered
    if hovered {
        let rect = tab_resp.rect;
        let btn_size = 14.0;
        let btn_rect = egui::Rect::from_min_size(
            egui::pos2(rect.max.x - btn_size - 2.0, rect.center().y - btn_size / 2.0),
            egui::vec2(btn_size, btn_size),
        );
        let btn_resp = ui.interact(
            btn_rect,
            egui::Id::new(("tab_close", tab_pos.0, tab_pos.1)),
            egui::Sense::click(),
        );
        // Quiet "×" that gains a soft red disc only while pointed at,
        // matching the low-chrome look of Explorer's tab close buttons.
        if btn_resp.hovered() {
            ui.painter()
                .circle_filled(btn_rect.center(), btn_size / 2.0, egui::Color32::from_rgb(196, 43, 28));
            ui.painter().text(
                btn_rect.center(),
                egui::Align2::CENTER_CENTER,
                "×",
                egui::FontId::proportional(btn_size - 4.0),
                egui::Color32::WHITE,
            );
        } else {
            ui.painter().text(
                btn_rect.center(),
                egui::Align2::CENTER_CENTER,
                "×",
                egui::FontId::proportional(btn_size - 2.0),
                ui.visuals().widgets.noninteractive.fg_stroke.color,
            );
        }
        if btn_resp.clicked() {
            close_clicked = true;
        }
    }
    TabItemEvents {
        clicked: tab_resp.clicked(),
        secondary_clicked: tab_resp.secondary_clicked(),
        close_clicked,
        secondary_pos: if tab_resp.secondary_clicked() {
            tab_resp.interact_pointer_pos()
        } else {
            None
        },
        rect: tab_resp.rect,
    }
}

/// Background fill for right-click context menus, deliberately distinct from
/// the page background (`panel_fill`, which egui otherwise matches exactly)
/// so the floating menu reads as its own surface in both themes.
fn context_menu_fill(theme: egui::Theme) -> egui::Color32 {
    match theme {
        // Page: gray(27) — a blue-tinted dark slate stands out clearly.
        egui::Theme::Dark => egui::Color32::from_rgb(34, 43, 60),
        // Page: gray(248) — a blue-tinted light gray stands out clearly.
        egui::Theme::Light => egui::Color32::from_rgb(226, 235, 250),
    }
}

/// A toolbar command button with an accent-blue "ribbon" treatment so the
/// action rows read as commands rather than page content. Keeps the 3D
/// bevel/hover-lift/press-in states via a scoped widget-style override.
fn toolbar_button(
    ui: &mut egui::Ui,
    label: String,
    icon: Option<&egui::TextureHandle>,
) -> egui::Response {
    let dark = ui.visuals().dark_mode;
    let (face, hover_face, active_face, border, hover_border) = if dark {
        (
            egui::Color32::from_rgb(45, 64, 84),
            egui::Color32::from_rgb(58, 82, 106),
            egui::Color32::from_rgb(30, 44, 58),
            egui::Color32::from_rgb(17, 25, 34),
            egui::Color32::from_rgb(104, 148, 190),
        )
    } else {
        (
            egui::Color32::from_rgb(232, 241, 250),
            egui::Color32::WHITE,
            egui::Color32::from_rgb(184, 212, 240),
            egui::Color32::from_rgb(163, 197, 229),
            egui::Color32::from_rgb(110, 165, 220),
        )
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
fn context_menu_stroke(theme: egui::Theme) -> egui::Stroke {
    match theme {
        egui::Theme::Dark => egui::Stroke::new(1.0, egui::Color32::from_rgb(72, 88, 115)),
        egui::Theme::Light => egui::Stroke::new(1.0, egui::Color32::from_rgb(150, 170, 205)),
    }
}

/// Same as `Response::context_menu`, but paints the popup with the distinct
/// per-theme fill from `context_menu_fill` (egui's popup frame reads
/// `visuals.window_fill`, which defaults to the page background color).
fn styled_context_menu<R>(
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
        if ui.button("Open in Windows Explorer").clicked() {
            *row_action = Some(RowAction::OpenInExplorer(entry_path.to_path_buf()));
            ui.close();
        }
        if ui.button("★ Add to Favourites").clicked() {
            *row_action = Some(RowAction::FavouriteFolder(entry_path.to_path_buf()));
            ui.close();
        }
    }
}

fn help_heading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(8.0);
    ui.label(egui::RichText::new(text).strong().heading());
    ui.add_space(2.0);
}

fn help_content(ui: &mut egui::Ui) {
    let w = |ui: &mut egui::Ui, text: &str| {
        ui.label(egui::RichText::new(text).weak());
    };

    help_heading(ui, "Getting Started");
    w(ui, "FileMan is a dual-pane file manager. The left panel shows a folder tree with your Favourites at the top. The center area has two independent file browsers, each with tabs, an address bar, a filter, and navigation buttons.");
    ui.add_space(4.0);
    w(ui, "Switch users via the dropdown in the top-right corner. Each user has independent settings, favourites, toolbar layout, and shortcuts.");

    help_heading(ui, "Navigation");
    w(ui, "Address Bar — type a path and press Enter to navigate directly.");
    w(ui, "Back (Alt+Left) — return to the previous folder.");
    w(ui, "Forward (Alt+Right) — go forward after going back.");
    w(ui, "Up (Backspace) — go to the parent folder.");
    w(ui, "Tabs — each pane supports multiple tabs. Open a new tab with + Tab, or close one with the x on hover. Pinned tabs resist accidental navigation.");

    help_heading(ui, "View Modes");
    w(ui, "Switch between layouts via Settings > View:");
    w(ui, "  Details — columns for name, date, type, size. Click headers to sort.");
    w(ui, "  List — compact single-column list.");
    w(ui, "  Icons — large icon grid for image-heavy folders.");
    w(ui, "The filter box (next to the Up button) narrows visible files by name. Click the red x to clear.");

    help_heading(ui, "File Operations");
    w(ui, "Toolbar buttons provide quick access to Copy (Ctrl+C), Cut (Ctrl+X), Paste (Ctrl+V), Delete (Del), Rename (F2), New Folder, New File, Find (Ctrl+F), and Refresh (F5).");
    w(ui, "Right-click any file or folder for the context menu with additional options: Extract, Copy Filename, Copy Folder Path, Open With, Open in Windows Explorer, and Add to Favourites.");

    help_heading(ui, "Favourites");
    w(ui, "Right-click a folder and select Add to Favourites to pin it to the Folder Tree. Right-click a favourite to remove it.");

    help_heading(ui, "Custom Actions");
    w(ui, "Custom actions let you open files with any application. Go to Settings > Custom Actions to add one. Each action shows as an icon button on the second toolbar row.");

    help_heading(ui, "Settings");
    w(ui, "Appearance — theme (Light/Dark), font family, font size, tab layout (horizontal/vertical).");
    w(ui, "Keyboard Shortcuts — click Rebind next to any action, then press the new key combination.");
    w(ui, "Toolbar — reorder or toggle which buttons appear on the main row.");
    w(ui, "View — choose the default listing layout (Details, List, or Icons).");
    w(ui, "Advanced — set FileMan as the default folder explorer, or export/import all settings via JSON.");

    help_heading(ui, "Keyboard Shortcuts");
    w(ui, "Ctrl+C Copy | Ctrl+X Cut | Ctrl+V Paste | Ctrl+F Find");
    w(ui, "F2 Rename | F3 Copy Filename | F4 Copy Folder Path | F5 Refresh");
    w(ui, "Backspace Go Up | Delete Delete | Alt+Left Back | Alt+Right Forward");
    w(ui, "Enter Confirm | Escape Cancel / Close dialog");

    help_heading(ui, "Tips");
    w(ui, "- Pinned tabs won't navigate away when you double-click a folder.");
    w(ui, "- The filter is per-tab, so each pane filters independently.");
    w(ui, "- Drag the pane divider to resize left/right panes.");
    w(ui, "- Press Esc to close any dialog including this Help window.");
    w(ui, "- Use Export/Import in Advanced settings to transfer your setup to another machine.");
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
