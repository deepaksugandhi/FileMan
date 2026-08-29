use crate::actions::{Action, ActionRef};
use crate::archive;
use crate::fs_ops::{self, ClipboardOp};
use crate::pane::Pane;
use crate::progress::{self, BackgroundOp, OpStatus};
use crate::session::{self, WindowGeometry};
use crate::tab::ViewMode;
use crate::tree;
use eframe::egui;
use egui::scroll_area::ScrollBarVisibility;
use rusqlite::Connection;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

/// Height of the launcher/file-launch search boxes on the toolbar's second
/// row, matched to `toolbar_button`'s icon+text button height so the boxes
/// don't sit shorter (and thus off-center) than the buttons beside them.
const TOOLBAR_ROW2_HEIGHT: f32 = 24.0;

/// Reads file paths from the Windows clipboard in `CF_HDROP` format.
///
/// Returns `Some(paths)` when the clipboard holds file drop data (e.g. after
/// a Ctrl+C in Explorer), or `None` if the clipboard is empty or doesn't
/// contain `CF_HDROP` data.
#[cfg(windows)]
fn read_os_clipboard_hdrop() -> Option<Vec<PathBuf>> {
    use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
    use windows::Win32::System::Ole::CF_HDROP;
    use windows::Win32::UI::Shell::DragQueryFileW;

    unsafe {
        if OpenClipboard(None).is_err() {
            return None;
        }

        let hdrop_result = GetClipboardData(CF_HDROP.0 as u32);
        let hdrop = match hdrop_result {
            Ok(h) => h,
            Err(_) => {
                let _ = CloseClipboard();
                return None;
            }
        };

        let hdrop = windows::Win32::UI::Shell::HDROP(hdrop.0);
        let count = DragQueryFileW(hdrop, 0xFFFF_FFFF, None);
        let mut paths = Vec::with_capacity(count as usize);
        for i in 0..count {
            let len = DragQueryFileW(hdrop, i, None) as usize;
            let mut buf = vec![0u16; len + 1];
            DragQueryFileW(hdrop, i, Some(&mut buf));
            let s = String::from_utf16_lossy(&buf[..len]);
            if !s.is_empty() {
                paths.push(PathBuf::from(s));
            }
        }

        let _ = CloseClipboard();

        if paths.is_empty() {
            None
        } else {
            Some(paths)
        }
    }
}

/// Non-Windows stub – clipboard file-drop reading is not supported.
#[cfg(not(windows))]
fn read_os_clipboard_hdrop() -> Option<Vec<PathBuf>> {
    None
}

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
    Rename {
        path: PathBuf,
        name: String,
    },
    NewFolder {
        name: String,
    },
    NewFile {
        name: String,
    },
    /// Shown when a copy/move hits name collisions in the destination: the
    /// user picks overwrite or keep-as-copy for the shown item (Shift+click
    /// applies the choice to every remaining conflict). `conflicts` holds the
    /// items still awaiting a decision; `resolved` accumulates the decisions
    /// for the whole batch (conflict-free items start out in there) and runs
    /// as one background transfer once `conflicts` drains.
    PasteConflict {
        dest_dir: PathBuf,
        op: Option<ClipboardOp>,
        conflicts: Vec<PathBuf>,
        resolved: Vec<crate::progress::TransferItem>,
    },
    /// Tab context menu: right-click on a tab to duplicate or close it.
    TabContext {
        pane_idx: usize,
        tab_idx: usize,
    },
    /// Renaming a tab's display label (independent of its folder).
    RenameTab {
        pane_idx: usize,
        tab_idx: usize,
        name: String,
    },
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
        /// Set once the Find input has been given initial focus, so we don't
        /// keep stealing focus back from the user on every frame.
        query_focused: bool,
    },
    /// Create a new user profile.
    NewUser {
        name: String,
    },
    /// Help / user manual.
    Help,
    /// Confirm delete: paths ready to be deleted, waiting for user confirmation.
    ConfirmDelete {
        paths: Vec<PathBuf>,
    },
    /// A column header was clicked in Details view: the candidate sorting
    /// (`col`/`asc`) is waiting for the user to choose its scope — every
    /// open tab (which also becomes the universal default for future tabs),
    /// or just the tab that was clicked. `pane_idx` records which pane's
    /// tab was clicked so the dialog can describe the exact change even if
    /// the user clicks around while it is open.
    ApplySort {
        col: String,
        asc: bool,
        pane_idx: usize,
    },
}

pub struct FileManApp {
    conn: Connection,
    current_user_id: i64,
    /// Cached list of user profiles, refreshed on switch/create.
    users: Vec<crate::user::User>,
    panes: Vec<Pane>,
    active_pane: usize,
    dirty: bool,
    /// When `dirty` was last flushed to SQLite. Window/divider drags set
    /// `dirty` every frame, and each `persist()` is a full transaction with
    /// an fsync — so coalesce them instead of writing per frame.
    last_persist: std::time::Instant,
    /// Whether the window had focus last frame, so regaining it can trigger a
    /// re-listing of both panes (files may have changed in another app).
    was_focused: bool,
    last_size: egui::Vec2,
    clipboard: Vec<PathBuf>,
    clipboard_op: Option<ClipboardOp>,
    dialog: Option<Dialog>,
    /// Set to `true` when a dialog is opened this frame, cleared after the
    /// first render. Used to seed one-time widget state (e.g. text selection
    /// in the rename modal).
    dialog_just_opened: bool,
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
    /// Last window title string sent via `ViewportCommand::Title`, so we only
    /// re-issue the command when the active folder actually changes.
    last_title: String,
    /// Last known top-left window position in screen points, for persistence.
    last_pos: Option<(f32, f32)>,
    /// Background file operation in progress (copy/move/delete).
    background_op: Option<BackgroundOp>,
    /// Editable address bar text for this pane.
    /// Index of the pane whose address bar is currently focused (being edited).
    focused_address_pane: Option<usize>,
    /// Cached network server UNC paths for the sidebar tree.
    network_servers: Vec<PathBuf>,
    /// Cached shell-known folders (Desktop, Documents, Downloads, …) for
    /// the sidebar tree. Resolved once at startup — they're OS-wide, not
    /// per-user-profile, and the shell lookup isn't free.
    system_folders: Vec<(String, PathBuf)>,
    /// Drive roots for the sidebar. `list_drives` stats all 26 letters, and
    /// an offline mapped drive can block for hundreds of ms — so resolve it
    /// once at startup rather than every frame.
    // ponytail: never refreshed, so a drive plugged in mid-session needs a
    // restart to appear. Add a WM_DEVICECHANGE hook if that becomes annoying.
    drives: Vec<PathBuf>,
    /// Favourite folder paths for quick access.
    favourites: Vec<String>,
    /// Recently accessed files and folders, newest first.
    recent_items: Vec<crate::db::RecentItem>,
    /// Whether the Recent dropdown popup is open.
    show_recent_popup: bool,
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
    /// Configured launcher apps for the quick-launch toolbar.
    launcher_apps: Vec<crate::actions::LauncherApp>,
    /// Text in the launcher search/filter input.
    launcher_filter: String,
    /// Icons loaded for launcher apps, keyed by exe path.
    launcher_icons: HashMap<String, Option<egui::TextureHandle>>,
    /// Draft label for the settings add-new-launcher-app form.
    new_launcher_label: String,
    /// Draft exe path for the settings add-new-launcher-app form.
    new_launcher_exe: Option<PathBuf>,
    /// Draft args for the settings add-new-launcher-app form.
    new_launcher_args: String,
    /// Configured file launch shortcuts.
    file_launches: Vec<crate::actions::FileLaunch>,
    /// Text in the file launch search/filter input.
    file_launch_filter: String,
    /// Draft label for the settings add-new-file-launch form.
    new_file_launch_label: String,
    /// Draft file path for the settings add-new-file-launch form.
    new_file_launch_file: Option<PathBuf>,
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
    /// Draft extension for the "File Types" add-override form.
    new_ext_override_ext: String,
    /// Draft executable path for the "File Types" add-override form.
    new_ext_override_exe: Option<PathBuf>,
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
    /// Whether hidden files/folders (Windows FILE_ATTRIBUTE_HIDDEN) are shown
    /// in listings, persisted per user as `show_hidden`. Off by default.
    show_hidden: bool,
    /// Labels of Windows Explorer shell context-menu items to omit from the
    /// "Windows Explorer" submenu, persisted per user as
    /// `shell_menu_hidden` (newline-joined). Empty by default.
    shell_menu_hidden: std::collections::HashSet<String>,
    /// Scratch buffer for the Settings > Shell Menu textarea, kept separate
    /// from `shell_menu_hidden` so in-progress edits aren't lost on rebuild.
    shell_menu_hidden_text: String,
    /// Cached result of the last `shell_menu::query_items` call, keyed by
    /// path. `Response::context_menu`'s closure body re-runs every frame
    /// while the menu is open, and `query_items` is a blocking COM/shell
    /// call (slow, and worse over a network path) — without this cache it
    /// re-ran on every frame, stalling the UI thread and flickering the
    /// cursor between arrow and hourglass.
    shell_menu_cache: Option<(Vec<std::path::PathBuf>, Vec<crate::shell_menu::ShellMenuItem>)>,
    /// State for the tips card: current tip, rotation timing and session
    /// visibility.
    tips: crate::tips::TipsCard,
    /// Pane body rects captured during rendering, for drag & drop
    /// hit-testing. Refreshed every frame.
    dnd_pane_rects: [Option<egui::Rect>; 2],
    /// Tab-header rects captured during rendering — `((pane, tab), rect,
    /// is_active)` — so a dragged item hovering an inactive tab can open it.
    dnd_tab_rects: Vec<((usize, usize), egui::Rect, bool)>,
    /// In-progress drag-to-reorder of a tab inside its pane's strip, if any.
    tab_reorder: Option<TabReorderDrag>,
    /// Subdirectory listing for each expanded sidebar-tree folder, keyed by
    /// path. `CollapsingHeader::show` re-runs its body closure every frame a
    /// node is open, so without this cache an expanded branch would re-hit
    /// the filesystem (`read_dir`) on every single repaint. Invalidated via
    /// `mark_dir_dirty`.
    tree_subdirs_cache: HashMap<PathBuf, Vec<PathBuf>>,
    /// In-flight background `list_subdirs` calls, keyed by directory. A
    /// network folder's `read_dir` can block for seconds; listing subdirs on
    /// a background thread (like the main pane's listing job) keeps the tree
    /// from freezing the whole UI while a branch is expanding.
    tree_subdirs_jobs: HashMap<PathBuf, mpsc::Receiver<std::io::Result<Vec<PathBuf>>>>,
    /// Hand-off point with the OLE drop target registered in
    /// `native_drag.rs`: this frame's pane/tab rects go in, a resolved drop
    /// comes back out. See `native_drag`'s module docs for why.
    dnd_shared: crate::native_drag::SharedDndState,
    /// Set once `native_drag::register_drop_target` has succeeded for this
    /// window (needs a live `eframe::Frame`, unavailable at construction).
    drop_target_registered: bool,
    /// Cached raw window handle for Win32 API calls (e.g. shell context
    /// menu). Set once per frame when the `eframe::Frame` is available.
    #[cfg(windows)]
    hwnd: Option<windows::Win32::Foundation::HWND>,
    /// A row-drag that just started this frame, queued for
    /// `start_native_drag` once the current pane borrow ends.
    pending_native_drag: Option<(usize, Vec<PathBuf>, PathBuf)>,
    /// Universal default sorting for Details view (config keys `sort_col`/
    /// `sort_asc`). Every newly opened tab starts with this; tabs the user
    /// re-sorted individually keep their own choice instead.
    universal_sort_col: String,
    /// Direction of `universal_sort_col`; `true` is ascending.
    universal_sort_asc: bool,
}

/// Resolves the raw HWND of the app window. `None` on non-Windows or if the
/// handle lookup fails.
fn window_hwnd(frame: &eframe::Frame) -> Option<windows::Win32::Foundation::HWND> {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let handle = frame.window_handle().ok()?;
        if let RawWindowHandle::Win32(h) = handle.as_raw() {
            return Some(windows::Win32::Foundation::HWND(h.hwnd.get() as *mut _));
        }
        None
    }
    #[cfg(not(windows))]
    {
        let _ = frame;
        None
    }
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
                fonts.font_data.insert(
                    "custom".to_owned(),
                    std::sync::Arc::new(egui::FontData::from_owned(bytes)),
                );
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
    AppLauncher,
    FileLauncher,
    FileTypes,
    ViewMode,
    Advanced,
    About,
}

/// Whitelists a stored universal-sort column so a hand-edited settings table
/// can't put new tabs into an unsortable state.
fn parse_sort_col(raw: &str) -> Option<&'static str> {
    match raw {
        "name" => Some("name"),
        "modified" => Some("modified"),
        "size" => Some("size"),
        "archive" => Some("archive"),
        _ => None,
    }
}

/// Human-readable label for a sort column id (matches the Details-view
/// headers).
fn sort_col_label(col: &str) -> &'static str {
    match col {
        "modified" => "Modified",
        "size" => "Size",
        "archive" => "Attributes",
        _ => "Name",
    }
}

/// The sorting a column-header click selects: clicking the active column
/// reverses its direction; clicking another column sorts by it ascending.
fn next_sort(current_col: &str, current_asc: bool, clicked: &str) -> (String, bool) {
    if current_col == clicked {
        (current_col.to_string(), !current_asc)
    } else {
        (clicked.to_string(), true)
    }
}

/// Pads `panes` up to two panes rooted at C:\ if there are fewer than two,
/// truncating if there are more (shouldn't happen given the session schema,
/// but be safe), and clamping `active_pane` into the resulting valid range.
/// Freshly created panes' first tab is seeded with `default_sort` — the
/// user's universal sorting — while already-loaded panes keep whatever each
/// restored tab had saved.
fn ensure_two_panes(
    mut panes: Vec<Pane>,
    active_pane: usize,
    default_sort: (&str, bool),
) -> (Vec<Pane>, usize) {
    while panes.len() < 2 {
        let mut pane = Pane::new(PathBuf::from("C:\\"));
        pane.tabs[0].sort_col = default_sort.0.to_string();
        pane.tabs[0].sort_asc = default_sort.1;
        panes.push(pane);
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
        let universal_sort_col = crate::config::get(&conn, current_user_id, "sort_col")
            .and_then(|raw| parse_sort_col(&raw))
            .unwrap_or("name")
            .to_string();
        let universal_sort_asc = crate::config::get(&conn, current_user_id, "sort_asc")
            .map(|raw| raw != "false")
            .unwrap_or(true);
        let (panes, active_pane) = match loaded {
            Some(s) if !s.panes.is_empty() => ensure_two_panes(
                s.panes,
                s.active_pane,
                (&universal_sort_col, universal_sort_asc),
            ),
            _ => ensure_two_panes(Vec::new(), 0, (&universal_sort_col, universal_sort_asc)),
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
        let tips_enabled =
            crate::config::get(&conn, current_user_id, crate::tips::KEY_TIPS_ENABLED)
                .map(|raw| raw != "false")
                .unwrap_or(true);
        let show_hidden = crate::config::get(&conn, current_user_id, "show_hidden")
            .map(|raw| raw == "true")
            .unwrap_or(false);
        let shell_menu_hidden_text =
            crate::config::get(&conn, current_user_id, "shell_menu_hidden").unwrap_or_default();
        let shell_menu_hidden = parse_shell_menu_hidden(&shell_menu_hidden_text);
        let favourites = crate::db::get_favourites(&conn, current_user_id);
        let split_ratio = crate::db::get_split_ratio(&conn, current_user_id).unwrap_or(0.5);
        let tree_width = crate::db::get_tree_width(&conn, current_user_id).unwrap_or(200.0);
        let users = crate::user::list_users(&conn);
        let _ = crate::actions::init_tables(&conn);
        let shortcut_map = crate::actions::load_shortcut_map(&conn, current_user_id);
        let toolbar_actions = crate::actions::load_toolbar(&conn, current_user_id);
        let custom_actions = crate::actions::list_custom_actions(&conn, current_user_id);
        let launcher_apps = crate::actions::list_launcher_apps(&conn, current_user_id);
        let file_launches = crate::actions::list_file_launches(&conn, current_user_id);
        let mut panes = panes;
        let startup_path = if let Some(dir) = startup_dir {
            // Launched as the default folder explorer with a clicked folder.
            let first = &mut panes[0].tabs[0];
            first.path = dir.clone();
            first.listing_dirty = true;
            Some(dir)
        } else {
            Some(panes[0].tabs[0].path.clone())
        };
        // Record the initial directory in the recent list so it's not blank
        // on first launch.
        if let Some(ref p) = startup_path {
            crate::db::add_recent_item(&conn, current_user_id, &p.display().to_string(), p.is_dir());
        }
        let recent_items = crate::db::get_recent_items(&conn, current_user_id, 50);
        FileManApp {
            conn,
            current_user_id,
            users,
            panes,
            active_pane,
            dirty: false,
            last_persist: std::time::Instant::now(),
            was_focused: true,
            last_size: egui::vec2(1200.0, 800.0),
            clipboard: Vec::new(),
            clipboard_op: None,
            dialog: None,
            dialog_just_opened: false,
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
            last_title: String::new(),
            last_pos: None,
            background_op: None,
            focused_address_pane: None,
            network_servers: tree::list_network_servers(),
            system_folders: tree::list_system_folders(),
            drives: tree::list_drives(),
            favourites,
            recent_items,
            show_recent_popup: false,
            listing_jobs: [None, None],
            background_op_dirs: Vec::new(),
            split_ratio,
            tree_width,
            last_monitor_name: None,
            shortcut_map,
            toolbar_actions,
            custom_actions,
            custom_icons: HashMap::new(),
            launcher_apps,
            launcher_filter: String::new(),
            launcher_icons: HashMap::new(),
            new_launcher_label: String::new(),
            new_launcher_exe: None,
            new_launcher_args: String::new(),
            file_launches,
            file_launch_filter: String::new(),
            new_file_launch_label: String::new(),
            new_file_launch_file: None,
            file_icons: HashMap::new(),
            capturing_shortcut_for: None,
            new_custom_action_label: String::new(),
            new_custom_action_exe: None,
            new_ext_override_ext: String::new(),
            new_ext_override_exe: None,
            find_job: None,
            tab_orientation,
            tab_strip_width,
            taskbar_badge_applied: false,
            instance_slot,
            tips_enabled,
            show_hidden,
            shell_menu_hidden,
            shell_menu_hidden_text,
            shell_menu_cache: None,
            tips: crate::tips::TipsCard::new(),
            dnd_pane_rects: [None, None],
            dnd_tab_rects: Vec::new(),
            tab_reorder: None,
            tree_subdirs_cache: HashMap::new(),
            tree_subdirs_jobs: HashMap::new(),
            dnd_shared: std::sync::Arc::new(std::sync::Mutex::new(
                crate::native_drag::DndSharedState::default(),
            )),
            drop_target_registered: false,
            #[cfg(windows)]
            hwnd: None,
            pending_native_drag: None,
            universal_sort_col,
            universal_sort_asc,
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
                            tab.listing = if self.show_hidden {
                                entries
                            } else {
                                entries.into_iter().filter(|e| !e.hidden).collect()
                            };
                            tab.listing_error = None;
                            tab.listing_version += 1;
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
        self.tree_subdirs_cache.remove(dir);
        self.tree_subdirs_jobs.remove(dir);
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
        let _ = session::save_session(
            &self.conn,
            self.current_user_id,
            &window,
            &self.panes,
            self.active_pane,
        );
        let _ = crate::db::set_split_ratio(&self.conn, self.current_user_id, self.split_ratio);
        let _ = crate::db::set_tree_width(&self.conn, self.current_user_id, self.tree_width);
    }

    fn active_tab_dir(&self) -> PathBuf {
        self.panes[self.active_pane].active_tab().path.clone()
    }

    /// Opens `path` with the user's pinned per-extension override if one
    /// exists (Settings > File Types), otherwise the Windows default app.
    fn open_path(&self, path: &std::path::Path) {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .and_then(|ext| crate::actions::get_ext_override(&self.conn, self.current_user_id, ext));
        match ext {
            Some(exe) => {
                let _ = std::process::Command::new(exe).arg(path).spawn();
            }
            None => {
                let _ = std::process::Command::new("cmd")
                    .args(["/C", "start", "", &path.to_string_lossy()])
                    .spawn();
            }
        }
    }

    /// Opens a new tab on `pane_idx` seeded with the user's universal default
    /// sorting rather than the built-in name-ascending fallback.
    fn open_tab_with_default_sort(&mut self, pane_idx: usize, path: PathBuf) {
        let (col, asc) = (self.universal_sort_col.clone(), self.universal_sort_asc);
        let pane = &mut self.panes[pane_idx];
        pane.open_tab(path.clone());
        let tab = pane.active_tab_mut();
        tab.sort_col = col;
        tab.sort_asc = asc;
        self.record_recent(&path, path.is_dir());
    }

    /// Applies `col`/`asc` to every open tab in both panes and stores them as
    /// the user's universal default that all future tabs start with.
    fn apply_sort_everywhere(&mut self, col: &str, asc: bool) {
        for pane in &mut self.panes {
            for tab in &mut pane.tabs {
                tab.sort_col = col.to_string();
                tab.sort_asc = asc;
            }
        }
        self.universal_sort_col = col.to_string();
        self.universal_sort_asc = asc;
        let _ = crate::config::set(
            &self.conn,
            crate::config::Scope::User(self.current_user_id),
            "sort_col",
            col,
        );
        let _ = crate::config::set(
            &self.conn,
            crate::config::Scope::User(self.current_user_id),
            "sort_asc",
            if asc { "true" } else { "false" },
        );
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

    /// Records a file or folder as recently accessed.
    fn record_recent(&mut self, path: &std::path::Path, is_dir: bool) {
        let path_str = path.display().to_string();
        crate::db::add_recent_item(&self.conn, self.current_user_id, &path_str, is_dir);
        self.recent_items = crate::db::get_recent_items(&self.conn, self.current_user_id, 50);
    }

    /// Removes a single item from the recent list.
    #[allow(dead_code)]
    fn remove_recent(&mut self, path: &str) {
        crate::db::remove_recent_item(&self.conn, self.current_user_id, path);
        self.recent_items = crate::db::get_recent_items(&self.conn, self.current_user_id, 50);
    }

    /// Clears all recent items.
    fn clear_recent(&mut self) {
        crate::db::clear_recent_items(&self.conn, self.current_user_id);
        self.recent_items.clear();
        self.status = "Recent history cleared".to_string();
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
        self.universal_sort_col = crate::config::get(&self.conn, user_id, "sort_col")
            .and_then(|raw| parse_sort_col(&raw))
            .unwrap_or("name")
            .to_string();
        self.universal_sort_asc = crate::config::get(&self.conn, user_id, "sort_asc")
            .map(|raw| raw != "false")
            .unwrap_or(true);
        let (panes, active_pane) = match loaded {
            Some(s) if !s.panes.is_empty() => ensure_two_panes(
                s.panes,
                s.active_pane,
                (&self.universal_sort_col, self.universal_sort_asc),
            ),
            _ => ensure_two_panes(
                Vec::new(),
                0,
                (&self.universal_sort_col.clone(), self.universal_sort_asc),
            ),
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
        self.recent_items = crate::db::get_recent_items(&self.conn, user_id, 50);
        self.shortcut_map = crate::actions::load_shortcut_map(&self.conn, user_id);
        self.toolbar_actions = crate::actions::load_toolbar(&self.conn, user_id);
        self.custom_actions = crate::actions::list_custom_actions(&self.conn, user_id);
        self.launcher_apps = crate::actions::list_launcher_apps(&self.conn, user_id);
        self.launcher_filter.clear();
        self.launcher_icons.clear();
        self.file_launches = crate::actions::list_file_launches(&self.conn, user_id);
        self.file_launch_filter.clear();
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
                    self.dialog_just_opened = true; self.dialog = Some(Dialog::NewFolder {
                        name: String::new(),
                    });
                }
                Action::NewFile => {
                    self.dialog_just_opened = true; self.dialog = Some(Dialog::NewFile {
                        name: String::new(),
                    });
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
                        let path = pane.active_tab().path.clone();
                        self.record_recent(&path, path.is_dir());
                        self.dirty = true;
                    }
                }
                Action::GoForward => {
                    let pane = &mut self.panes[self.active_pane];
                    if pane.active_tab().locked {
                        self.status = "Tab is pinned — unpin it to navigate".to_string();
                    } else if pane.active_tab_mut().go_forward() {
                        let path = pane.active_tab().path.clone();
                        self.record_recent(&path, path.is_dir());
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
                    self.open_tab_with_default_sort(self.active_pane, current);
                    self.dirty = true;
                }
                Action::CloseTab => {
                    let pane = &mut self.panes[self.active_pane];
                    let idx = pane.active_tab;
                    pane.close_tab(idx);
                    self.dirty = true;
                }
                Action::Refresh => {
                    let dir = self.active_tab_dir();
                    self.panes[self.active_pane].active_tab_mut().listing_dirty = true;
                    self.tree_subdirs_cache.remove(&dir);
                    self.tree_subdirs_jobs.remove(&dir);
                }
                Action::Find => {
                    let search_path = self.active_tab_dir();
                    self.dialog_just_opened = true; self.dialog = Some(Dialog::Find {
                        query: String::new(),
                        results: Vec::new(),
                        search_path,
                        sort_col: "name".to_string(),
                        sort_asc: true,
                        name_filter: String::new(),
                        folder_filter: String::new(),
                        include_folders: true,
                        query_focused: false,
                    });
                }
                Action::ToggleSettings => self.show_settings = !self.show_settings,
                Action::SelectAll => self.select_all_in_view(),
            },
            ActionRef::Custom(id) => {
                if let Some(custom) = self.custom_actions.iter().find(|c| c.id == id) {
                    let paths = self.selected_paths();
                    if !paths.is_empty() {
                        let mut cmd = std::process::Command::new(&custom.exe_path);
                        cmd.args(&paths);
                        let _ = cmd.spawn();
                    }
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
        self.panes[pane_idx].active_tab_mut().try_navigate(path.clone());
        self.record_recent(&path, path.is_dir());
        true
    }

    /// Renders one node of the sidebar folder tree: a collapsing header that
    /// lazily lists its subdirectories when expanded. Clicking a header
    /// toggles expand/collapse; navigating only happens when expanding.
    /// When `force_expand` is true, ancestor nodes are forced open (used
    /// after navigation to reveal the active path in the tree). `label`
    /// overrides the header text (system folders show a friendly name
    /// instead of the path's last segment).
    fn show_dir_node(
        &mut self,
        ui: &mut egui::Ui,
        dir: &Path,
        label: Option<&str>,
        active_path: &Path,
        force_expand: bool,
    ) {
        let label = label
            .map(|l| l.to_string())
            .unwrap_or_else(|| {
                dir.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| dir.display().to_string())
            });
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
        let mut header =
            egui::CollapsingHeader::new(header_text).id_salt(format!("tree_{}", dir.display()));
        if force_expand && is_ancestor {
            header = header.open(Some(true));
        } else if self.tree_collapse_frames > 0 && !is_ancestor {
            // Explorer-style: while the post-navigation window is open,
            // collapse every branch that isn't on the active path. `open`
            // persists through CollapsingState, so this sticks afterwards.
            header = header.open(Some(false));
        }
        let response = header.show(ui, |ui| {
            if let Some(subdirs) = self.tree_subdirs_cache.get(dir).cloned() {
                for subdir in subdirs {
                    self.show_dir_node(ui, &subdir, None, active_path, force_expand);
                }
                return;
            }
            // Not cached yet: poll (or start) a background `list_subdirs`
            // job rather than blocking here — a network folder's `read_dir`
            // can take seconds, and this closure re-runs every frame the
            // branch is expanded.
            let mut resolved: Option<Vec<PathBuf>> = None;
            if let Some(rx) = self.tree_subdirs_jobs.get(dir) {
                match rx.try_recv() {
                    Ok(result) => resolved = Some(result.unwrap_or_default()),
                    Err(mpsc::TryRecvError::Empty) => {}
                    Err(mpsc::TryRecvError::Disconnected) => resolved = Some(Vec::new()),
                }
            } else {
                let (tx, rx) = mpsc::channel();
                let job_dir = dir.to_path_buf();
                thread::spawn(move || {
                    let _ = tx.send(crate::fs_entry::list_subdirs(&job_dir));
                });
                self.tree_subdirs_jobs.insert(dir.to_path_buf(), rx);
            }
            if let Some(subdirs) = resolved {
                self.tree_subdirs_jobs.remove(dir);
                self.tree_subdirs_cache
                    .insert(dir.to_path_buf(), subdirs.clone());
                for subdir in subdirs {
                    self.show_dir_node(ui, &subdir, None, active_path, force_expand);
                }
            } else {
                ui.horizontal(|ui| {
                    ui.add_space(18.0);
                    ui.add(egui::Spinner::new().size(12.0));
                });
                ui.ctx().request_repaint();
            }
        });
        if is_active {
            // Keep centering the active folder while the post-navigation
            // scroll window is open (see `tree_scroll_frames`).
            if self.tree_scroll_frames > 0 {
                response
                    .header_response
                    .scroll_to_me(Some(egui::Align::Center));
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

    /// Ctrl+A: selects every entry currently visible in the active pane's
    /// listing (respecting the tab's name filter), like Explorer.
    fn select_all_in_view(&mut self) {
        let tab = self.panes[self.active_pane].active_tab_mut();
        let (filter, sort_col, sort_asc) =
            (tab.filter.clone(), tab.sort_col.clone(), tab.sort_asc);
        let names: Vec<String> = tab
            .display_entries(&filter, &sort_col, sort_asc)
            .iter()
            .map(|e| e.name.clone())
            .collect();
        if names.is_empty() {
            return;
        }
        self.last_selected_index = Some(names.len() - 1);
        tab.select_all(&names);
        self.dirty = true;
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

    /// Builds the breadcrumb trail for `path`: each ancestor from the root
    /// down to `path` itself, paired with its display label (folder/drive
    /// name), for rendering as clickable navigation segments.
    fn path_breadcrumbs(path: &Path) -> Vec<(String, PathBuf)> {
        let mut ancestors: Vec<&Path> = path.ancestors().collect();
        ancestors.reverse();
        ancestors
            .into_iter()
            .map(|anc| {
                let label = anc
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| anc.display().to_string());
                (label, anc.to_path_buf())
            })
            .collect()
    }

    /// Resolves the subdirectories of `dir` from the shared tree cache,
    /// starting a background `list_subdirs` job when not cached yet (a
    /// network folder's `read_dir` can block for seconds). Returns `None`
    /// while the listing is still in flight. Shares the sidebar tree's
    /// cache/jobs, so an already-expanded branch resolves instantly and
    /// `mark_dir_dirty` invalidates both consumers at once.
    fn poll_subdirs(
        cache: &mut HashMap<PathBuf, Vec<PathBuf>>,
        jobs: &mut HashMap<PathBuf, mpsc::Receiver<std::io::Result<Vec<PathBuf>>>>,
        dir: &Path,
    ) -> Option<Vec<PathBuf>> {
        if let Some(subdirs) = cache.get(dir) {
            return Some(subdirs.clone());
        }
        let mut resolved: Option<Vec<PathBuf>> = None;
        if let Some(rx) = jobs.get(dir) {
            match rx.try_recv() {
                Ok(result) => resolved = Some(result.unwrap_or_default()),
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => resolved = Some(Vec::new()),
            }
        } else {
            let (tx, rx) = mpsc::channel();
            let job_dir = dir.to_path_buf();
            thread::spawn(move || {
                let _ = tx.send(crate::fs_entry::list_subdirs(&job_dir));
            });
            jobs.insert(dir.to_path_buf(), rx);
        }
        if let Some(subdirs) = resolved {
            jobs.remove(dir);
            cache.insert(dir.to_path_buf(), subdirs.clone());
            Some(subdirs)
        } else {
            None
        }
    }

    /// Clickable breadcrumb separator: looks like the plain ">" label but
    /// opens a dropdown of `parent`'s subfolders on click, so any child can
    /// be jumped to directly. The child that continues the current path (if
    /// any) is highlighted. Returns the picked folder.
    fn crumb_separator_menu(
        ui: &mut egui::Ui,
        font_id: &egui::FontId,
        subdirs_cache: &mut HashMap<PathBuf, Vec<PathBuf>>,
        subdirs_jobs: &mut HashMap<PathBuf, mpsc::Receiver<std::io::Result<Vec<PathBuf>>>>,
        parent: &Path,
        current_child: Option<&Path>,
    ) -> Option<PathBuf> {
        let text_color = ui.visuals().text_color();
        let galley = ui
            .painter()
            .layout_no_wrap(">".to_string(), font_id.clone(), text_color);
        let (rect, resp) = ui.allocate_at_least(
            egui::vec2(galley.size().x, ui.spacing().interact_size.y),
            egui::Sense::click(),
        );
        let popup_id = resp.id.with("crumb_sep_menu");
        let menu_open = egui::Popup::is_id_open(ui.ctx(), popup_id);
        if resp.hovered() || menu_open {
            ui.painter().rect_filled(
                rect.expand(2.0),
                3.0,
                ui.visuals().widgets.open.weak_bg_fill,
            );
        }
        let glyph_color = if resp.hovered() || menu_open {
            ui.visuals().strong_text_color()
        } else {
            text_color
        };
        ui.painter().galley(
            egui::pos2(rect.min.x, rect.center().y - galley.size().y / 2.0),
            galley,
            glyph_color,
        );
        let resp = resp.on_hover_text("Show subfolders");
        let mut picked = None;
        let max_height = (ui.ctx().content_rect().max.y - rect.max.y - 12.0).clamp(120.0, 600.0);
        egui::Popup::menu(&resp)
            .id(popup_id)
            .show(|ui| {
                ui.set_min_width(180.0);
                egui::ScrollArea::vertical()
                    .max_height(max_height)
                    .show(ui, |ui| match Self::poll_subdirs(subdirs_cache, subdirs_jobs, parent) {
                        Some(subdirs) if subdirs.is_empty() => {
                            ui.label(egui::RichText::new("No subfolders").weak());
                        }
                        Some(subdirs) => {
                            for sub in subdirs {
                                let name = sub
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| sub.display().to_string());
                                let on_path = current_child == Some(sub.as_path());
                                if ui.selectable_label(on_path, name).clicked() {
                                    picked = Some(sub);
                                }
                            }
                        }
                        None => {
                            ui.horizontal(|ui| {
                                ui.add(egui::Spinner::new().size(12.0));
                                ui.label("Loading…");
                            });
                            ui.ctx().request_repaint();
                        }
                    });
            });
        picked
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
        // When the internal clipboard is empty (files were copied in Explorer
        // or another app), try reading CF_HDROP from the Windows clipboard.
        let clipboard_op = if self.clipboard.is_empty() {
            if let Some(os_paths) = read_os_clipboard_hdrop() {
                if os_paths.is_empty() {
                    self.status = "Clipboard is empty".into();
                    return;
                }
                self.status = format!(
                    "Imported {} item(s) from clipboard",
                    os_paths.len()
                );
                self.clipboard = os_paths;
                self.clipboard_op = Some(ClipboardOp::Copy);
                Some(ClipboardOp::Copy)
            } else {
                self.status = "Clipboard is empty".into();
                return;
            }
        } else {
            self.clipboard_op
        };
        let dest = self.active_tab_dir();
        let op = clipboard_op;

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
    /// collisions up front with a cheap `Path::exists` (no recursive walk).
    /// A clean batch runs immediately; any colliding item opens
    /// `Dialog::PasteConflict` so the user can choose overwrite or
    /// save-as-copy (Shift+click applies the choice to all conflicts).
    /// Either way the whole batch then runs as a single background
    /// operation with a progress bar, rather than blocking the UI thread —
    /// see `progress::copy_items_bg`/`move_items_bg`.
    fn transfer_items(&mut self, items: Vec<PathBuf>, dest: PathBuf, op: Option<ClipboardOp>) {
        let mut conflicts: Vec<PathBuf> = Vec::new();
        let mut resolved: Vec<progress::TransferItem> = Vec::new();
        for src in items {
            let collides = src
                .file_name()
                .is_some_and(|name| dest.join(name).exists());
            if collides {
                conflicts.push(src);
            } else {
                resolved.push(progress::TransferItem::plain(src));
            }
        }

        if conflicts.is_empty() {
            self.start_transfer(resolved, dest, op);
        } else {
            self.dialog_just_opened = true; self.dialog = Some(Dialog::PasteConflict {
                dest_dir: dest,
                op,
                conflicts,
                resolved,
            });
        }
    }

    /// Runs a fully-resolved transfer batch as one background operation.
    fn start_transfer(
        &mut self,
        items: Vec<progress::TransferItem>,
        dest: PathBuf,
        op: Option<ClipboardOp>,
    ) {
        self.background_op_dirs = vec![dest.clone()];
        if op == Some(ClipboardOp::Cut) {
            for item in &items {
                if let Some(parent) = item.src.parent() {
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

    /// Applies the user's choice from the `PasteConflict` dialog to either
    /// just the shown item or, when `apply_all` (Shift+click), every
    /// remaining conflict. When the last conflict is resolved, the whole
    /// batch is handed to `start_transfer`.
    fn resolve_paste_conflict(&mut self, overwrite: bool, apply_all: bool) {
        let Some(Dialog::PasteConflict {
            dest_dir,
            op,
            mut conflicts,
            mut resolved,
        }) = self.dialog.take()
        else {
            return;
        };

        // The dialog only exists while at least one conflict is pending.
        let take = if apply_all { conflicts.len() } else { 1 };
        let chosen: Vec<PathBuf> = conflicts.drain(..take).collect();

        // Names already claimed in the destination by this batch, so a
        // generated copy name can never collide with a sibling transfer.
        let mut taken: std::collections::HashSet<String> = resolved
            .iter()
            .map(|t| {
                t.dest_name.clone().unwrap_or_else(|| {
                    t.src
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                })
            })
            .collect();

        for src in chosen {
            if overwrite {
                resolved.push(progress::TransferItem {
                    src,
                    dest_name: None,
                    overwrite: true,
                });
            } else {
                let name = Self::next_free_copy_name(&dest_dir, &src, &mut taken);
                resolved.push(progress::TransferItem {
                    src,
                    dest_name: Some(name),
                    overwrite: false,
                });
            }
        }

        if conflicts.is_empty() {
            self.start_transfer(resolved, dest_dir, op);
        } else {
            self.dialog_just_opened = true; self.dialog = Some(Dialog::PasteConflict {
                dest_dir,
                op,
                conflicts,
                resolved,
            });
        }
    }

    /// A free "keep both" name for `src` inside `dest_dir`:
    /// `name (copy).ext`, then `name (2).ext`, `name (3).ext`, … — never
    /// colliding with the filesystem or with names already claimed in
    /// `taken` by the same transfer batch.
    fn next_free_copy_name(
        dest_dir: &Path,
        src: &Path,
        taken: &mut std::collections::HashSet<String>,
    ) -> String {
        let stem = src
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Copy".to_string());
        let ext = src
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        let mut candidate = format!("{stem} (copy){ext}");
        let mut n = 2u32;
        while dest_dir.join(&candidate).exists() || taken.contains(&candidate) {
            candidate = format!("{stem} ({n}){ext}");
            n += 1;
        }
        taken.insert(candidate.clone());
        candidate
    }

    /// Registers `native_drag`'s OLE drop target for this window, once a
    /// `frame` (and so an HWND) is available. Idempotent.
    fn ensure_drop_target_registered(&mut self, frame: &eframe::Frame) {
        if self.drop_target_registered {
            return;
        }
        if let Some(hwnd) = window_hwnd(frame) {
            #[cfg(windows)]
            {
                self.hwnd = Some(hwnd);
            }
            if crate::native_drag::register_drop_target(hwnd, self.dnd_shared.clone()).is_ok() {
                self.drop_target_registered = true;
            }
        }
    }

    /// Drives drag-to-reorder for `pane_idx`'s tab strip. Called once per
    /// frame with the freshly laid-out tab rects; starts a drag when a tab
    /// reports a press, live-swaps the dragged tab with whatever slot the
    /// pointer has reached, and ends the drag on button release. Reordering
    /// re-indexes tabs, so egui's per-widget drag tracking (keyed by widget
    /// id) can't carry the gesture — this tracks it manually instead.
    ///
    /// The insertion slot is computed from the *other* tabs' centers only:
    /// they don't move until an actual swap happens, so the calculation is
    /// stable frame-to-frame instead of oscillating at the midpoint.
    fn update_tab_reorder(
        &mut self,
        ui: &egui::Ui,
        pane_idx: usize,
        tab_rects: &[((usize, usize), egui::Rect, bool)],
        vertical_strip: bool,
        drag_started_at: Option<usize>,
    ) {
        if let Some(idx) = drag_started_at {
            if self.panes[pane_idx].tabs.len() > 1 {
                self.tab_reorder = Some(TabReorderDrag {
                    pane_idx,
                    idx,
                    moved: false,
                });
            }
        }
        let active = match self.tab_reorder {
            Some(d) if d.pane_idx == pane_idx => d,
            _ => return,
        };
        // The dragged tab may have been closed out from under the gesture
        // (e.g. keyboard shortcut mid-drag): drop the state instead of
        // indexing out of bounds.
        if active.idx >= self.panes[pane_idx].tabs.len() {
            self.tab_reorder = None;
            return;
        }
        let Some(pos) = ui.input(|i| i.pointer.interact_pos().or(i.pointer.latest_pos())) else {
            return;
        };
        let p = if vertical_strip { pos.y } else { pos.x };
        let axis_center = |r: &egui::Rect| {
            if vertical_strip {
                r.center().y
            } else {
                r.center().x
            }
        };
        let mut target = 0usize;
        for (i, (_, rect, _)) in tab_rects.iter().enumerate() {
            if i == active.idx {
                continue;
            }
            if p > axis_center(rect) {
                target += 1;
            }
        }
        if target != active.idx {
            self.panes[pane_idx].move_tab(active.idx, target);
            self.tab_reorder = Some(TabReorderDrag {
                pane_idx,
                idx: target,
                moved: true,
            });
            self.dirty = true;
        }
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    }

    /// Publishes this frame's pane/tab hit-test rects (already computed by
    /// `show_pane_body`/the tab strip) to the shared state `native_drag`'s
    /// drop target reads from — needed before a drag can start, since once
    /// `DoDragDrop` is running the OS calls that target directly, off
    /// egui's normal per-frame update.
    fn sync_dnd_shared(&self, ctx: &egui::Context) {
        let mut st = self.dnd_shared.lock().unwrap();
        st.pixels_per_point = ctx.pixels_per_point();
        for i in 0..2 {
            st.pane_rects[i] = self.dnd_pane_rects[i].map(|r| (r.min.x, r.min.y, r.max.x, r.max.y));
        }
        st.tab_rects = self
            .dnd_tab_rects
            .iter()
            .map(|&((pane, tab), rect, _is_active)| {
                (
                    (pane, tab),
                    (rect.min.x, rect.min.y, rect.max.x, rect.max.y),
                )
            })
            .collect();
    }

    /// Resolves a drop's target pane/tab into the destination folder,
    /// switching to that tab first if the drop landed on one specifically
    /// (mirrors the old hover-to-open-tab UX, just resolved at drop time
    /// instead of live during the drag).
    fn resolve_drop_dest(&mut self, pane: usize, tab: Option<usize>) -> PathBuf {
        self.active_pane = pane;
        if let Some(tab_idx) = tab {
            if self.panes[pane].active_tab != tab_idx {
                self.panes[pane].active_tab = tab_idx;
                self.dirty = true;
            }
        }
        self.panes[pane].active_tab().path.clone()
    }

    /// Per-frame check for a native drop that landed directly on one of our
    /// own panes/tabs without us having started it — i.e. a genuinely
    /// external drag (Explorer, WinRAR…) dropped straight onto FileMan. A
    /// self-drop (started by `start_native_drag`) is already consumed
    /// there, so this normally only fires for external drops.
    fn process_pending_native_drop(&mut self) {
        let pending = self.dnd_shared.lock().unwrap().pending_drop.take();
        if let Some(drop) = pending {
            let dest = self.resolve_drop_dest(drop.pane, drop.tab);
            let op = if drop.is_move {
                ClipboardOp::Cut
            } else {
                ClipboardOp::Copy
            };
            self.transfer_items(drop.paths, dest, Some(op));
        }
    }

    /// Starts an OS-level drag of `paths` (see `native_drag`'s module docs
    /// for why this must happen the moment the drag gesture starts, not
    /// deferred until the cursor is seen leaving the window). Blocks until
    /// the drop resolves, then handles one of three outcomes: dropped back
    /// onto one of our own panes/tabs (resolved via `dnd_shared`, written by
    /// `native_drag`'s `IDropTarget`), dropped onto another application
    /// (deletes the sources on MOVE, matching Explorer's drag semantics), or
    /// cancelled/failed.
    fn start_native_drag(&mut self, from_pane: usize, paths: Vec<PathBuf>, from_dir: PathBuf) {
        {
            let mut st = self.dnd_shared.lock().unwrap();
            st.own_drag = true;
            st.pending_drop = None;
        }
        let outcome = crate::native_drag::start_drag_out(&paths);
        let pending = {
            let mut st = self.dnd_shared.lock().unwrap();
            st.own_drag = false;
            st.pending_drop.take()
        };

        if let Some(drop) = pending {
            let dest = self.resolve_drop_dest(drop.pane, drop.tab);
            if dest == from_dir {
                self.status = "Source and destination are the same folder".to_string();
            } else {
                let op = if drop.is_move {
                    ClipboardOp::Cut
                } else {
                    ClipboardOp::Copy
                };
                self.clipboard = drop.paths.clone();
                self.clipboard_op = Some(op);
                self.transfer_items(drop.paths, dest, Some(op));
            }
            // The dragged items left the source folder: forget its selection.
            if let Some(src_pane) = self.panes.get_mut(from_pane) {
                src_pane.active_tab_mut().clear_selection();
            }
            return;
        }

        // Not dropped on one of our own panes: it genuinely left to the OS
        // (another application accepted it, or the drag was cancelled).
        match outcome {
            crate::native_drag::DragOutOutcome::Dropped { moved } => {
                if moved {
                    let parents: Vec<PathBuf> = paths
                        .iter()
                        .filter_map(|p| p.parent().map(|x| x.to_path_buf()))
                        .collect();
                    match crate::fs_ops::delete_to_trash(&paths) {
                        Ok(()) => {
                            for dir in parents {
                                self.mark_dir_dirty(&dir);
                            }
                            self.status = format!("Moved {} item(s) out of FileMan", paths.len());
                        }
                        Err(e) => self.status = format!("Drop move failed: {e}"),
                    }
                } else {
                    self.status = format!("Copied {} item(s) to another application", paths.len());
                }
            }
            crate::native_drag::DragOutOutcome::Cancelled => {}
            crate::native_drag::DragOutOutcome::Failed(e) => {
                self.status = format!("Drag failed: {e}");
            }
        }
    }

    fn delete_selection(&mut self) {
        let paths = self.selected_paths();
        if paths.is_empty() {
            self.status = "Nothing selected".into();
            return;
        }
        self.dialog_just_opened = true; self.dialog = Some(Dialog::ConfirmDelete { paths });
    }

    fn begin_rename(&mut self) {
        let tab = self.panes[self.active_pane].active_tab();
        if tab.selected.len() != 1 {
            self.status = "Select exactly one item to rename".into();
            return;
        }
        let name = tab.selected.iter().next().unwrap().clone();
        self.dialog_just_opened = true; self.dialog = Some(Dialog::Rename {
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
                self.panes[self.active_pane]
                    .active_tab_mut()
                    .clear_selection();
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
                    self.panes[self.active_pane]
                        .active_tab_mut()
                        .clear_selection();
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
        // Set only when exactly one folder was created, so it can be
        // selected once the listing refreshes — ambiguous (and skipped) for
        // a multi-name paste into the New Folder box.
        let mut created_folder_name: Option<String> = None;
        let result = match &dialog {
            Dialog::Rename { path, name } => fs_ops::rename_item(path, name)
                .map(|_| format!("Renamed to {name}"))
                .map_err(|err| format!("Rename failed: {err}")),
            Dialog::NewFolder { name } => {
                let names: Vec<&str> = name
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .collect();
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
                        if names.len() == 1 {
                            created_folder_name = Some(names[0].to_string());
                        }
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
            Dialog::RenameTab {
                pane_idx,
                tab_idx,
                name,
            } => {
                if let Some(tab) = self
                    .panes
                    .get_mut(*pane_idx)
                    .and_then(|p| p.tabs.get_mut(*tab_idx))
                {
                    tab.custom_name = if name.trim().is_empty() {
                        None
                    } else {
                        Some(name.trim().to_string())
                    };
                }
                Ok(String::new())
            }
            Dialog::TabContext { .. }
            | Dialog::Find { .. }
            | Dialog::Help
            | Dialog::ConfirmDelete { .. }
            | Dialog::PasteConflict { .. }
            | Dialog::ApplySort { .. } => Ok(String::new()),
        };
        if result.is_ok() {
            dirty_dir = match &dialog {
                Dialog::Rename { path, .. } => path.parent().map(|p| p.to_path_buf()),
                Dialog::NewFolder { .. } | Dialog::NewFile { .. } => Some(parent.clone()),
                Dialog::TabContext { .. }
                | Dialog::Find { .. }
                | Dialog::NewUser { .. }
                | Dialog::Help
                | Dialog::ConfirmDelete { .. }
                | Dialog::PasteConflict { .. }
                | Dialog::RenameTab { .. }
                | Dialog::ApplySort { .. } => None,
            };
        }
        if let Some(dir) = dirty_dir {
            self.mark_dir_dirty(&dir);
        }
        // After a successful rename, select the renamed file so the user
        // can immediately see and interact with it.
        if result.is_ok() {
            if let Dialog::Rename { name, .. } = &dialog {
                let tab = self.panes[self.active_pane].active_tab_mut();
                tab.select_only(name);
                self.last_selected_index = None;
            }
        }
        if let Some(name) = created_folder_name {
            let tab = self.panes[self.active_pane].active_tab_mut();
            tab.select_only(&name);
            self.last_selected_index = None;
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
            egui::RichText::new(
                "Small hints about FileMan's functions, shown near the bottom-left corner.",
            )
            .weak()
            .small(),
        );

        ui.add_space(14.0);
        settings_group_label(ui, "Hidden Files");
        let mut show_hidden = self.show_hidden;
        if ui
            .checkbox(&mut show_hidden, "Show hidden files and folders")
            .changed()
        {
            self.show_hidden = show_hidden;
            for pane in &mut self.panes {
                for tab in &mut pane.tabs {
                    tab.listing_dirty = true;
                }
            }
            let _ = crate::config::set(
                &self.conn,
                crate::config::Scope::User(self.current_user_id),
                "show_hidden",
                if show_hidden { "true" } else { "false" },
            );
        }
        ui.label(
            egui::RichText::new("Items with the Windows \"Hidden\" attribute. Off by default.")
                .weak()
                .small(),
        );

        ui.add_space(14.0);
        settings_group_label(ui, "Windows Explorer Menu");
        ui.label(
            egui::RichText::new(
                "Hide items from the right-click \"Windows Explorer\" submenu by \
                 label, one per line (must match exactly, including case).",
            )
            .weak()
            .small(),
        );
        let resp = ui.add(
            egui::TextEdit::multiline(&mut self.shell_menu_hidden_text)
                .desired_rows(4)
                .desired_width(f32::INFINITY)
                .hint_text("e.g.\nOpen with\nGive access to"),
        );
        if resp.lost_focus() || resp.changed() {
            self.shell_menu_hidden = parse_shell_menu_hidden(&self.shell_menu_hidden_text);
        }
        if resp.lost_focus() {
            let _ = crate::config::set(
                &self.conn,
                crate::config::Scope::User(self.current_user_id),
                "shell_menu_hidden",
                &self.shell_menu_hidden_text,
            );
        }

        ui.add_space(10.0);
        ui.label(
            egui::RichText::new("Changes apply immediately and are remembered per user.")
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
            .num_columns(4)
            .spacing([16.0, 5.0])
            .striped(true)
            .show(ui, |ui| {
                for action in Action::ALL {
                    // An action can hold several combos (e.g. Copy Filename
                    // is F3 and Ctrl+Shift+C); list them all, sorted so the
                    // HashMap's iteration order never flickers the display.
                    let mut bound: Vec<crate::actions::KeyCombo> = self
                        .shortcut_map
                        .iter()
                        .filter(|(_, a)| **a == ActionRef::Builtin(action))
                        .map(|(c, _)| *c)
                        .collect();
                    bound.sort_by_key(|c| c.to_string());
                    let combo_label = if bound.is_empty() {
                        "(none)".to_string()
                    } else {
                        bound
                            .iter()
                            .map(|c| c.to_string())
                            .collect::<Vec<_>>()
                            .join(" / ")
                    };
                    ui.label(action.label());
                    ui.label(egui::RichText::new(&combo_label).weak());
                    let capturing = self.capturing_shortcut_for == Some(action);
                    let rebind_label = if capturing {
                        "Press a key…"
                    } else {
                        "Rebind"
                    };
                    if ui.button(rebind_label).clicked() {
                        self.capturing_shortcut_for = Some(action);
                    }
                    if !bound.is_empty() {
                        if ui.button("Clear").clicked() {
                            for combo in bound {
                                let _ = crate::actions::clear_binding(
                                    &self.conn,
                                    crate::actions::Scope::User(self.current_user_id),
                                    combo,
                                );
                            }
                            self.shortcut_map =
                                crate::actions::load_shortcut_map(&self.conn, self.current_user_id);
                            self.status = format!("Cleared shortcut for {}", action.label());
                        }
                    } else {
                        ui.label("");
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
                let tex = crate::icon_cache::load_icon_texture(ctx, &custom.exe_path);
                self.custom_icons.insert(custom.exe_path.clone(), tex);
            }
            let icon = self.custom_icons.get(&custom.exe_path).cloned().flatten();
            ui.horizontal(|ui| {
                match &icon {
                    Some(tex) => {
                        ui.add(egui::Image::new(egui::load::SizedTexture::new(
                            tex.id(),
                            egui::vec2(20.0, 20.0),
                        )));
                    }
                    None => {
                        ui.label(egui::RichText::new("⚙").weak());
                    }
                }
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(&custom.label).strong());
                    ui.label(egui::RichText::new(&custom.exe_path).weak().small());
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Remove").clicked() {
                        remove = Some(custom.id);
                    }
                });
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
                    [240.0, 0.0],
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
                        ui.label(egui::RichText::new(exe.display().to_string()).weak());
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
                let add_btn = egui::Button::new("Add").fill(ui.visuals().selection.bg_fill);
                if ui.add(add_btn).clicked() {
                    if let Some(exe) = self.new_custom_action_exe.take() {
                        let label = std::mem::take(&mut self.new_custom_action_label);
                        let _ = crate::actions::add_custom_action(
                            &self.conn,
                            self.current_user_id,
                            &label,
                            &exe.display().to_string(),
                        );
                        self.custom_actions =
                            crate::actions::list_custom_actions(&self.conn, self.current_user_id);
                        self.status = format!("Added custom action \"{label}\"");
                    }
                }
            });
        });
    }

    /// Settings page: manage launcher apps that appear as quick-launch
    /// buttons on the toolbar's second row.
    fn settings_page_app_launcher(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        ui.label(
            egui::RichText::new(
                "Configure applications that appear as quick-launch buttons \
                 on the toolbar. Use the search box on the toolbar to filter \
                 and launch any configured app.",
            )
            .weak()
            .small(),
        );
        ui.add_space(10.0);

        let mut remove: Option<i64> = None;
        let mut toggle_show: Option<(i64, bool)> = None;
        for app in self.launcher_apps.clone() {
            if !self.launcher_icons.contains_key(&app.exe_path) {
                let tex = crate::icon_cache::load_icon_texture(ctx, &app.exe_path);
                self.launcher_icons.insert(app.exe_path.clone(), tex);
            }
            let icon = self.launcher_icons.get(&app.exe_path).cloned().flatten();
            ui.horizontal(|ui| {
                match &icon {
                    Some(tex) => {
                        ui.add(egui::Image::new(egui::load::SizedTexture::new(
                            tex.id(),
                            egui::vec2(20.0, 20.0),
                        )));
                    }
                    None => {
                        ui.label(egui::RichText::new("\u{1F680}").weak());
                    }
                }
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(&app.label).strong());
                    let path_display = if app.args.is_empty() {
                        app.exe_path.clone()
                    } else {
                        format!("{} {}", app.exe_path, app.args)
                    };
                    ui.label(egui::RichText::new(path_display).weak().small());
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Remove").clicked() {
                        remove = Some(app.id);
                    }
                    let mut show = app.show_button;
                    if ui.checkbox(&mut show, "Show button").changed() {
                        toggle_show = Some((app.id, show));
                    }
                });
            });
            ui.add_space(2.0);
            ui.separator();
        }
        if let Some((id, show)) = toggle_show {
            let _ = crate::actions::set_launcher_show_button(&self.conn, id, show);
            self.launcher_apps =
                crate::actions::list_launcher_apps(&self.conn, self.current_user_id);
        }
        if let Some(id) = remove {
            let _ = crate::actions::remove_launcher_app(&self.conn, id);
            self.launcher_apps =
                crate::actions::list_launcher_apps(&self.conn, self.current_user_id);
            self.status = "Launcher app removed".into();
        }

        // Add-app form.
        ui.add_space(12.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(egui::RichText::new("Add a launcher app").strong());
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Name").weak());
                ui.add_space(8.0);
                let name_edit = ui.add_sized(
                    [240.0, 0.0],
                    egui::TextEdit::singleline(&mut self.new_launcher_label),
                );
                if name_edit.changed() {
                    self.dirty = true;
                }
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Program").weak());
                ui.add_space(8.0);
                if ui.button("Browse\u{2026}").clicked() {
                    self.new_launcher_exe = rfd::FileDialog::new()
                        .set_title("Choose Executable")
                        .pick_file();
                }
                match &self.new_launcher_exe {
                    Some(exe) => {
                        ui.label(egui::RichText::new(exe.display().to_string()).weak());
                    }
                    None => {
                        ui.label(egui::RichText::new("No program selected").weak());
                    }
                }
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Arguments").weak());
                ui.add_space(8.0);
                ui.add_sized(
                    [240.0, 0.0],
                    egui::TextEdit::singleline(&mut self.new_launcher_args)
                        .hint_text("optional"),
                );
            });
            ui.add_space(8.0);
            let can_add = !self.new_launcher_label.trim().is_empty()
                && self.new_launcher_exe.is_some();
            ui.add_enabled_ui(can_add, |ui| {
                let add_btn = egui::Button::new("Add").fill(ui.visuals().selection.bg_fill);
                if ui.add(add_btn).clicked() {
                    if let Some(exe) = self.new_launcher_exe.take() {
                        let label = std::mem::take(&mut self.new_launcher_label);
                        let args = std::mem::take(&mut self.new_launcher_args);
                        let _ = crate::actions::add_launcher_app(
                            &self.conn,
                            self.current_user_id,
                            &label,
                            &exe.display().to_string(),
                            &args,
                        );
                        self.launcher_apps =
                            crate::actions::list_launcher_apps(&self.conn, self.current_user_id);
                        self.status = format!("Added launcher app \"{label}\"");
                    }
                }
            });
        });
    }

    /// Settings page: manage file launch shortcuts that open a specific
    /// file with the default application for its extension.
    fn settings_page_file_launcher(&mut self, ui: &mut egui::Ui) {
        ui.label(
            egui::RichText::new(
                "Create shortcuts for specific files. Each shortcut opens \
                 the file with the default application associated with its \
                 extension. Buttons appear on the toolbar's second row.",
            )
            .weak()
            .small(),
        );
        ui.add_space(10.0);

        let mut remove: Option<i64> = None;
        let mut toggle_show: Option<(i64, bool)> = None;
        for fl in self.file_launches.clone() {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("\u{1F4C4}").weak());
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(&fl.label).strong());
                    ui.label(egui::RichText::new(&fl.file_path).weak().small());
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Remove").clicked() {
                        remove = Some(fl.id);
                    }
                    let mut show = fl.show_button;
                    if ui.checkbox(&mut show, "Show button").changed() {
                        toggle_show = Some((fl.id, show));
                    }
                });
            });
            ui.add_space(2.0);
            ui.separator();
        }
        if let Some((id, show)) = toggle_show {
            let _ = crate::actions::set_file_launch_show_button(&self.conn, id, show);
            self.file_launches =
                crate::actions::list_file_launches(&self.conn, self.current_user_id);
        }
        if let Some(id) = remove {
            let _ = crate::actions::remove_file_launch(&self.conn, id);
            self.file_launches =
                crate::actions::list_file_launches(&self.conn, self.current_user_id);
            self.status = "File launch shortcut removed".into();
        }

        // Add form.
        ui.add_space(12.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(egui::RichText::new("Add a file launch shortcut").strong());
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Name").weak());
                ui.add_space(8.0);
                let name_edit = ui.add_sized(
                    [240.0, 0.0],
                    egui::TextEdit::singleline(&mut self.new_file_launch_label),
                );
                if name_edit.changed() {
                    self.dirty = true;
                }
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("File").weak());
                ui.add_space(8.0);
                if ui.button("Browse\u{2026}").clicked() {
                    self.new_file_launch_file = rfd::FileDialog::new()
                        .set_title("Choose File")
                        .pick_file();
                }
                match &self.new_file_launch_file {
                    Some(path) => {
                        ui.label(egui::RichText::new(path.display().to_string()).weak());
                    }
                    None => {
                        ui.label(egui::RichText::new("No file selected").weak());
                    }
                }
            });
            ui.add_space(8.0);
            let can_add = !self.new_file_launch_label.trim().is_empty()
                && self.new_file_launch_file.is_some();
            ui.add_enabled_ui(can_add, |ui| {
                let add_btn = egui::Button::new("Add").fill(ui.visuals().selection.bg_fill);
                if ui.add(add_btn).clicked() {
                    let file = self.new_file_launch_file.take().unwrap();
                    let label = std::mem::take(&mut self.new_file_launch_label);
                    let _ = crate::actions::add_file_launch(
                        &self.conn,
                        self.current_user_id,
                        &label,
                        &file.display().to_string(),
                    );
                    self.file_launches =
                        crate::actions::list_file_launches(&self.conn, self.current_user_id);
                    self.status = format!("Added file launch shortcut \"{label}\"");
                }
            });
        });
    }

    /// Settings page: per-extension "always open with" overrides.
    fn settings_page_file_types(&mut self, ui: &mut egui::Ui) {
        let overrides = crate::actions::list_ext_overrides(&self.conn, self.current_user_id);
        let mut remove: Option<String> = None;
        for (ext, exe_path) in &overrides {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(format!(".{ext}")).strong().monospace());
                ui.label(egui::RichText::new(exe_path).weak().small());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Remove").clicked() {
                        remove = Some(ext.clone());
                    }
                });
            });
            ui.add_space(2.0);
            ui.separator();
        }
        if let Some(ext) = remove {
            let _ =
                crate::actions::remove_ext_override(&self.conn, self.current_user_id, &ext);
            self.status = format!("Removed override for .{ext}");
        }

        ui.add_space(12.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(egui::RichText::new("Add an override").strong());
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Extension").weak());
                ui.add_space(8.0);
                ui.add_sized(
                    [80.0, 0.0],
                    egui::TextEdit::singleline(&mut self.new_ext_override_ext)
                        .hint_text("xlsm"),
                );
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Program").weak());
                ui.add_space(8.0);
                if ui.button("Browse…").clicked() {
                    self.new_ext_override_exe = rfd::FileDialog::new()
                        .set_title("Choose Executable")
                        .pick_file();
                }
                match &self.new_ext_override_exe {
                    Some(exe) => {
                        ui.label(egui::RichText::new(exe.display().to_string()).weak());
                    }
                    None => {
                        ui.label(egui::RichText::new("No program selected").weak());
                    }
                }
            });
            ui.add_space(8.0);
            let can_add =
                !self.new_ext_override_ext.trim().is_empty() && self.new_ext_override_exe.is_some();
            ui.add_enabled_ui(can_add, |ui| {
                let add_btn = egui::Button::new("Add").fill(ui.visuals().selection.bg_fill);
                if ui.add(add_btn).clicked() {
                    if let Some(exe) = self.new_ext_override_exe.take() {
                        let ext = std::mem::take(&mut self.new_ext_override_ext);
                        let _ = crate::actions::set_ext_override(
                            &self.conn,
                            self.current_user_id,
                            &ext,
                            &exe.display().to_string(),
                        );
                        self.status = format!("Files ending in \"{ext}\" now always open with this program");
                    }
                }
            });
        });
    }

    /// Settings page: default listing view mode.
    fn settings_page_view_mode(&mut self, ui: &mut egui::Ui) {
        settings_group_label(ui, "Listing Layout");
        ui.label(
            egui::RichText::new("Changes apply to the active tab immediately.")
                .weak()
                .small(),
        );
        ui.add_space(6.0);
        let current_mode = self.panes[self.active_pane].active_tab().view_mode;
        ui.horizontal(|ui| {
            for (label, vm) in [
                ("Details", ViewMode::Details),
                ("List", ViewMode::List),
                ("Icons", ViewMode::Icons),
            ] {
                if ui.selectable_label(current_mode == vm, label).clicked() && current_mode != vm {
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
                        Ok(()) => self.status = format!("Settings exported to {}", path.display()),
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
    }

    /// "About" settings page: version, website, and where to send bug
    /// reports / feature requests. Kept separate from Advanced so it reads
    /// as the app's front door, not a buried footnote.
    fn settings_page_about(&mut self, ui: &mut egui::Ui) {
        ui.add(
            egui::Image::new(egui::include_image!("../docs/FileMan Logo.png"))
                .max_height(64.0)
                .shrink_to_fit(),
        );
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(format!("FileMan v{}", env!("CARGO_PKG_VERSION")))
                .strong()
                .size(self.font_size + 2.0),
        );
        ui.label(
            egui::RichText::new("A fast, keyboard-first dual-pane file explorer for Windows.")
                .weak(),
        );

        ui.add_space(16.0);
        settings_group_label(ui, "Website");
        ui.hyperlink_to("www.speed4ca.com", "https://www.speed4ca.com");

        ui.add_space(16.0);
        settings_group_label(ui, "Support");
        ui.label(
            egui::RichText::new("Found a bug or have a feature request? Email us:")
                .weak()
                .small(),
        );
        ui.hyperlink_to("speed4ca@gmail.com", "mailto:speed4ca@gmail.com");
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
        self.show_hidden = crate::config::get(&self.conn, uid, "show_hidden")
            .map(|raw| raw == "true")
            .unwrap_or(false);
        self.shell_menu_hidden_text =
            crate::config::get(&self.conn, uid, "shell_menu_hidden").unwrap_or_default();
        self.shell_menu_hidden = parse_shell_menu_hidden(&self.shell_menu_hidden_text);
        self.shortcut_map = crate::actions::load_shortcut_map(&self.conn, uid);
        self.toolbar_actions = crate::actions::load_toolbar(&self.conn, uid);
        self.custom_actions = crate::actions::list_custom_actions(&self.conn, uid);
        self.custom_icons.clear();
        self.launcher_apps = crate::actions::list_launcher_apps(&self.conn, uid);
        self.launcher_filter.clear();
        self.launcher_icons.clear();
        self.file_launches = crate::actions::list_file_launches(&self.conn, uid);
        self.file_launch_filter.clear();
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
                            (SettingsPage::AppLauncher, "App Launcher"),
                            (SettingsPage::FileLauncher, "File Launcher"),
                            (SettingsPage::FileTypes, "File Types"),
                            (SettingsPage::ViewMode, "View"),
                            (SettingsPage::Advanced, "Advanced"),
                            (SettingsPage::About, "About"),
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
                    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("settings_content_scroll")
                            .scroll_bar_visibility(ScrollBarVisibility::AlwaysVisible)
                            .auto_shrink(false)
                            .show(ui, |ui| match self.settings_page {
                                SettingsPage::Appearance => {
                                    settings_header(
                                        ui,
                                        "Appearance",
                                        "Personalize how FileMan looks.",
                                    );
                                    self.settings_page_appearance(ui);
                                }
                                SettingsPage::Shortcuts => {
                                    settings_header(
                                        ui,
                                        "Keyboard Shortcuts",
                                        "Customize the key combinations for commands.",
                                    );
                                    self.settings_page_shortcuts(ui);
                                }
                                SettingsPage::Toolbar => {
                                    settings_header(
                                        ui,
                                        "Toolbar",
                                        "Choose which buttons appear on the main row.",
                                    );
                                    self.settings_page_toolbar(ui);
                                }
                                SettingsPage::CustomActions => {
                                    settings_header(
                                        ui,
                                        "Custom Actions",
                                        "Open files with your favourite applications.",
                                    );
                                    self.settings_page_custom_actions(ctx, ui);
                                }
                                SettingsPage::AppLauncher => {
                                    settings_header(
                                        ui,
                                        "App Launcher",
                                        "Quick-launch buttons on the toolbar's second row.",
                                    );
                                    self.settings_page_app_launcher(ctx, ui);
                                }
                                SettingsPage::FileLauncher => {
                                    settings_header(
                                        ui,
                                        "File Launcher",
                                        "Quick-launch shortcuts for specific files on the toolbar.",
                                    );
                                    self.settings_page_file_launcher(ui);
                                }
                                SettingsPage::FileTypes => {
                                    settings_header(
                                        ui,
                                        "File Types",
                                        "Always open a file extension with a specific program, regardless of Windows' current default (useful when another app keeps re-claiming an extension, e.g. macro-enabled Excel files).",
                                    );
                                    self.settings_page_file_types(ui);
                                }
                                SettingsPage::ViewMode => {
                                    settings_header(
                                        ui,
                                        "View",
                                        "Choose the default listing layout.",
                                    );
                                    self.settings_page_view_mode(ui);
                                }
                                SettingsPage::Advanced => {
                                    settings_header(
                                        ui,
                                        "Advanced",
                                        "System integration and application info.",
                                    );
                                    self.settings_page_advanced(ui);
                                }
                                SettingsPage::About => {
                                    settings_header(
                                        ui,
                                        "About",
                                        "Version, website, and support contact.",
                                    );
                                    self.settings_page_about(ui);
                                }
                            });
                    });
                });
            });
        self.show_settings = open;
    }

    fn show_tab_context_menu(&mut self, ctx: &egui::Context) {
        // Keep the dialog alive across frames (no `take`) so the menu stays
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
                    self.open_tab_with_default_sort(pane_idx, path.clone());
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
                    .add_enabled(!locked, egui::Button::new("Close Tab"))
                    .on_disabled_hover_text("Unpin the tab first")
                    .clicked()
                {
                    self.panes[pane_idx].close_tab(tab_idx);
                    self.dirty = true;
                    self.dialog = None;
                }
                if ui
                    .button(if locked { "Unpin Tab" } else { "Pin Tab" })
                    .clicked()
                {
                    self.panes[pane_idx].tabs[tab_idx].locked = !locked;
                    self.dirty = true;
                    self.dialog = None;
                }
                // Renaming is allowed even for pinned tabs — it only
                // changes the label, not the folder.
                if ui.button("Rename Tab").clicked() {
                    self.dialog_just_opened = true; self.dialog = Some(Dialog::RenameTab {
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
    fn show_pane_body(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        pane_idx: usize,
        is_active: bool,
    ) {
        // Record this pane's extent for the drag & drop hit-test pass.
        self.dnd_pane_rects[pane_idx] = Some(ui.max_rect());

        // Click on pane background to set as active
        let pane_resp = ui.interact(
            ui.max_rect(),
            egui::Id::new(("pane_bg", pane_idx)),
            egui::Sense::click(),
        );
        if pane_resp.clicked() {
            self.active_pane = pane_idx;
            self.dirty = true;
        }

        let result = self.show_tab_strip(ui, pane_idx, is_active);

        // Show tab context menu via dialog
        if let Some(idx) = result.context_menu {
            self.tab_menu_pos = result.menu_pos;
            self.dialog_just_opened = true; self.dialog = Some(Dialog::TabContext {
                pane_idx,
                tab_idx: idx,
            });
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
                self.open_tab_with_default_sort(pane_idx, current_path);
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
    fn show_tab_strip(
        &mut self,
        ui: &mut egui::Ui,
        pane_idx: usize,
        is_active: bool,
    ) -> TabStripResult {
        let mut clicked = None;
        let mut closed = None;
        let mut opened = false;
        let mut context_menu = None;
        let mut menu_pos: Option<egui::Pos2> = None;
        let mut hover: Option<(usize, usize)> = None;
        match self.tab_orientation {
            TabOrientation::Horizontal => {
                let mut tab_rects: Vec<((usize, usize), egui::Rect, bool)> = Vec::new();
                let mut reorder_started: Option<usize> = None;
                let drag_highlight = match self.tab_reorder {
                    Some(d) if d.pane_idx == pane_idx => Some(d.idx),
                    _ => None,
                };
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
                            tab.custom_name.is_some(),
                            &mut hover,
                            drag_highlight == Some(tab_idx),
                            None,
                            &tab.path,
                        );
                        tab_rects.push(((pane_idx, tab_idx), ev.rect, is_tab_active));
                        clicked = clicked.or(ev.clicked.then_some(tab_idx));
                        context_menu = context_menu.or(ev.secondary_clicked.then_some(tab_idx));
                        closed = closed.or(ev.close_clicked.then_some(tab_idx));
                        menu_pos = menu_pos.or(ev.secondary_pos);
                        if ev.drag_started {
                            reorder_started = Some(tab_idx);
                        }
                    }
                    if ui.button("+").clicked() {
                        opened = true;
                    }
                });
                self.dnd_tab_rects.extend(tab_rects.clone());
                self.update_tab_reorder(ui, pane_idx, &tab_rects, false, reorder_started);
                TabStripResult {
                    clicked,
                    closed,
                    opened,
                    context_menu,
                    content_rect: None,
                    menu_pos,
                }
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
                let mut reorder_started: Option<usize> = None;
                let drag_highlight = match self.tab_reorder {
                    Some(d) if d.pane_idx == pane_idx => Some(d.idx),
                    _ => None,
                };
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
                            tab.custom_name.is_some(),
                            &mut hover,
                            drag_highlight == Some(tab_idx),
                            Some(egui::vec2(row_w, row_h)),
                            &tab.path,
                        );
                        tab_rects.push(((pane_idx, tab_idx), ev.rect, is_tab_active));
                        clicked = clicked.or(ev.clicked.then_some(tab_idx));
                        context_menu = context_menu.or(ev.secondary_clicked.then_some(tab_idx));
                        closed = closed.or(ev.close_clicked.then_some(tab_idx));
                        menu_pos = menu_pos.or(ev.secondary_pos);
                        if ev.drag_started {
                            reorder_started = Some(tab_idx);
                        }
                    }
                    ui.add_space(2.0);
                    if ui
                        .add_sized([row_w, single_h], egui::Button::new("+"))
                        .clicked()
                    {
                        opened = true;
                    }
                });
                self.dnd_tab_rects.extend(tab_rects.clone());
                self.update_tab_reorder(ui, pane_idx, &tab_rects, true, reorder_started);

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
                    ui.painter()
                        .rect_filled(handle_rect, 3.0, ui.visuals().widgets.active.bg_fill);
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
                TabStripResult {
                    clicked,
                    closed,
                    opened,
                    context_menu,
                    content_rect: Some(content_rect),
                    menu_pos,
                }
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
        // Explorer-style framed address field: a clickable breadcrumb trail
        // by default, switching to a typeable path box (via the folder icon)
        // for manually entering a path.
        //
        // The closure below can't touch `self` directly (it already holds
        // `pane`, a mutable borrow of `self.panes[pane_idx]`), so anything
        // that would normally be `self.foo = ...` is staged into a local and
        // applied to `self` once the closure returns.
        let mut focused_address_pane = self.focused_address_pane;
        let mut deferred_status: Option<String> = None;
        let mut deferred_toast: Option<String> = None;
        let mut deferred_recent: Vec<(PathBuf, bool)> = Vec::new();
        let mut became_active = false;
        let mut became_dirty = false;
        // Disjoint field borrows so the closure below can poll the shared
        // subdirs cache/jobs (breadcrumb dropdowns) while holding `pane`.
        let subdirs_cache = &mut self.tree_subdirs_cache;
        let subdirs_jobs = &mut self.tree_subdirs_jobs;
        egui::Frame::new()
            .fill(ui.visuals().window_fill())
            .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
            .corner_radius(4.0)
            .inner_margin(egui::Margin::same(3))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("📁").on_hover_text("Type a path").clicked() {
                        pane.address_edit_mode = true;
                    }
                    if pane.address_edit_mode {
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
                        // Seed keyboard focus once, then stop asking: egui
                        // evaluates `lost_focus` against the LIVE focus
                        // state, so re-requesting every frame would cancel
                        // the very Enter-surrenders-focus transition the
                        // commit below relies on.
                        if focused_address_pane != Some(pane_idx) {
                            address_resp.request_focus();
                        }
                        // Track which pane's address bar has focus
                        if address_resp.has_focus() {
                            focused_address_pane = Some(pane_idx);
                        } else if focused_address_pane == Some(pane_idx) {
                            focused_address_pane = None;
                        }
                        if address_resp.lost_focus() {
                            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                // Explorer's "Copy as path" wraps the path
                                // in double quotes; accept both forms.
                                // No synchronous existence check here: for a
                                // network path, `Path::exists()` blocks the UI
                                // thread on the OS's connection timeout (can
                                // be tens of seconds). Navigate optimistically
                                // and let the background listing job report
                                // a bad path via `listing_error` instead.
                                let typed = pane.address_bar.trim().trim_matches('"').trim();
                                let target = PathBuf::from(typed);
                                if pane.active_tab_mut().try_navigate(target.clone()) {
                                    became_active = true;
                                    became_dirty = true;
                                    deferred_recent.push((target.clone(), target.is_dir()));
                                } else {
                                    deferred_status =
                                        Some("Tab is pinned — unpin it to navigate".to_string());
                                }
                            }
                            pane.address_edit_mode = false;
                        }
                    } else {
                        let crumbs = Self::path_breadcrumbs(&current_path);
                        // Fit as many trailing crumbs (closest to the current
                        // folder) as the available width allows, so a long
                        // path truncates its earliest ancestors behind an
                        // ellipsis instead of spilling into the next pane.
                        const MIN_STRETCH: f32 = 24.0; // room left for the copy-path click area
                        let font_id = egui::TextStyle::Button.resolve(ui.style());
                        let text_color = ui.visuals().text_color();
                        let btn_padding = ui.spacing().button_padding.x * 2.0;
                        let item_spacing = ui.spacing().item_spacing.x;
                        let measure = |ui: &egui::Ui, s: &str| -> f32 {
                            ui.painter()
                                .layout_no_wrap(s.to_string(), font_id.clone(), text_color)
                                .size()
                                .x
                        };
                        let sep_width = measure(ui, ">") + item_spacing * 2.0;
                        let last = crumbs.len() - 1;
                        // Always show at least the current folder, regardless of budget.
                        let mut budget = ui.available_width()
                            - MIN_STRETCH
                            - (measure(ui, &crumbs[last].0) + btn_padding + item_spacing);
                        let mut first_shown = last;
                        for i in (0..last).rev() {
                            let w =
                                measure(ui, &crumbs[i].0) + btn_padding + item_spacing + sep_width;
                            if w > budget {
                                break;
                            }
                            budget -= w;
                            first_shown = i;
                        }
                        let truncated = first_shown > 0;
                        let last_idx = crumbs.len().saturating_sub(1);
                        let mut nav_target = None;
                        if truncated {
                            ui.label("…").on_hover_text(
                                crumbs[..first_shown]
                                    .iter()
                                    .map(|(l, _)| l.as_str())
                                    .collect::<Vec<_>>()
                                    .join(" > "),
                            );
                            if let Some(target) = Self::crumb_separator_menu(
                                ui,
                                &font_id,
                                subdirs_cache,
                                subdirs_jobs,
                                &crumbs[first_shown - 1].1,
                                Some(crumbs[first_shown].1.as_path()),
                            ) {
                                nav_target = Some(target);
                            }
                        }
                        let mut copy_clicked = false;
                        for (i, (label, full_path)) in crumbs.iter().enumerate().skip(first_shown) {
                            if i == last_idx {
                                // Current folder: not a navigation button
                                // (that would just "navigate" to the
                                // already-current path) — instead it shares
                                // the copy affordance with the empty stretch
                                // to its right, so a click anywhere at the
                                // end of the bar copies the full path.
                                // Painted as plain strong text, but
                                // interactive — a non-interactive label here
                                // made end-of-bar clicks miss whenever the
                                // crumbs filled most of the bar (narrow
                                // panes), which read as erratic copying.
                                let galley = ui.painter().layout_no_wrap(
                                    label.clone(),
                                    font_id.clone(),
                                    ui.visuals().strong_text_color(),
                                );
                                let (rect, resp) = ui.allocate_at_least(
                                    egui::vec2(
                                        galley.size().x,
                                        ui.spacing().interact_size.y,
                                    ),
                                    egui::Sense::click(),
                                );
                                ui.painter().galley(
                                    egui::pos2(
                                        rect.min.x,
                                        rect.center().y - galley.size().y / 2.0,
                                    ),
                                    galley,
                                    ui.visuals().strong_text_color(),
                                );
                                if resp
                                    .on_hover_text("Click to copy the full path")
                                    .clicked()
                                {
                                    copy_clicked = true;
                                }
                            } else if ui.button(label).clicked() {
                                nav_target = Some(full_path.clone());
                            }
                            if i != last_idx
                                && let Some(target) = Self::crumb_separator_menu(
                                    ui,
                                    &font_id,
                                    subdirs_cache,
                                    subdirs_jobs,
                                    full_path,
                                    crumbs.get(i + 1).map(|(_, p)| p.as_path()),
                                )
                            {
                                nav_target = Some(target);
                            }
                        }
                        // The empty space past the last segment copies the
                        // full path too, instead of entering edit mode.
                        let stretch = ui.allocate_response(
                            egui::vec2(ui.available_width().max(8.0), ui.spacing().interact_size.y),
                            egui::Sense::click(),
                        );
                        if stretch
                            .on_hover_text("Click to copy the full path")
                            .clicked()
                        {
                            copy_clicked = true;
                        }
                        if copy_clicked {
                            Self::set_clipboard_text(ctx, &current_path.to_string_lossy());
                            deferred_toast = Some("Path copied to clipboard".to_string());
                        }
                        if let Some(target) = nav_target {
                            if pane.active_tab_mut().try_navigate(target.clone()) {
                                became_active = true;
                                became_dirty = true;
                                deferred_recent.push((target, true));
                            } else {
                                deferred_status =
                                    Some("Tab is pinned — unpin it to navigate".to_string());
                            }
                        }
                    }
                });
            });
        self.focused_address_pane = focused_address_pane;
        if let Some(status) = deferred_status {
            self.status = status;
        }
        if became_active {
            self.active_pane = pane_idx;
        }
        if became_dirty {
            self.dirty = true;
        }
        if let Some(toast) = deferred_toast {
            // Inlined `show_toast`: that method takes `&mut self` as a
            // whole, which would conflict with `pane`'s still-live borrow
            // of `self.panes` for the rest of this function.
            self.status = toast.clone();
            self.last_status = toast.clone();
            self.toast = Some((toast, std::time::Instant::now()));
        }

        ui.horizontal(|ui| {
            if ui.button("⬅").on_hover_text("Back").clicked() {
                if pane.active_tab().locked {
                    self.status = "Tab is pinned — unpin it to navigate".to_string();
                } else if pane.active_tab_mut().go_back() {
                    let path = pane.active_tab().path.clone();
                    deferred_recent.push((path.clone(), path.is_dir()));
                    self.dirty = true;
                }
            }
            if ui.button("➡").on_hover_text("Forward").clicked() {
                if pane.active_tab().locked {
                    self.status = "Tab is pinned — unpin it to navigate".to_string();
                } else if pane.active_tab_mut().go_forward() {
                    let path = pane.active_tab().path.clone();
                    deferred_recent.push((path.clone(), path.is_dir()));
                    self.dirty = true;
                }
            }
            if ui.button("⬆").on_hover_text("Up").clicked() {
                if let Some(parent) = current_path.parent() {
                    let parent_path = parent.to_path_buf();
                    if pane.active_tab_mut().try_navigate(parent_path.clone()) {
                        self.dirty = true;
                        deferred_recent.push((parent_path, true));
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
                if ui
                    .small_button(
                        egui::RichText::new("×").color(egui::Color32::from_rgb(196, 43, 28)),
                    )
                    .on_hover_text("Clear filter")
                    .clicked()
                {
                    tab.filter.clear();
                }
            }
        });

        // Filtering + sorting involves an O(n log n) sort with
        // per-comparison lowercasing allocations — cache it on the tab and
        // only redo the work when the listing/filter/sort actually changed,
        // not on every repaint (blinking cursor, hover, toast fade, ...).
        let (query, sort_col, sort_asc) = {
            let tab = pane.active_tab();
            (tab.filter.clone(), tab.sort_col.clone(), tab.sort_asc)
        };
        let listing_result: Result<Vec<crate::fs_entry::FsEntry>, String> =
            match &pane.active_tab().listing_error {
                Some(err) => Err(err.clone()),
                None => Ok(pane
                    .active_tab_mut()
                    .display_entries(&query, &sort_col, sort_asc)
                    .to_vec()),
            };
        match listing_result {
            Ok(entries) => {
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
                let mut open_targets: Option<Vec<PathBuf>> = None;
                let mut row_action: Option<RowAction> = None;
                let mut drag_start: Option<String> = None;

                // A background listing job is still running and hasn't
                // delivered a single entry yet — show a spinner instead of an
                // empty pane so a slow network folder or a huge directory
                // doesn't read as a frozen UI.
                let loading_empty = entries.is_empty() && self.listing_jobs[pane_idx].is_some();
                if loading_empty {
                    ui.vertical_centered(|ui| {
                        ui.add_space(48.0);
                        ui.add(egui::Spinner::new().size(28.0));
                        ui.add_space(8.0);
                        ui.label(egui::RichText::new("Loading…").weak());
                    });
                    ctx.request_repaint();
                }

                if !loading_empty && entries.is_empty() {
                    ui.vertical_centered(|ui| {
                        ui.add_space(48.0);
                        ui.label(
                            egui::RichText::new("There are no files/folder yet here.").weak(),
                        );
                    });
                }

                if !loading_empty && !entries.is_empty() {
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
                                .scroll_bar_visibility(ScrollBarVisibility::AlwaysVisible)
                                .show(ui, |ui| {
                                    egui_extras::TableBuilder::new(ui)
                                        .id_salt(format!("file_table_pane_{pane_idx}"))
                                        .scroll_bar_visibility(ScrollBarVisibility::AlwaysVisible)
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
                                                sort_header(
                                                    ui,
                                                    "Name",
                                                    "name",
                                                    &sort_col,
                                                    sort_asc,
                                                    &mut sort_clicked,
                                                );
                                            });
                                            header.col(|ui| {
                                                sort_header(
                                                    ui,
                                                    "Modified",
                                                    "modified",
                                                    &sort_col,
                                                    sort_asc,
                                                    &mut sort_clicked,
                                                );
                                            });
                                            header.col(|ui| {
                                                sort_header(
                                                    ui,
                                                    "Size",
                                                    "size",
                                                    &sort_col,
                                                    sort_asc,
                                                    &mut sort_clicked,
                                                );
                                            });
                                            header.col(|ui| {
                                                sort_header(
                                                    ui,
                                                    "Attributes",
                                                    "archive",
                                                    &sort_col,
                                                    sort_asc,
                                                    &mut sort_clicked,
                                                );
                                            });
                                        })
                                        .body(|body| {
                                            live_widths = Some(body.widths().to_vec());
                                            body.rows(row_height, entries.len(), |mut row| {
                                                let entry = &entries[row.index()];
                                                let row_idx = row.index();
                                                let is_selected =
                                                    pane.active_tab().selected.contains(&entry.name);

                                                row.set_selected(is_selected);

                                                row.col(|ui| {
                                                    // Folders keep their emoji glyph;
                                                    // files show the associated app
                                                    // icon for their type, falling
                                                    // back to bare text when none.
                                                    ui.horizontal(|ui| {
                                                        if entry.is_dir {
                                                            ui.label(
                                                                egui::RichText::new("\u{1F4C1}")
                                                                    .color(listing_text),
                                                            );
                                                        } else if let Some(tex) = &entry_icons[row_idx]
                                                        {
                                                            ui.add(egui::Image::new(
                                                                egui::load::SizedTexture::new(
                                                                    tex.id(),
                                                                    egui::vec2(16.0, 16.0),
                                                                ),
                                                            ));
                                                        }
                                                        ui.add(
                                                            egui::Label::new(
                                                                egui::RichText::new(
                                                                    entry.name.as_str(),
                                                                )
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
                                                    ui.label(
                                                        egui::RichText::new(text).color(listing_text),
                                                    );
                                                });
                                                row.col(|ui| {
                                                    let size_text = if entry.is_dir {
                                                        String::new()
                                                    } else {
                                                        format_file_size(entry.size)
                                                    };
                                                    ui.label(
                                                        egui::RichText::new(size_text)
                                                            .color(listing_text),
                                                    );
                                                });
                                                row.col(|ui| {
                                                    let mut attrs = String::new();
                                                    if entry.readonly {
                                                        attrs.push('R');
                                                    }
                                                    if entry.hidden {
                                                        attrs.push('H');
                                                    }
                                                    if entry.system {
                                                        attrs.push('S');
                                                    }
                                                    if entry.archive {
                                                        attrs.push('A');
                                                    }
                                                    ui.label(
                                                        egui::RichText::new(attrs)
                                                            .color(listing_text),
                                                    );
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
                                                        open_targets = Some(vec![entry.path.clone()]);
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
                                                let selection_paths = context_menu_paths(
                                                    pane.active_tab(),
                                                    entry,
                                                    is_selected,
                                                );
                                                styled_context_menu(&row_resp, |ui| {
                                                    show_entry_context_menu(
                                                        ui,
                                                        &mut row_action,
                                                        &entry.path,
                                                        entry.is_dir,
                                                        &selection_paths,
                                                        &self.shell_menu_hidden,
                                                        &mut self.shell_menu_cache,
                                                    );
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
                                // Don't apply yet: the user first chooses whether
                                // this sorting covers every open tab (and future
                                // ones) or just this tab.
                                let tab = pane.active_tab();
                                let (new_col, new_asc) = next_sort(&tab.sort_col, tab.sort_asc, &col);
                                self.dialog_just_opened = true; self.dialog = Some(Dialog::ApplySort {
                                    col: new_col,
                                    asc: new_asc,
                                    pane_idx,
                                });
                            }
                        }
                        ViewMode::List => {
                            egui::ScrollArea::vertical()
                                .id_salt(format!("file_list_pane_{pane_idx}"))
                                .scroll_bar_visibility(ScrollBarVisibility::AlwaysVisible)
                                .show(ui, |ui| {
                                    for (idx, entry) in entries.iter().enumerate() {
                                        let is_selected =
                                            pane.active_tab().selected.contains(&entry.name);
                                        let resp = ui
                                            .horizontal(|ui| {
                                                if entry.is_dir {
                                                    ui.label(
                                                        egui::RichText::new("\u{1F4C1}")
                                                            .color(listing_text),
                                                    );
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
                                            &resp,
                                            entry,
                                            is_selected,
                                            &mut select_name,
                                            &mut select_index,
                                            &mut nav_target,
                                            &mut open_targets,
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
                                        let selection_paths = context_menu_paths(
                                            pane.active_tab(),
                                            entry,
                                            is_selected,
                                        );
                                        styled_context_menu(&resp, |ui| {
                                            show_entry_context_menu(
                                                ui,
                                                &mut row_action,
                                                &entry.path,
                                                entry.is_dir,
                                                &selection_paths,
                                                &self.shell_menu_hidden,
                                                &mut self.shell_menu_cache,
                                            );
                                        });
                                    }
                                });
                        }
                        ViewMode::Icons => {
                            egui::ScrollArea::vertical()
                                .id_salt(format!("file_icons_pane_{pane_idx}"))
                                .scroll_bar_visibility(ScrollBarVisibility::AlwaysVisible)
                                .show(ui, |ui| {
                                    ui.horizontal_wrapped(|ui| {
                                        for (idx, entry) in entries.iter().enumerate() {
                                            let is_selected =
                                                pane.active_tab().selected.contains(&entry.name);
                                            ui.allocate_ui(egui::vec2(76.0, 72.0), |ui| {
                                                // Tile: associated app icon (or the
                                                // generic glyph) above the filename.
                                                // The union of both responses drives
                                                // selection/opening so clicking either
                                                // part works.
                                                let resp = ui
                                                    .vertical_centered(|ui| {
                                                        let img_resp = if entry.is_dir {
                                                            ui.label(
                                                                egui::RichText::new("🗀")
                                                                    .color(listing_text),
                                                            )
                                                        } else if let Some(tex) = &entry_icons[idx] {
                                                            ui.add(egui::Image::new(
                                                                egui::load::SizedTexture::new(
                                                                    tex.id(),
                                                                    egui::vec2(32.0, 32.0),
                                                                ),
                                                            ))
                                                        } else {
                                                            ui.label(
                                                                egui::RichText::new("🗋")
                                                                    .color(listing_text),
                                                            )
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
                                                    &resp,
                                                    entry,
                                                    is_selected,
                                                    &mut select_name,
                                                    &mut select_index,
                                                    &mut nav_target,
                                                    &mut open_targets,
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
                                                let selection_paths = context_menu_paths(
                                                    pane.active_tab(),
                                                    entry,
                                                    is_selected,
                                                );
                                                styled_context_menu(&resp, |ui| {
                                                    show_entry_context_menu(
                                                        ui,
                                                        &mut row_action,
                                                        &entry.path,
                                                        entry.is_dir,
                                                        &selection_paths,
                                                        &self.shell_menu_hidden,
                                                        &mut self.shell_menu_cache,
                                                    );
                                                });
                                            });
                                        }
                                    });
                                });
                        }
                    }
                }

                // A row drag starts a copy/move gesture: make sure the
                // dragged entry is part of the selection, then queue the
                // native OS drag to start once this pane's borrow ends (see
                // `start_native_drag`). Modifiers are ignored at drag START
                // on purpose — Shift+drag means "move" at drop time, not
                // range-select.
                if let Some(name) = drag_start.take() {
                    if !pane.active_tab().selected.contains(&name) {
                        pane.active_tab_mut().select_only(&name);
                    }
                    self.last_selected_index = None;
                    self.active_pane = pane_idx;
                    let tab = pane.active_tab();
                    let paths: Vec<PathBuf> =
                        tab.selected.iter().map(|n| tab.path.join(n)).collect();
                    let from_dir = tab.path.clone();
                    self.pending_native_drag = Some((pane_idx, paths, from_dir));
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
                // Enter opens selected entries, same as a double-click.
                // Directories are navigated into; files are opened with
                // their default associated application.
                if nav_target.is_none()
                    && open_targets.is_none()
                    && pane_idx == self.active_pane
                    && ctx.memory(|m| m.focused()).is_none()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter))
                {
                    let selected = &pane.active_tab().selected;
                    if !selected.is_empty() {
                        let mut dirs: Vec<std::path::PathBuf> = Vec::new();
                        let mut files: Vec<std::path::PathBuf> = Vec::new();
                        for entry in &entries {
                            if selected.contains(&entry.name) {
                                if entry.is_dir {
                                    dirs.push(entry.path.clone());
                                } else {
                                    files.push(entry.path.clone());
                                }
                            }
                        }
                        // If exactly one directory is selected, navigate into it.
                        // Otherwise open all selected files.
                        if dirs.len() == 1 && files.is_empty() {
                            nav_target = Some(dirs.into_iter().next().unwrap());
                        } else if !files.is_empty() {
                            open_targets = Some(files);
                        }
                    }
                }
                // Arrow keys move/extend the single selection through the
                // list (Explorer-style): Up/Down move, Shift+Up/Down extends
                // from a fixed anchor, Left goes up a folder, Right opens
                // the selected folder (mirrors Enter, but directories only).
                if pane_idx == self.active_pane
                    && ctx.memory(|m| m.focused()).is_none()
                    && !entries.is_empty()
                {
                    let arrow_down = ui.input(|i| i.key_pressed(egui::Key::ArrowDown));
                    let arrow_up = ui.input(|i| i.key_pressed(egui::Key::ArrowUp));
                    if arrow_down || arrow_up {
                        let cur = pane
                            .active_tab()
                            .selected
                            .iter()
                            .next()
                            .and_then(|n| entries.iter().position(|e| &e.name == n));
                        let next_idx = match cur {
                            Some(i) if arrow_down => (i + 1).min(entries.len() - 1),
                            Some(i) => i.saturating_sub(1),
                            None => 0,
                        };
                        if shift {
                            let anchor = self.last_selected_index.unwrap_or(cur.unwrap_or(next_idx));
                            let (start, end) = (anchor.min(next_idx), anchor.max(next_idx));
                            let range_names: Vec<String> =
                                entries[start..=end].iter().map(|e| e.name.clone()).collect();
                            pane.active_tab_mut().clear_selection();
                            pane.active_tab_mut().select_range(&range_names);
                            self.last_selected_index = Some(anchor);
                        } else {
                            pane.active_tab_mut().select_only(&entries[next_idx].name);
                            self.last_selected_index = Some(next_idx);
                        }
                        self.active_pane = pane_idx;
                    } else if ui.input(|i| i.key_pressed(egui::Key::ArrowLeft)) {
                        if let Some(parent) = pane.active_tab().path.parent().map(|p| p.to_path_buf())
                            && pane.active_tab_mut().try_navigate(parent.clone())
                        {
                            self.dirty = true;
                            deferred_recent.push((parent, true));
                        }
                    } else if nav_target.is_none() && ui.input(|i| i.key_pressed(egui::Key::ArrowRight)) {
                        let selected = &pane.active_tab().selected;
                        if selected.len() == 1
                            && let Some(entry) = entries.iter().find(|e| selected.contains(&e.name))
                            && entry.is_dir
                        {
                            nav_target = Some(entry.path.clone());
                        }
                    }
                }
                if let Some(target) = nav_target {
                    let pinned = pane.active_tab().locked;
                    if pinned {
                        // A pinned tab never moves: open the folder in a new
                        // tab placed right beside it instead.
                        let (def_col, def_asc) =
                            (self.universal_sort_col.clone(), self.universal_sort_asc);
                        let mut new_tab = crate::tab::Tab::new(target.clone());
                        new_tab.sort_col = def_col;
                        new_tab.sort_asc = def_asc;
                        let insert_at = pane.active_tab + 1;
                        pane.tabs.insert(insert_at, new_tab);
                        pane.active_tab = insert_at;
                        self.active_pane = pane_idx;
                        self.dirty = true;
                        deferred_recent.push((target, true));
                    } else if pane.active_tab_mut().try_navigate(target.clone()) {
                        self.active_pane = pane_idx;
                        self.dirty = true;
                        deferred_recent.push((target, true));
                    }
                }
                if let Some(targets) = open_targets {
                    for target in &targets {
                        deferred_recent.push((target.clone(), false));
                        self.open_path(target);
                    }
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
                            self.dialog_just_opened = true; self.dialog = Some(Dialog::NewFolder {
                                name: String::new(),
                            });
                        }
                        RowAction::NewFile => {
                            self.dialog_just_opened = true; self.dialog = Some(Dialog::NewFile {
                                name: String::new(),
                            });
                        }
                        RowAction::CopyName => self.copy_filename(ctx),
                        RowAction::CopyFolderPath => self.copy_folder_path(ctx),
                        RowAction::ExtractHere => self.extract_here(),
                        RowAction::ExtractTo => self.extract_to(),
                        RowAction::FavouriteFolder(path) => {
                            let path_str = path.display().to_string();
                            if crate::db::is_favourite(&self.conn, self.current_user_id, &path_str)
                            {
                                self.remove_favourite(&path_str);
                            } else {
                                if crate::db::add_favourite(
                                    &self.conn,
                                    self.current_user_id,
                                    &path_str,
                                )
                                .is_ok()
                                {
                                    self.favourites =
                                        crate::db::get_favourites(&self.conn, self.current_user_id);
                                    self.status =
                                        format!("Added to favourites: {}", path.display());
                                }
                            }
                        }
                        RowAction::OpenWith(path) => {
                            // rundll32 shell32.dll,OpenAs_RunDLL silently opens
                            // the default app instead of prompting once a file
                            // type already has an association. OpenWith.exe is
                            // what Explorer's own "Open with" menu item runs
                            // (HKCR\Unknown\shell\openas\command) and always
                            // shows the chooser.
                            let _ = std::process::Command::new("openwith.exe")
                                .arg(&path)
                                .spawn();
                        }
                        RowAction::OpenInExplorer(path) => {
                            let _ = std::process::Command::new("explorer").arg(&path).spawn();
                        }
                        RowAction::Properties(path) => {
                            crate::win_default::show_properties(&path);
                        }
                        RowAction::ShellCommand { id, paths } => {
                            #[cfg(windows)]
                            if let Some(hwnd) = self.hwnd {
                                crate::shell_menu::invoke(hwnd, &paths, id);
                            }
                        }
                    }
                }
            }
            Err(err) => {
                ui.colored_label(egui::Color32::RED, format!("Error: {err}"));
            }
        }
        // Flush deferred recent-item recordings now that `pane`'s borrow is released.
        for (path, is_dir) in deferred_recent {
            self.record_recent(&path, is_dir);
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
        // Taskbar/title-bar text: active folder name first, then the app
        // name, so the folder is what's legible in a crowded taskbar.
        // Windows shows this text directly, so update it in-place rather
        // than only formatting a display string.
        let active_dir = self.active_tab_dir();
        let folder_name = active_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| active_dir.display().to_string());
        let title = format!("{folder_name} - FileMan");
        if title != self.last_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }
        // A tab-reorder drag ends wherever the button is released, not only
        // over the strip it started in.
        if self.tab_reorder.is_some() && !ctx.input(|i| i.pointer.primary_down()) {
            if self.tab_reorder.take().is_some_and(|d| d.moved) {
                self.dirty = true;
            }
        }
        ctx.set_theme(self.theme_pref);
        for theme in [egui::Theme::Dark, egui::Theme::Light] {
            ctx.style_mut_of(theme, |style| {
                // Compact, Windows command-bar density.
                style.spacing.item_spacing = egui::vec2(8.0, 4.0);
                style.spacing.button_padding = egui::vec2(8.0, 4.0);
                style.spacing.menu_margin = egui::Margin::same(4);
                // egui 0.36 defaults to floating scroll bars whose handles fade
                // to zero opacity when the pointer is away, making them look
                // missing. Solid bars are opaque but reserve space at the END
                // OF THE CONTENT, so inside a horizontal scroller the vertical
                // bar ends up past the last column. Floating geometry is the
                // one that pins to the edge of the *visible* pane regardless of
                // content width — so keep it floating, but with a reserved,
                // fully-opaque strip so the handle is always plainly visible.
                // Foreground-colored handles because the themed
                // `inactive.bg_fill` is too close to the track color.
                let mut scroll_style = egui::style::ScrollStyle::thin();
                scroll_style.foreground_color = true;
                scroll_style.floating_width = 5.0;
                scroll_style.floating_allocated_width = 5.0;
                // Fully opaque in every state: translucent handles composite
                // against whatever content happens to be underneath, which
                // makes the vertical and horizontal handles read as different
                // colors.
                scroll_style.dormant_handle_opacity = 1.0;
                scroll_style.active_handle_opacity = 1.0;
                scroll_style.interact_handle_opacity = 1.0;
                scroll_style.dormant_background_opacity = 1.0;
                scroll_style.active_background_opacity = 1.0;
                scroll_style.interact_background_opacity = 1.0;
                style.spacing.scroll = scroll_style;
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
            v.widgets.noninteractive.fg_stroke =
                egui::Stroke::new(1.0, egui::Color32::from_rgb(220, 220, 220));
            v.widgets.inactive.fg_stroke =
                egui::Stroke::new(1.0, egui::Color32::from_rgb(240, 240, 240));
            v.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);
            v.widgets.active.fg_stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);
            v.widgets.open.fg_stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);
            // 3D button treatment: raised resting face with a dark bevel
            // edge, a brighter border + 1px lift on hover, and a sunken
            // darker fill while pressed.
            v.widgets.inactive.bg_fill = egui::Color32::from_rgb(50, 50, 54);
            v.widgets.inactive.weak_bg_fill = egui::Color32::from_rgb(50, 50, 54);
            v.widgets.inactive.bg_stroke =
                egui::Stroke::new(1.0, egui::Color32::from_rgb(16, 16, 18));
            v.widgets.hovered.bg_fill = egui::Color32::from_rgb(60, 60, 66);
            v.widgets.hovered.weak_bg_fill = egui::Color32::from_rgb(60, 60, 66);
            v.widgets.hovered.bg_stroke =
                egui::Stroke::new(1.0, egui::Color32::from_rgb(110, 110, 120));
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
            v.widgets.inactive.bg_stroke =
                egui::Stroke::new(1.0, egui::Color32::from_rgb(173, 173, 178));
            v.widgets.hovered.bg_fill = egui::Color32::WHITE;
            v.widgets.hovered.weak_bg_fill = egui::Color32::WHITE;
            v.widgets.hovered.bg_stroke =
                egui::Stroke::new(1.0, egui::Color32::from_rgb(120, 120, 128));
            v.widgets.hovered.expansion = 1.0;
            v.widgets.active.bg_fill = egui::Color32::from_rgb(222, 222, 226);
            v.widgets.active.weak_bg_fill = egui::Color32::from_rgb(222, 222, 226);
            v.widgets.active.bg_stroke =
                egui::Stroke::new(1.0, egui::Color32::from_rgb(150, 150, 156));
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
            let cancelled = ctx.input(|i| {
                i.events.iter().any(|e| {
                    matches!(
                        e,
                        egui::Event::Key {
                            key: egui::Key::Escape,
                            pressed: true,
                            ..
                        }
                    )
                })
            });
            let combo = ctx.input(|i| {
                // Ctrl+C/X/V never arrive as `Event::Key` — egui-winit
                // converts them into clipboard events (see
                // `clipboard_event_combo`) — so check those first,
                // otherwise those combos could never be (re)bound.
                clipboard_event_combo(i).or_else(|| {
                    i.events.iter().rev().find_map(|e| match e {
                        egui::Event::Key {
                            key,
                            pressed: true,
                            repeat: false,
                            modifiers,
                            ..
                        } => {
                            // `Key::Copy`/`Cut`/`Paste` only arrive from
                            // dedicated hardware keys here — don't let them
                            // shadow the actual key being pressed. Bare
                            // modifier keys (Ctrl/Shift/Alt/Super) fire their
                            // own Key event the instant they're pressed down
                            // — e.g. holding Ctrl before N arrives sends
                            // `Key::ControlLeft` first — so skip those and
                            // keep waiting for the real key.
                            let is_synthetic = matches!(
                                key,
                                egui::Key::Copy
                                    | egui::Key::Cut
                                    | egui::Key::Paste
                                    | egui::Key::ShiftLeft
                                    | egui::Key::ShiftRight
                                    | egui::Key::ControlLeft
                                    | egui::Key::ControlRight
                                    | egui::Key::AltLeft
                                    | egui::Key::AltRight
                                    | egui::Key::SuperLeft
                                    | egui::Key::SuperRight
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
                        self.shortcut_map =
                            crate::actions::load_shortcut_map(&self.conn, self.current_user_id);
                        self.status = format!("Bound {combo} to {}", action.label());
                    }
                    Ok(Some(conflict)) => {
                        self.status = format!(
                            "{combo} is already bound to {}",
                            conflict.label(&self.custom_actions)
                        );
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
                    || id == egui::Id::new("launcher_filter")
                    || id == egui::Id::new("file_launch_filter")
            })
        });
        // '*' jumps straight to the filter box, wherever focus currently is
        // (but not while already typing in some other text field, where '*'
        // should just be typed normally).
        if self.dialog.is_none() && self.capturing_shortcut_for.is_none() && !text_focused {
            // Consume (not just read) the '*' event: this runs before the
            // filter box is drawn this frame, and requesting focus takes
            // effect immediately — if the event were left in the queue, the
            // now-focused TextEdit would see it later this same frame and
            // insert a literal '*'.
            let star_pressed = ctx.input_mut(|i| {
                let before = i.events.len();
                i.events
                    .retain(|e| !matches!(e, egui::Event::Text(t) if t == "*"));
                i.events.len() != before
            });
            if star_pressed {
                ctx.memory_mut(|m| {
                    m.request_focus(egui::Id::new(("filter_input", self.active_pane)))
                });
            }
        }

        if self.dialog.is_none() && self.capturing_shortcut_for.is_none() && !text_focused {
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

        // Another app may have changed these folders while we were in the
        // background, so re-list both panes the moment the window regains
        // focus. Only on the false->true edge, not for every focused frame.
        // ponytail: refresh-on-focus rather than a filesystem watcher — no
        // extra threads or handles, and it covers the case that actually
        // matters (edit a file elsewhere, alt-tab back). Add
        // ReadDirectoryChangesW only if live updates while focused are needed.
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        if focused && !self.was_focused {
            for pane in &mut self.panes {
                pane.active_tab_mut().listing_dirty = true;
            }
        }
        self.was_focused = focused;

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
                    .scroll_bar_visibility(ScrollBarVisibility::AlwaysVisible)
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
                                    if self
                                        .try_navigate_active(self.active_pane, path.to_path_buf())
                                    {
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

                        // System folders (Desktop, Documents, Downloads, …),
                        // resolved through the shell so redirected locations
                        // (e.g. OneDrive) land on their real paths.
                        if !self.system_folders.is_empty() {
                            for (label, path) in self.system_folders.clone() {
                                let icon = match label.as_str() {
                                    "Desktop" => "🖥",
                                    "Documents" => "🗋",
                                    "Downloads" => "📥",
                                    "Music" => "🎵",
                                    "Pictures" => "🖼",
                                    _ => "🎬",
                                };
                                let display = format!("{icon} {label}");
                                self.show_dir_node(
                                    ui,
                                    &path,
                                    Some(&display),
                                    &active_path,
                                    force_expand,
                                );
                            }
                            ui.separator();
                        }

                        for drive in self.drives.clone() {
                            self.show_dir_node(ui, &drive, None, &active_path, force_expand);
                        }
                        let mut network_roots = self.network_servers.clone();
                        if let Some(active_unc_root) = tree::unc_share_root(&active_path) {
                            let already_covered = network_roots.iter().any(|r| {
                                r.to_string_lossy().to_lowercase()
                                    == active_unc_root.to_string_lossy().to_lowercase()
                            });
                            if !already_covered {
                                network_roots.push(active_unc_root);
                            }
                        }
                        if !network_roots.is_empty() {
                            ui.separator();
                            ui.label(egui::RichText::new("Network").strong());
                            for server in &network_roots {
                                self.show_dir_node(ui, server, None, &active_path, force_expand);
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
                self.tree_width =
                    (self.tree_width + divider_resp.drag_delta().x).clamp(tree_min_w, tree_max_w);
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

                // 🕒 Recent button — always the first toolbar button.
                let recent_btn = toolbar_button(
                    ui,
                    "🕒 Recent".to_string(),
                    None,
                    ButtonStyle::Blue,
                );
                if recent_btn.clicked() {
                    self.show_recent_popup = !self.show_recent_popup;
                }
                let popup_id = recent_btn.id.with("recent_popup");
                if self.show_recent_popup {
                    let recent = self.recent_items.clone();
                    let folders: Vec<_> = recent.iter().filter(|i| i.is_dir).take(15).collect();
                    let files: Vec<_> = recent.iter().filter(|i| !i.is_dir).take(15).collect();
                    let area_resp = egui::Area::new(popup_id)
                        .fixed_pos(recent_btn.rect.left_bottom() + egui::vec2(0.0, 4.0))
                        .order(egui::Order::Foreground)
                        .interactable(true)
                        .show(ui.ctx(), |ui| {
                            egui::Frame::popup(ui.style()).show(ui, |ui| {
                                ui.set_min_width(260.0);
                                if folders.is_empty() && files.is_empty() {
                                    ui.label(egui::RichText::new("No recent items").weak());
                                } else {
                                    if !folders.is_empty() {
                                        ui.label(egui::RichText::new("Folders").strong());
                                        for item in &folders {
                                            let path = std::path::Path::new(&item.path);
                                            let name = path
                                                .file_name()
                                                .map(|n| n.to_string_lossy().into_owned())
                                                .unwrap_or_else(|| item.path.clone());
                                            let btn = ui.button(format!("\u{1F4C1} {name}"));
                                            if btn.clicked() {
                                                if self.try_navigate_active(self.active_pane, path.to_path_buf()) {
                                                    self.dirty = true;
                                                }
                                                self.show_recent_popup = false;
                                            }
                                        }
                                    }
                                    if !folders.is_empty() && !files.is_empty() {
                                        ui.separator();
                                    }
                                    if !files.is_empty() {
                                        ui.label(egui::RichText::new("Files").strong());
                                        for item in &files {
                                            let path = std::path::Path::new(&item.path);
                                            let name = path
                                                .file_name()
                                                .map(|n| n.to_string_lossy().into_owned())
                                                .unwrap_or_else(|| item.path.clone());
                                            let btn = ui.button(format!("\u{1F4C4} {name}"));
                                            if btn.clicked() {
                                                self.record_recent(path, false);
                                                self.open_path(path);
                                                self.show_recent_popup = false;
                                            }
                                        }
                                    }
                                    ui.separator();
                                    if ui.button("Clear Recent").clicked() {
                                        self.clear_recent();
                                        self.show_recent_popup = false;
                                    }
                                }
                            });
                        });
                    // Close on click-away: any click that is NOT inside the
                    // popup area and NOT on the Recent button itself.
                    let pointer = ui.input(|i| i.pointer.clone());
                    if pointer.any_click() && !area_resp.response.rect.contains(pointer.interact_pos().unwrap_or_default()) && !recent_btn.rect.contains(pointer.interact_pos().unwrap_or_default()) {
                        self.show_recent_popup = false;
                    }
                }

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
                                Action::CopyFilename => ("Copy Filename", "Copy full path of selected file (Ctrl+Shift+C)", true),
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
                                Action::SelectAll => ("Select All", "Select all items in the view (Ctrl+A)", true),
                                _ => continue,
                            };
                            ui.add_enabled_ui(enabled, |ui| {
                                if toolbar_button(ui, label.to_owned(), None, ButtonStyle::Blue)
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
                        self.dialog_just_opened = true; self.dialog = Some(Dialog::Help);
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
                        self.dialog_just_opened = true; self.dialog = Some(Dialog::NewUser { name: String::new() });
                    }
                });
            });

            // Second toolbar line: app launcher search + pinned launch
            // buttons, file launch shortcuts, and custom "open with" action buttons.
            let has_launcher = !self.launcher_apps.is_empty();
            let has_file_launch = !self.file_launches.is_empty();
            let has_custom = !self.custom_actions.is_empty();
            if has_launcher || has_file_launch || has_custom {
                ui.add_space(2.0);
                // `horizontal` already center-aligns cross-axis (unlike
                // `with_layout`, which would claim the panel's full
                // remaining height for this row instead of just one line).
                ui.horizontal(|ui| {
                    // Search filter for launcher apps (left side).
                    let mut launch_app: Option<i64> = None;
                    if has_launcher {
                        let filter_edit = ui.add(
                            egui::TextEdit::singleline(&mut self.launcher_filter)
                                .id(egui::Id::new("launcher_filter"))
                                .hint_text("\u{26A1} Search apps...")
                                .desired_width(140.0)
                                .min_size(egui::vec2(140.0, TOOLBAR_ROW2_HEIGHT)),
                        );
                        if filter_edit.changed() {
                            self.dirty = true;
                        }

                        // Dropdown: show all matching apps when filter has text.
                        let filter_lower = self.launcher_filter.to_lowercase();
                        if !filter_lower.is_empty() {
                            let matches: Vec<_> = self
                                .launcher_apps
                                .iter()
                                .filter(|a| {
                                    a.label.to_lowercase().contains(&filter_lower)
                                        || a.exe_path.to_lowercase().contains(&filter_lower)
                                })
                                .cloned()
                                .collect();
                            if !matches.is_empty() {
                                egui::Popup::from_response(&filter_edit)
                                    .open(true)
                                    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                                    .show(|ui| {
                                        ui.set_min_width(180.0);
                                        for app in &matches {
                                            if ui
                                                .button(format!("\u{26A1} {}", app.label))
                                                .on_hover_text(&app.exe_path)
                                                .clicked()
                                            {
                                                launch_app = Some(app.id);
                                            }
                                        }
                                    });
                            }
                        }

                        ui.separator();
                    }

                    // Pinned launcher buttons (filtered by search text, only
                    // those with show_button enabled).
                    let filter_lower = self.launcher_filter.to_lowercase();
                    for app in &self.launcher_apps {
                        if !app.show_button {
                            continue;
                        }
                        let matches = filter_lower.is_empty()
                            || app.label.to_lowercase().contains(&filter_lower);
                        if !matches {
                            continue;
                        }
                        if !self.launcher_icons.contains_key(&app.exe_path) {
                            let tex =
                                crate::icon_cache::load_icon_texture(&ctx.clone(), &app.exe_path);
                            self.launcher_icons.insert(app.exe_path.clone(), tex);
                        }
                        let icon = self.launcher_icons.get(&app.exe_path).cloned().flatten();
                        let label = format!("\u{26A1} {}", app.label);
                        if toolbar_button(ui, label, icon.as_ref(), ButtonStyle::Blue)
                            .on_hover_text(format!("Launch {}", app.exe_path))
                            .clicked()
                        {
                            launch_app = Some(app.id);
                        }
                    }
                    if let Some(id) = launch_app {
                        if let Some(app) = self.launcher_apps.iter().find(|a| a.id == id) {
                            let exe = app.exe_path.clone();
                            let args = app.args.clone();
                            let label = app.label.clone();
                            let _ = std::process::Command::new(&exe)
                                .args(args.split_whitespace())
                                .spawn();
                            self.status = format!("Launched {label}");
                            self.launcher_filter.clear();
                        }
                    }

                    // File launch shortcut buttons with search filter.
                    let mut launch_file: Option<i64> = None;
                    if has_file_launch && has_launcher {
                        ui.separator();
                    }
                    if has_file_launch {
                        let fl_filter_edit = ui.add(
                            egui::TextEdit::singleline(&mut self.file_launch_filter)
                                .id(egui::Id::new("file_launch_filter"))
                                .hint_text("\u{1F4C4} Search files...")
                                .desired_width(140.0)
                                .min_size(egui::vec2(140.0, TOOLBAR_ROW2_HEIGHT)),
                        );
                        if fl_filter_edit.changed() {
                            self.dirty = true;
                        }

                        // Dropdown: show all matching file launches when filter has text.
                        let fl_filter_lower = self.file_launch_filter.to_lowercase();
                        if !fl_filter_lower.is_empty() {
                            let matches: Vec<_> = self
                                .file_launches
                                .iter()
                                .filter(|fl| {
                                    fl.label.to_lowercase().contains(&fl_filter_lower)
                                        || fl.file_path.to_lowercase().contains(&fl_filter_lower)
                                })
                                .cloned()
                                .collect();
                            if !matches.is_empty() {
                                egui::Popup::from_response(&fl_filter_edit)
                                    .open(true)
                                    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                                    .show(|ui| {
                                        ui.set_min_width(180.0);
                                        for fl in &matches {
                                            if ui
                                                .button(format!("\u{1F4C4} {}", fl.label))
                                                .on_hover_text(&fl.file_path)
                                                .clicked()
                                            {
                                                launch_file = Some(fl.id);
                                            }
                                        }
                                    });
                            }
                        }

                        ui.separator();
                    }
                    let fl_filter_lower = self.file_launch_filter.to_lowercase();
                    for fl in &self.file_launches {
                        if !fl.show_button {
                            continue;
                        }
                        let matches = fl_filter_lower.is_empty()
                            || fl.label.to_lowercase().contains(&fl_filter_lower);
                        if !matches {
                            continue;
                        }
                        let label = format!("\u{1F4C4} {}", fl.label);
                        if toolbar_button(ui, label, None, ButtonStyle::Blue)
                            .on_hover_text(format!("Open {}", fl.file_path))
                            .clicked()
                        {
                            launch_file = Some(fl.id);
                        }
                    }
                    if let Some(id) = launch_file {
                        if let Some(fl) = self.file_launches.iter().find(|f| f.id == id) {
                            let file = fl.file_path.clone();
                            let label = fl.label.clone();
                            let _ = std::process::Command::new("cmd")
                                .args(["/C", "start", "", &file])
                                .spawn();
                            self.status = format!("Opened {label}");
                            self.file_launch_filter.clear();
                        }
                    }

                    // Custom "open with" action buttons (existing behavior).
                    if has_custom && (has_launcher || has_file_launch) {
                        ui.separator();
                    }
                    let mut launch_custom: Option<i64> = None;
                    for custom in &self.custom_actions {
                        if !self.custom_icons.contains_key(&custom.exe_path) {
                            let tex =
                                crate::icon_cache::load_icon_texture(&ctx.clone(), &custom.exe_path);
                            self.custom_icons.insert(custom.exe_path.clone(), tex);
                        }
                        let icon = self.custom_icons.get(&custom.exe_path).cloned().flatten();
                        let label = format!("\u{1F50D} {}", custom.label);
                        if toolbar_button(ui, label, icon.as_ref(), ButtonStyle::Green)
                            .on_hover_text(format!(
                                "Open the selection with {}",
                                custom.exe_path
                            ))
                            .clicked()
                        {
                            launch_custom = Some(custom.id);
                        }
                    }
                    if let Some(id) = launch_custom {
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

            // Background operations: show a progress bar while running,
            // then dismiss immediately and surface the result as a toast.
            let mut dismiss_op = false;
            let mut newly_finished_dirs: Vec<PathBuf> = Vec::new();
            if let Some(ref mut op) = self.background_op {
                let still_running = op.poll();
                if !still_running {
                    newly_finished_dirs = std::mem::take(&mut self.background_op_dirs);
                    self.dirty = true;
                    match &op.status {
                        OpStatus::Completed(msg) => {
                            self.status = msg.clone();
                        }
                        OpStatus::Failed(msg) => {
                            self.status = format!("Error: {msg}");
                        }
                        _ => {}
                    }
                    dismiss_op = true;
                } else {
                    // Show a small floating progress bar while the op runs.
                    let progress_text = format!(
                        "{}/{} files — {}",
                        op.progress.files_done, op.progress.files_total, op.progress.current_file
                    );
                    let fraction = if op.progress.files_total > 0 {
                        op.progress.files_done as f32 / op.progress.files_total as f32
                    } else {
                        0.0
                    };
                    let font = egui::FontId::proportional(self.font_size);
                    let painter = ctx.layer_painter(egui::LayerId::new(
                        egui::Order::Foreground,
                        egui::Id::new("op_progress"),
                    ));
                    let galley = painter.layout_no_wrap(progress_text, font, egui::Color32::WHITE);
                    let pad = 8.0;
                    let bar_w = 240.0;
                    let row_h = galley.size().y + pad * 2.0;
                    let screen = ctx.input(|i| i.viewport_rect());
                    let pos = egui::pos2(
                        (screen.center().x - bar_w / 2.0).max(screen.left() + 8.0),
                        screen.top() + 14.0,
                    );
                    let bg = egui::Color32::from_rgba_premultiplied(40, 40, 40, 230);
                    let bar_bg = egui::Color32::from_rgb(70, 70, 70);
                    let bar_fill = egui::Color32::from_rgb(100, 160, 255);
                    // Background rect
                    painter.rect_filled(
                        egui::Rect::from_min_size(pos, egui::vec2(bar_w, row_h)),
                        6.0,
                        bg,
                    );
                    // Label
                    painter.galley(
                        egui::pos2(pos.x + pad, pos.y + pad),
                        galley,
                        egui::Color32::WHITE,
                    );
                    // Progress bar track
                    let bar_y = pos.y + row_h - pad - 4.0;
                    let bar_rect = egui::Rect::from_min_size(
                        egui::pos2(pos.x + pad, bar_y),
                        egui::vec2(bar_w - pad * 2.0, 4.0),
                    );
                    painter.rect_filled(bar_rect, 2.0, bar_bg);
                    // Progress bar fill
                    let fill_w = (bar_w - pad * 2.0) * fraction;
                    if fill_w > 0.0 {
                        painter.rect_filled(
                            egui::Rect::from_min_size(
                                bar_rect.min,
                                egui::vec2(fill_w, 4.0),
                            ),
                            2.0,
                            bar_fill,
                        );
                    }
                    ctx.request_repaint();
                }
            }
            for dir in &newly_finished_dirs {
                self.mark_dir_dirty(dir);
            }
            if dismiss_op {
                self.background_op = None;
                self.dirty = true;
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
                    query_focused,
                }) = &mut self.dialog
                {
                    let search_path_clone = search_path.clone();
                    let searching = self.find_job.is_some();
                    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                        find_close = true;
                    }
                    // Show the dialog UI, capturing any actions needed
                    let mut dialog_ui = |ui: &mut egui::Ui,
                                         query: &mut String,
                                         results: &mut Vec<crate::fs_entry::FsEntry>,
                                         sort_col: &mut String,
                                         sort_asc: &mut bool,
                                         name_filter: &mut String,
                                         folder_filter: &mut String,
                                         include_folders: &mut bool,
                                         query_focused: &mut bool| {
                        ui.horizontal(|ui| {
                            ui.label("Search in:");
                            ui.label(search_path_clone.display().to_string())
                                .on_hover_text(search_path_clone.display().to_string());
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let close_resp = ui.add(
                                    egui::Button::new(
                                        egui::RichText::new("Close").color(egui::Color32::BLACK),
                                    )
                                    .fill(egui::Color32::from_rgb(240, 140, 140)),
                                );
                                if close_resp.clicked() {
                                    find_close = true;
                                }
                            });
                        });
                        ui.horizontal(|ui| {
                            ui.label("Find:");
                            let edit = ui.text_edit_singleline(query);
                            if !*query_focused {
                                edit.request_focus();
                                *query_focused = true;
                            }
                            let enter_pressed =
                                edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            let search_clicked = ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new("Search").color(egui::Color32::BLACK),
                                    )
                                    .fill(egui::Color32::from_rgb(140, 200, 240)),
                                )
                                .clicked();
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
                            .scroll_bar_visibility(ScrollBarVisibility::AlwaysVisible)
                            .striped(true)
                            .resizable(true)
                            .sense(egui::Sense::click())
                            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                            .vscroll(true)
                            .max_scroll_height(320.0)
                            .column(egui_extras::Column::initial(420.0).at_least(200.0).resizable(true).clip(true))
                            .column(egui_extras::Column::remainder().clip(true))
                            .column(egui_extras::Column::initial(150.0).at_least(150.0).resizable(true).clip(false))
                            .column(egui_extras::Column::initial(110.0).at_least(90.0).resizable(true).clip(true))
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
                    let avail = ctx.input(|i| i.viewport_rect());
                    let default_size = egui::vec2(
                        (avail.width() * 0.75).clamp(760.0, 1200.0),
                        (avail.height() * 0.7).clamp(420.0, 720.0),
                    );
                    egui::Window::new("Find Files")
                        .resizable(true)
                        .default_size(default_size)
                        .min_width(760.0)
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
                                query_focused,
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
                            self.record_recent(&path, false);
                            self.open_path(&path);
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
                                .scroll_bar_visibility(ScrollBarVisibility::AlwaysVisible)
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
                    // Network shares have no Recycle Bin: those items will
                    // be deleted permanently, so say so up front.
                    let has_network_items =
                        if let Some(Dialog::ConfirmDelete { paths }) = &self.dialog {
                            paths.iter().any(|p| crate::fs_ops::is_network_path(p))
                        } else {
                            false
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
                            if has_network_items {
                                ui.add_space(4.0);
                                ui.label(
                                    egui::RichText::new(
                                        "Network items cannot go to the Recycle Bin \
                                         and will be deleted permanently.",
                                    )
                                    .weak(),
                                );
                            }
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
                let is_apply_sort = matches!(&self.dialog, Some(Dialog::ApplySort { .. }));
                if is_apply_sort {
                    let mut apply_all = false;
                    let mut apply_one = false;
                    let mut close = false;
                    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                        close = true;
                    }
                    let Some(Dialog::ApplySort { col, asc, pane_idx }) = &self.dialog else {
                        unreachable!()
                    };
                    let new_col_label = sort_col_label(col);
                    let new_dir = if *asc { "ascending" } else { "descending" };
                    // Spell out the exact change: what the clicked tab is
                    // sorted by right now vs the candidate, and how many of
                    // the open tabs each scope would actually touch.
                    let origin_tab = self.panes[*pane_idx].active_tab();
                    let old_col_label = sort_col_label(&origin_tab.sort_col);
                    let old_dir =
                        if origin_tab.sort_asc { "ascending" } else { "descending" };
                    let origin_name = origin_tab.display_label();
                    let mut total_tabs = 0usize;
                    let mut changed_tabs = 0usize;
                    for p in &self.panes {
                        for t in &p.tabs {
                            total_tabs += 1;
                            if t.sort_col != *col || t.sort_asc != *asc {
                                changed_tabs += 1;
                            }
                        }
                    }
                    egui::Window::new("Sorting")
                        .id(egui::Id::new("apply_sort_window"))
                        .title_bar(true)
                        .resizable(false)
                        .collapsible(false)
                        .fixed_size(egui::vec2(400.0, 0.0))
                        // Modal-style placement: pinned to the screen centre
                        // rather than egui's cascading default position.
                        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                        .show(&ctx, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new("Current sorting:").weak());
                                ui.label(format!("{old_col_label} {old_dir}"));
                            });
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new("New sorting:").weak());
                                ui.label(
                                    egui::RichText::new(format!("{new_col_label} {new_dir}"))
                                        .strong(),
                                );
                            });
                            ui.add_space(6.0);
                            ui.label(format!(
                                "\u{2022} All open tabs — re-sorts all {total_tabs} tabs across \
                                 both panes ({changed_tabs} will actually change) and becomes \
                                 the sorting every new tab starts with."
                            ));
                            ui.add_space(4.0);
                            ui.label(format!(
                                "\u{2022} This tab only — re-sorts just \"{origin_name}\"; other \
                                 tabs and the default for future tabs stay unchanged."
                            ));
                            ui.add_space(8.0);
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button("Cancel").clicked() {
                                    close = true;
                                }
                                if ui.button("This tab only").clicked() {
                                    apply_one = true;
                                }
                                let all_resp = ui.button("All open tabs");
                                if all_resp.clicked() {
                                    apply_all = true;
                                }
                                // Default focus on the broadest choice so
                                // Enter applies everywhere immediately (Esc
                                // still cancels).
                                if ctx.memory(|m| m.focused().is_none()) {
                                    all_resp.request_focus();
                                }
                            });
                        });
                    if apply_all || apply_one {
                        if let Some(Dialog::ApplySort { col, asc, pane_idx }) = self.dialog.take() {
                            if apply_all {
                                self.apply_sort_everywhere(&col, asc);
                                self.status = format!(
                                    "Sorted all tabs by {} {} — new tabs will use it too",
                                    sort_col_label(&col),
                                    if asc { "ascending" } else { "descending" }
                                );
                            } else {
                                // This tab only: the universal default stays
                                // untouched, the tab keeps its own sorting
                                // across navigation and restarts. Targets the
                                // tab whose header was clicked, not whichever
                                // pane happens to be active now.
                                let tab = self.panes[pane_idx].active_tab_mut();
                                tab.sort_col = col.clone();
                                tab.sort_asc = asc;
                                self.status = format!(
                                    "Sorted this tab by {} {}",
                                    sort_col_label(&col),
                                    if asc { "ascending" } else { "descending" }
                                );
                            }
                            self.dirty = true;
                        }
                    } else if close {
                        self.dialog = None;
                    }
                }
                let is_paste_conflict =
                    matches!(&self.dialog, Some(Dialog::PasteConflict { .. }));
                if is_paste_conflict {
                    let mut cancel = false;
                    // (overwrite, apply-to-all) once the user picks a side.
                    let mut choice: Option<(bool, bool)> = None;
                    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                        cancel = true;
                    }
                    let (dest_dir, op, conflict_count, first_name) =
                        if let Some(Dialog::PasteConflict {
                            dest_dir,
                            op,
                            conflicts,
                            ..
                        }) = &self.dialog
                        {
                            let first = conflicts
                                .first()
                                .and_then(|p| p.file_name())
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            (dest_dir.clone(), *op, conflicts.len(), first)
                        } else {
                            unreachable!()
                        };
                    let verb = if op == Some(ClipboardOp::Cut) {
                        "moving"
                    } else {
                        "copying"
                    };
                    egui::Window::new("Name Conflict")
                        .id(egui::Id::new("paste_conflict_window"))
                        .title_bar(true)
                        .resizable(false)
                        .collapsible(false)
                        .fixed_size(egui::vec2(470.0, 0.0))
                        // Modal-style placement: pinned to the screen centre
                        // rather than egui's cascading default position.
                        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                        .show(&ctx, |ui| {
                            if conflict_count > 1 {
                                ui.label(format!(
                                    "{conflict_count} of the items you're {verb} already \
                                     exist in \"{}\" with the same names.",
                                    dest_dir.display()
                                ));
                                ui.add_space(4.0);
                                ui.label(format!(
                                    "This one: \"{first_name}\" — replace the existing \
                                     item, or keep both?"
                                ));
                                ui.add_space(6.0);
                                ui.label(
                                    egui::RichText::new(format!(
                                        "Tip: hold Shift while clicking a button to apply \
                                         your choice to all {conflict_count} conflicting \
                                         items at once."
                                    ))
                                    .weak(),
                                );
                            } else {
                                ui.label(format!(
                                    "\"{first_name}\" already exists in \"{}\".",
                                    dest_dir.display()
                                ));
                                ui.add_space(4.0);
                                ui.label(
                                    "Do you want to replace the existing item, or keep \
                                     both by saving the incoming one as a copy?",
                                );
                            }
                            ui.add_space(8.0);
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button("Cancel").clicked() {
                                    cancel = true;
                                }
                                // Read Shift at click time so "hold Shift for
                                // all items" works with either button.
                                let shift = ui.input(|i| i.modifiers.shift);
                                let copy_resp = ui
                                    .button("Save as Copy")
                                    .on_hover_text(
                                        "Keep both: the incoming item is renamed \
                                         (e.g. \"name (copy).ext\")",
                                    );
                                if copy_resp.clicked() {
                                    choice = Some((false, shift));
                                }
                                let overwrite_resp = ui
                                    .button("Overwrite")
                                    .on_hover_text("Replace the existing item");
                                if overwrite_resp.clicked() {
                                    choice = Some((true, shift));
                                }
                                // Default focus on the affirmative button so
                                // Enter confirms immediately (Esc still
                                // cancels). Seed it only while nothing holds
                                // focus, so Tab away stays respected.
                                if ctx.memory(|m| m.focused().is_none()) {
                                    overwrite_resp.request_focus();
                                }
                            });
                        });
                    if cancel {
                        self.dialog = None;
                        self.status = "Transfer cancelled — nothing was changed".to_string();
                    } else if let Some((overwrite, apply_all)) = choice {
                        self.resolve_paste_conflict(overwrite, apply_all);
                        self.dirty = true;
                    }
                }
                let is_find = matches!(&self.dialog, Some(Dialog::Find { .. }));
                if self.dialog.is_some()
                    && !find_close
                    && !is_find
                    && !is_help
                    && !is_confirm_delete
                    && !is_paste_conflict
                    && !is_apply_sort
                {
                    let mut commit = false;
                    let mut cancel = false;
                    if let Some(dialog) = &mut self.dialog {
                        let multiline = matches!(dialog, Dialog::NewFolder { .. });
                        let (title, name) = match dialog {
                            Dialog::Rename { name, .. } => ("Rename", name),
                            Dialog::NewFolder { name } => ("New Folder", name),
                            Dialog::NewFile { name } => ("New File", name),
                            Dialog::NewUser { name } => ("New User", name),
                            Dialog::RenameTab { name, .. } => ("Rename Tab", name),
                            Dialog::Find { .. } | Dialog::TabContext { .. } | Dialog::Help
                            | Dialog::ConfirmDelete { .. } | Dialog::PasteConflict { .. }
                            | Dialog::ApplySort { .. } => {
                                unreachable!()
                            }
                        };
                        egui::Window::new(title)
                            // Modal-style placement: pinned to screen centre.
                            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                            // Windows are fixed-size (Resize-backed) unless
                            // told otherwise, so a longer name on reopen was
                            // clipped to whatever size the dialog last had.
                            .auto_sized()
                            .show(&ctx, |ui| {
                                if multiline {
                                    ui.label("One folder per line:");
                                }
                                let is_rename = title == "Rename";
                                let just_opened = self.dialog_just_opened;
                                let dialog_text_id = egui::Id::new("dialog_rename_text");
                                let edit = if multiline {
                                    ui.add(
                                        egui::TextEdit::multiline(name)
                                            .id(dialog_text_id)
                                            .desired_rows(4)
                                            .desired_width(260.0),
                                    )
                                } else {
                                    // Auto-size the rename input: measure text
                                    // width via the painter and clamp to
                                    // 80% of available width so long
                                    // filenames (with extension) fit.
                                    let font_id = egui::TextStyle::Body.resolve(ui.style());
                                    let text_width = ui.painter().layout_no_wrap(
                                        name.clone(),
                                        font_id,
                                        egui::Color32::WHITE,
                                    ).size().x;
                                    // Fixed generous ceiling, not derived from
                                    // `ui.available_width()`: inside a
                                    // freshly (re)opened auto-sized Window,
                                    // that reflects the *previous* frame's
                                    // remembered size, so it stayed clamped
                                    // to whatever a shorter name last used.
                                    let desired = (text_width + 80.0).clamp(260.0, 1000.0);
                                    let mut output = egui::TextEdit::singleline(name)
                                        .id(dialog_text_id)
                                        .desired_width(desired)
                                        .show(ui);
                                    // On every open of a rename dialog,
                                    // select the filename stem (everything
                                    // before the last dot) so the user can
                                    // immediately type a new name.
                                    let is_rename_tab = title == "Rename Tab";
                                    if just_opened && (is_rename || is_rename_tab) {
                                        let end = if is_rename {
                                            name.rfind('.').unwrap_or(name.len())
                                        } else {
                                            name.len()
                                        };
                                        let range = egui::text::CCursorRange::two(
                                            egui::text::CCursor::new(0),
                                            egui::text::CCursor::new(end),
                                        );
                                        output.state.cursor.set_char_range(Some(range));
                                        egui::TextEdit::store_state(
                                            ui.ctx(),
                                            output.response.id,
                                            output.state,
                                        );
                                    }
                                    output.response.response
                                };
                                // Enter submits single-line dialogs; a
                                // multiline folder-name box needs Enter to
                                // insert newlines, so it only submits via OK.
                                // Computed BEFORE the focus seed below: egui
                                // evaluates `lost_focus` against live state,
                                // so re-requesting focus after Enter already
                                // surrendered it would cancel the signal.
                                commit = !multiline
                                    && edit.lost_focus()
                                    && ui.input(|i| i.key_pressed(egui::Key::Enter));
                                // Esc closes any dialog.
                                cancel = ui.input(|i| i.key_pressed(egui::Key::Escape));
                                // Default keyboard focus goes to the input
                                // box — but seed it ONLY while nothing else
                                // holds focus (i.e. on open). Re-requesting
                                // every frame would fight egui's own Tab
                                // navigation and focus could never reach
                                // the OK/Cancel buttons.
                                if ctx.memory(|m| m.focused().is_none()) {
                                    edit.request_focus();
                                }
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
                    self.dialog_just_opened = false;
                    if cancel {
                        self.dialog = None;
                    } else if commit {
                        self.commit_dialog();
                    }
                    // After the dialog closes, clear egui focus so that
                    // keyboard shortcuts (Delete, F2, etc.)
                    // work immediately without the user having to click
                    // the file list first.
                    if self.dialog.is_none() {
                        // Unconditional clear (not surrender_focus, which
                        // only acts if the id still matches) — guarantees
                        // keyboard shortcuts work again even if focus moved
                        // elsewhere in the dialog before it closed.
                        ctx.memory_mut(|m| {
                            m.stop_text_input();
                        });
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

        // Native drag & drop (see `native_drag`'s module docs): register the
        // drop target once an HWND is available, publish this frame's
        // freshly-laid-out pane/tab rects for it to hit-test against, start
        // any row-drag that began this frame (blocks until the drop
        // resolves), then pick up a drop that landed on us without us
        // having started it (a genuinely external drag).
        self.ensure_drop_target_registered(frame);
        self.sync_dnd_shared(&ctx);
        if let Some((pane_idx, paths, from_dir)) = self.pending_native_drag.take() {
            self.start_native_drag(pane_idx, paths, from_dir);
        }
        self.process_pending_native_drop();

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
                let font = egui::FontId::proportional(self.font_size);
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
                painter.rect_filled(egui::Rect::from_min_size(pos, size), 6.0, fill);
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
                ctx.request_repaint_after(std::time::Duration::from_secs(TOAST_SECS) - elapsed);
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

        if self.dirty
            && self.last_persist.elapsed() >= std::time::Duration::from_millis(500)
        {
            self.persist();
            self.last_persist = std::time::Instant::now();
            self.dirty = false;
        }
    }

    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
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
                let a =
                    -std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * (k as f32 / steps as f32);
                pts.push(c + fill_r * egui::vec2(a.cos(), a.sin()));
            }
            painter.add(egui::Shape::convex_polygon(pts, color, egui::Stroke::NONE));
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
        SettingsPage::AppLauncher => {
            // Rocket / launch icon: triangle body + two fins + circle exhaust.
            // Body (upward triangle).
            painter.add(egui::Shape::convex_polygon(
                vec![p(0.50, 0.04), p(0.30, 0.70), p(0.70, 0.70)],
                color,
                egui::Stroke::NONE,
            ));
            // Left fin.
            painter.add(egui::Shape::line(
                vec![p(0.30, 0.70), p(0.16, 0.92), p(0.38, 0.78)],
                stroke,
            ));
            // Right fin.
            painter.add(egui::Shape::line(
                vec![p(0.70, 0.70), p(0.84, 0.92), p(0.62, 0.78)],
                stroke,
            ));
            // Exhaust circle.
            painter.circle_filled(p(0.50, 0.88), r.width() * 0.10, color);
        }
        SettingsPage::FileLauncher => {
            // Document with a small arrow: file + launch.
            // Document body (folded-corner rectangle).
            painter.add(egui::Shape::line(
                vec![
                    p(0.20, 0.06),
                    p(0.58, 0.06),
                    p(0.76, 0.24),
                    p(0.76, 0.94),
                    p(0.20, 0.94),
                    p(0.20, 0.06),
                ],
                stroke,
            ));
            painter.line_segment([p(0.58, 0.06), p(0.58, 0.24)], stroke);
            painter.line_segment([p(0.58, 0.24), p(0.76, 0.24)], stroke);
            // Small right-pointing triangle (launch) in the lower-left.
            painter.add(egui::Shape::convex_polygon(
                vec![p(0.24, 0.50), p(0.24, 0.82), p(0.48, 0.66)],
                color,
                egui::Stroke::NONE,
            ));
        }
        SettingsPage::FileTypes => {
            // Small file/document with a folded corner.
            painter.add(egui::Shape::line(
                vec![
                    p(0.22, 0.06),
                    p(0.60, 0.06),
                    p(0.78, 0.24),
                    p(0.78, 0.94),
                    p(0.22, 0.94),
                    p(0.22, 0.06),
                ],
                stroke,
            ));
            painter.line_segment([p(0.60, 0.06), p(0.60, 0.24)], stroke);
            painter.line_segment([p(0.60, 0.24), p(0.78, 0.24)], stroke);
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
        SettingsPage::About => {
            // Info glyph: ring, dot, stem.
            painter.circle_stroke(c, rad, stroke);
            painter.circle_filled(p(0.5, 0.28), 1.5, color);
            painter.line_segment([p(0.5, 0.46), p(0.5, 0.76)], stroke);
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
    open_targets: &mut Option<Vec<PathBuf>>,
    index: usize,
) {
    if resp.double_clicked() {
        if entry.is_dir {
            *nav_target = Some(entry.path.clone());
        } else {
            *open_targets = Some(vec![entry.path.clone()]);
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
    #[allow(dead_code)]
    OpenInExplorer(PathBuf),
    Properties(PathBuf),
    ShellCommand { id: u32, paths: Vec<PathBuf> },
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
    open_targets: &mut Option<Vec<PathBuf>>,
    index: usize,
) {
    register_entry_click(
        resp,
        entry,
        select_name,
        select_index,
        nav_target,
        open_targets,
        index,
    );
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
    /// True on the frame the user presses the tab — starts a reorder drag.
    drag_started: bool,
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

/// State of an in-progress drag-to-reorder gesture on a tab strip. The drag
/// is tracked manually (pointer position + button state) rather than through
/// egui's per-widget drag state, because live reordering re-indexes tabs and
/// would otherwise orphan the widget id mid-drag.
#[derive(Clone, Copy)]
struct TabReorderDrag {
    pane_idx: usize,
    /// Current index of the dragged tab; updated every time it swaps places.
    idx: usize,
    /// True once the tab actually changed slots, so a press+release that
    /// never moved anything doesn't mark the session dirty.
    moved: bool,
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
    let pos = egui::pos2(
        text_rect.left(),
        text_rect.center().y - galley.size().y / 2.0,
    );
    painter.with_clip_rect(text_rect).galley(pos, galley, color);
}

/// Draws one tab in a pane's tab strip (either inline in the horizontal row
/// or stretched to the sidebar width), including the orange active-tab
/// highlight and the hover "×" close button. Pure widget code — takes no
/// `self`, so it can run while the pane list is mutably borrowed. Tabs sense
/// drags too: `drag_started` on the returned events kicks off a reorder.
fn tab_strip_item(
    ui: &mut egui::Ui,
    label: &str,
    tab_pos: (usize, usize),
    is_tab_active: bool,
    is_active_pane: bool,
    locked: bool,
    renamed: bool,
    tab_hover: &mut Option<(usize, usize)>,
    is_being_dragged: bool,
    size: Option<egui::Vec2>,
    path: &std::path::Path,
) -> TabItemEvents {
    // Both orientations use fully custom surfaces with click+drag sensing:
    // horizontal rows hug their label like the old selectable_label did, and
    // vertical sidebar rows are sized to exactly the strip width — a long
    // label can never stretch the row into the list area, because the
    // interactive rect is fixed before painting and the text is
    // wrapped/clipped afterwards.
    let tab_resp = match size {
        Some(row_size) => {
            let row_rect = egui::Rect::from_min_size(ui.cursor().min, row_size);
            let resp = ui.interact(
                row_rect,
                egui::Id::new(("tab_row", tab_pos.0, tab_pos.1)),
                egui::Sense::click_and_drag(),
            );
            ui.advance_cursor_after_rect(row_rect);
            resp
        }
        None => {
            let font_size = ui
                .style()
                .text_styles
                .get(&egui::TextStyle::Body)
                .map_or(14.0, |f| f.size);
            let galley_w = ui
                .painter()
                .layout_no_wrap(
                    label.to_owned(),
                    egui::FontId::proportional(font_size),
                    egui::Color32::WHITE,
                )
                .size()
                .x;
            let row_w = (galley_w + 14.0).min(ui.available_width());
            let row_h = ui.spacing().interact_size.y.max(22.0);
            let row_rect = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(row_w, row_h));
            let resp = ui.interact(
                row_rect,
                egui::Id::new(("tab_row", tab_pos.0, tab_pos.1)),
                egui::Sense::click_and_drag(),
            );
            ui.advance_cursor_after_rect(row_rect);
            resp
        }
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
            if tab_resp.contains_pointer() {
                hovered_fill
            } else {
                fill
            },
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
    // A tab being drag-reordered wears an orange outline so the user can
    // see which tab they're carrying.
    if is_being_dragged {
        ui.painter().rect_stroke(
            rect,
            egui::CornerRadius::same(6),
            egui::Stroke::new(1.5, egui::Color32::from_rgb(255, 165, 0)),
            egui::StrokeKind::Inside,
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
            egui::Rect::from_center_size(egui::pos2(cx, cy + 3.6), egui::vec2(6.2, 4.6)),
            1.0,
            gold,
        );
    }
    // Renamed tabs wear a small teal dot in the top-left corner, marking the
    // label as user-assigned rather than the automatic folder name (the
    // right corners are taken by the padlock and the hover close button).
    if renamed {
        let teal = egui::Color32::from_rgb(0, 153, 188);
        ui.painter()
            .circle_filled(egui::pos2(rect.left() + 5.5, rect.top() + 5.5), 2.6, teal);
    }
    let hovered = *tab_hover == Some(tab_pos);
    let mut close_clicked = false;
    // Show × close button on the tab's trailing edge when hovered
    if hovered {
        let rect = tab_resp.rect;
        let btn_size = 14.0;
        let btn_rect = egui::Rect::from_min_size(
            egui::pos2(
                rect.max.x - btn_size - 2.0,
                rect.center().y - btn_size / 2.0,
            ),
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
            ui.painter().circle_filled(
                btn_rect.center(),
                btn_size / 2.0,
                egui::Color32::from_rgb(196, 43, 28),
            );
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
    // Explain the badge on renamed tabs: this label was set by the user,
    // not derived from the folder, and survives navigation.
    let path_display = path.to_string_lossy();
    let tab_resp = if renamed {
        tab_resp.on_hover_text(format!(
            "{path_display}\n\nCustom name \u{2014} this tab was renamed; it \
             keeps its name while you navigate. Right-click \u{25B8} Rename \
             Tab to change or clear it.",
        ))
    } else {
        tab_resp.on_hover_text(path_display.as_ref())
    };
    TabItemEvents {
        clicked: tab_resp.clicked(),
        secondary_clicked: tab_resp.secondary_clicked(),
        close_clicked,
        drag_started: tab_resp.drag_started(),
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
/// Color variant for toolbar buttons, so different action categories are
/// visually distinct at a glance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ButtonStyle {
    /// Default blue ribbon — builtin toolbar actions and launcher apps.
    Blue,
    /// Green/teal tint — custom "open with" actions.
    Green,
}

/// Styled toolbar command button with a ribbon-like 3D bevel effect so
/// action rows read as commands rather than page content. Keeps the 3D
/// bevel/hover-lift/press-in states via a scoped widget-style override.
fn toolbar_button(
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
fn context_menu_stroke(theme: egui::Theme) -> egui::Stroke {
    match theme {
        egui::Theme::Dark => egui::Stroke::new(1.0, egui::Color32::from_rgb(72, 88, 115)),
        egui::Theme::Light => egui::Stroke::new(1.0, egui::Color32::from_rgb(150, 170, 205)),
    }
}

/// Paths a right-click on `entry` should act on: the full multi-selection
/// when `entry` is already selected and more than one item is selected
/// (Explorer's own behavior), otherwise just `entry` itself.
fn context_menu_paths(
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

    // Windows Explorer shell context menu sub-menu. `query_items` is a
    // blocking shell/COM call, so it's cached per-selection rather than
    // re-run on every frame this menu stays open (see `shell_menu_cache`'s
    // doc).
    if shell_menu_cache
        .as_ref()
        .is_none_or(|(cached_paths, _)| cached_paths.as_slice() != selection_paths)
    {
        *shell_menu_cache = Some((
            selection_paths.to_vec(),
            crate::shell_menu::query_items(selection_paths),
        ));
    }
    let shell_items = &shell_menu_cache.as_ref().unwrap().1;
    if shell_items
        .iter()
        .any(|item| item.separator || !shell_menu_hidden.contains(&item.label))
    {
        ui.separator();
        ui.menu_button("Windows Explorer", |ui| {
            ui.set_min_width(180.0);
            render_shell_items(ui, row_action, selection_paths, shell_items, shell_menu_hidden);
        });
    }
}

/// Recursively render shell menu items into an egui sub-menu, skipping any
/// item whose label is in `shell_menu_hidden` (configured under Settings).
fn render_shell_items(
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
fn parse_shell_menu_hidden(raw: &str) -> std::collections::HashSet<String> {
    raw.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
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
    w(
        ui,
        "FileMan is a dual-pane file manager. The left panel shows a folder tree with your Favourites at the top. The center area has two independent file browsers, each with tabs, an address bar, a filter, and navigation buttons.",
    );
    ui.add_space(4.0);
    w(
        ui,
        "Switch users via the dropdown in the top-right corner. Each user has independent settings, favourites, toolbar layout, and shortcuts.",
    );

    help_heading(ui, "Navigation");
    w(
        ui,
        "Address Bar — type a path and press Enter to navigate directly.",
    );
    w(ui, "Back (Alt+Left) — return to the previous folder.");
    w(ui, "Forward (Alt+Right) — go forward after going back.");
    w(ui, "Up (Backspace) — go to the parent folder.");
    w(
        ui,
        "Tabs — each pane supports multiple tabs. Open a new tab with + Tab, or close one with the x on hover. Pinned tabs resist accidental navigation.",
    );
    w(
        ui,
        "🕒 Recent — the first toolbar button. Shows recently opened files and folders; click one to jump straight there, or Clear Recent to wipe the list.",
    );

    help_heading(ui, "View Modes");
    w(ui, "Switch between layouts via Settings > View:");
    w(
        ui,
        "  Details — columns for name, date, type, size. Click headers to sort.",
    );
    w(ui, "  List — compact single-column list.");
    w(ui, "  Icons — large icon grid for image-heavy folders.");
    w(
        ui,
        "The filter box (next to the Up button) narrows visible files by name. Click the red x to clear.",
    );

    help_heading(ui, "File Operations");
    w(
        ui,
        "Toolbar buttons provide quick access to Copy (Ctrl+C), Cut (Ctrl+X), Paste (Ctrl+V), Delete (Del), Rename (F2), New Folder, New File, Find (Ctrl+F), and Refresh (F5).",
    );
    w(
        ui,
        "Right-click any file or folder for the context menu with additional options: Extract, Copy Filename, Copy Folder Path, Open With, Open in Windows Explorer, and Add to Favourites.",
    );
    w(
        ui,
        "The \"Windows Explorer\" submenu covers your whole selection, not just the row you clicked — right-click any selected file with several others selected and pick, say, \"Combine files in Foxit PDF\" to combine all of them at once.",
    );
    w(
        ui,
        "Creating a single new folder selects it automatically — press Enter to open it right away. Enter also opens whatever single file or folder is currently selected.",
    );
    w(
        ui,
        "Files dragged in from a mail client (e.g. an Outlook attachment) are accepted the same as files dragged from Explorer.",
    );

    help_heading(ui, "Favourites");
    w(
        ui,
        "Right-click a folder and select Add to Favourites to pin it to the Folder Tree. Right-click a favourite to remove it.",
    );

    help_heading(ui, "Custom Actions");
    w(
        ui,
        "Custom actions let you open files with any application. Go to Settings > Custom Actions to add one. Each action shows as a 🔍 icon button on the second toolbar row.",
    );

    help_heading(ui, "App Launcher & File Launcher");
    w(
        ui,
        "App Launcher (Settings > App Launcher) lets you configure apps as ⚡ quick-launch buttons. File Launcher (Settings > File Launcher) does the same for specific files, opened with 📄 via their default app.",
    );
    w(
        ui,
        "Both show a search box on the toolbar's second row — type to filter, then click a result in the dropdown to launch it.",
    );

    help_heading(ui, "Settings");
    w(
        ui,
        "Appearance — theme (Light/Dark), font family, font size, tab layout (horizontal/vertical).",
    );
    w(
        ui,
        "Keyboard Shortcuts — click Rebind next to any action, then press the new key combination.",
    );
    w(
        ui,
        "Toolbar — reorder or toggle which buttons appear on the main row.",
    );
    w(
        ui,
        "View — choose the default listing layout (Details, List, or Icons).",
    );
    w(
        ui,
        "Advanced — set FileMan as the default folder explorer, or export/import all settings via JSON.",
    );

    help_heading(ui, "Keyboard Shortcuts");
    w(ui, "Ctrl+C Copy | Ctrl+X Cut | Ctrl+V Paste | Ctrl+F Find");
    w(
        ui,
        "F2 Rename | F3 Copy Filename | F4 Copy Folder Path | F5 Refresh",
    );
    w(
        ui,
        "Backspace Go Up | Delete Delete | Alt+Left Back | Alt+Right Forward",
    );
    w(
        ui,
        "Enter Confirm dialog / Open selected file or folder | Escape Cancel / Close dialog",
    );

    help_heading(ui, "Tips");
    w(
        ui,
        "- Pinned tabs won't navigate away when you double-click a folder.",
    );
    w(
        ui,
        "- The filter is per-tab, so each pane filters independently.",
    );
    w(ui, "- Drag the pane divider to resize left/right panes.");
    w(
        ui,
        "- Press Esc to close any dialog including this Help window.",
    );
    w(
        ui,
        "- Use Export/Import in Advanced settings to transfer your setup to another machine.",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_two_panes_pads_a_single_pane_up_to_two() {
        let panes = vec![Pane::new(PathBuf::from("D:\\one"))];
        let (panes, active_pane) = ensure_two_panes(panes, 0, ("name", true));
        assert_eq!(panes.len(), 2);
        assert_eq!(panes[0].tabs[0].path, PathBuf::from("D:\\one"));
        assert_eq!(panes[1].tabs[0].path, PathBuf::from("C:\\"));
        assert_eq!(active_pane, 0);
    }

    #[test]
    fn ensure_two_panes_creates_two_fresh_panes_from_empty() {
        let (panes, active_pane) = ensure_two_panes(Vec::new(), 0, ("name", true));
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
        let (panes, active_pane) = ensure_two_panes(panes, 1, ("name", true));
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
        let (panes, _) = ensure_two_panes(panes, 0, ("name", true));
        assert_eq!(panes.len(), 2);
        assert_eq!(panes[0].tabs[0].path, PathBuf::from("D:\\one"));
        assert_eq!(panes[1].tabs[0].path, PathBuf::from("E:\\two"));
    }

    #[test]
    fn ensure_two_panes_clamps_out_of_range_active_pane() {
        let panes = vec![Pane::new(PathBuf::from("C:\\"))];
        let (panes, active_pane) = ensure_two_panes(panes, 99, ("name", true));
        assert_eq!(panes.len(), 2);
        assert_eq!(active_pane, 1);
    }

    #[test]
    fn ensure_two_panes_seeds_only_fresh_panes_with_default_sort() {
        // A restored session pane keeps its own saved sorting; only the
        // freshly padded pane picks up the universal default.
        let mut restored = Pane::new(PathBuf::from("D:\\kept"));
        restored.tabs[0].sort_col = "modified".to_string();
        restored.tabs[0].sort_asc = false;
        let (panes, _) = ensure_two_panes(vec![restored], 0, ("size", false));
        assert_eq!(panes[0].tabs[0].sort_col, "modified");
        assert!(!panes[0].tabs[0].sort_asc);
        assert_eq!(panes[1].tabs[0].sort_col, "size");
        assert!(!panes[1].tabs[0].sort_asc);

        let (fresh, _) = ensure_two_panes(Vec::new(), 0, ("archive", true));
        assert_eq!(fresh[0].tabs[0].sort_col, "archive");
        assert!(fresh[0].tabs[0].sort_asc);
        assert_eq!(fresh[1].tabs[0].sort_col, "archive");
        assert!(fresh[1].tabs[0].sort_asc);
    }

    #[test]
    fn next_sort_flips_direction_on_same_column() {
        assert_eq!(next_sort("size", true, "size"), ("size".to_string(), false));
        assert_eq!(next_sort("size", false, "size"), ("size".to_string(), true));
    }

    #[test]
    fn next_sort_switches_column_ascending() {
        assert_eq!(
            next_sort("name", false, "modified"),
            ("modified".to_string(), true)
        );
    }

    #[test]
    fn parse_sort_col_accepts_known_columns_and_rejects_junk() {
        for col in ["name", "modified", "size", "archive"] {
            assert_eq!(parse_sort_col(col), Some(col));
        }
        assert_eq!(parse_sort_col("bogus"), None);
        assert_eq!(parse_sort_col(""), None);
    }

    #[test]
    fn poll_subdirs_resolves_background_listing_then_caches() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("sub_b")).unwrap();
        std::fs::create_dir(temp.path().join("sub_a")).unwrap();
        std::fs::write(temp.path().join("file.txt"), b"x").unwrap();

        let mut cache: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
        let mut jobs: HashMap<PathBuf, mpsc::Receiver<std::io::Result<Vec<PathBuf>>>> =
            HashMap::new();
        let dir = temp.path().to_path_buf();

        let mut resolved = None;
        for _ in 0..1000 {
            if let Some(subdirs) = FileManApp::poll_subdirs(&mut cache, &mut jobs, &dir) {
                resolved = Some(subdirs);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let subdirs = resolved.expect("background listing must resolve");
        let names: Vec<String> = subdirs
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["sub_a", "sub_b"], "only directories, sorted");
        assert!(cache.contains_key(&dir), "resolved listing is cached");
        assert!(jobs.is_empty(), "finished job is removed from the map");

        let cached = FileManApp::poll_subdirs(&mut cache, &mut jobs, &dir)
            .expect("cached listing resolves instantly");
        assert_eq!(cached.len(), 2);
        assert!(jobs.is_empty(), "cache hit must not spawn a job");
    }

    /// Headless reproduction of the address-bar flow: edit mode on, the
    /// TextEdit acquires focus via the app's per-frame request_focus, the
    /// user pastes a path, presses Enter — navigation must run and the bar
    /// must drop back to breadcrumb mode. Uses the real egui::Context so
    /// egui 0.36's own focus handling is exercised, not assumed.
    #[test]
    fn address_bar_enter_navigates_and_returns_to_breadcrumbs() {
        let ctx = egui::Context::default();
        let temp = std::env::temp_dir().join("fileman_addr_test");
        std::fs::create_dir_all(&temp).unwrap();
        let target = temp.display().to_string();

        let mut address_bar = String::new();
        let mut edit_mode = true;
        let mut navigated_to: Option<String> = None;
        let mut ever_focused = false;

        for frame in 0..6 {
            let mut raw = egui::RawInput::default();
            if frame == 4 {
                // The user pressed Enter (physical key event, like winit).
                raw.events.push(egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: Some(egui::Key::Enter),
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                });
            }
            let mut full = ctx.run_ui(raw, |ui| {
                // --- exact show_pane_content address-bar fragment ---
                if edit_mode {
                    let address_id = egui::Id::new(("address_bar", 0usize));
                    let address_resp = ui.add(
                        egui::TextEdit::singleline(&mut address_bar)
                            .id(address_id)
                            .desired_width(f32::INFINITY)
                            .hint_text("Type a path and press Enter...")
                            .frame(
                                egui::Frame::new()
                                    .fill(egui::Color32::TRANSPARENT)
                                    .stroke(egui::Stroke::NONE),
                            ),
                    );
                    // pane_idx == self.active_pane in this scenario. The
                    // app seeds focus only until it sticks (mirrors
                    // `focused_address_pane != Some(pane_idx)`); re-asking
                    // every frame would cancel egui's live lost_focus
                    // signal on the Enter frame.
                    if !ever_focused {
                        address_resp.request_focus();
                    }
                    if address_resp.has_focus() {
                        ever_focused = true;
                        // The app writes the pasted path here once focused
                        // (clipboard paste lands in the field).
                        if address_bar.is_empty() {
                            // Simulate Explorer's "Copy as path", which
                            // wraps the path in double quotes.
                            address_bar = format!("\"{target}\"");
                        }
                    }
                    if address_resp.lost_focus() {
                        if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            let typed = address_bar.trim().trim_matches('"').trim();
                            let t = std::path::PathBuf::from(typed);
                            navigated_to = Some(t.display().to_string());
                        }
                        edit_mode = false;
                    }
                }
            });
            // egui asserts at drop if produced font textures were never
            // consumed by a renderer; headless tests have none.
            full.textures_delta.clear();
            if navigated_to.is_some() {
                break;
            }
        }

        assert!(ever_focused, "TextEdit must acquire keyboard focus");
        assert_eq!(
            navigated_to.as_deref(),
            Some(target.as_str()),
            "Enter must navigate"
        );
        assert!(!edit_mode, "bar must return to breadcrumbs after Enter");
    }
}
