use eframe::egui;
use rusqlite::{Connection, Result};
use std::collections::HashMap;

/// A key combination: modifiers + one key. Round-trips through a DB-storable
/// string like `"Ctrl+Shift+X"` via [`KeyCombo::to_string`]/[`KeyCombo::parse`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyCombo {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub key: egui::Key,
}

impl KeyCombo {
    pub fn new(ctrl: bool, shift: bool, alt: bool, key: egui::Key) -> Self {
        KeyCombo {
            ctrl,
            shift,
            alt,
            key,
        }
    }

    pub fn ctrl(key: egui::Key) -> Self {
        KeyCombo::new(true, false, false, key)
    }

    pub fn plain(key: egui::Key) -> Self {
        KeyCombo::new(false, false, false, key)
    }

    /// True if this combo was just pressed this frame, per the given input
    /// state.
    pub fn matches_input(self, i: &egui::InputState) -> bool {
        i.key_pressed(self.key)
            && i.modifiers.ctrl == self.ctrl
            && i.modifiers.shift == self.shift
            && i.modifiers.alt == self.alt
    }
}

impl std::fmt::Display for KeyCombo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts = Vec::new();
        if self.ctrl {
            parts.push("Ctrl");
        }
        if self.shift {
            parts.push("Shift");
        }
        if self.alt {
            parts.push("Alt");
        }
        parts.push(self.key.name());
        write!(f, "{}", parts.join("+"))
    }
}

impl KeyCombo {
    pub fn parse(raw: &str) -> Option<Self> {
        let mut ctrl = false;
        let mut shift = false;
        let mut alt = false;
        let mut key = None;
        for part in raw.split('+') {
            match part {
                "Ctrl" => ctrl = true,
                "Shift" => shift = true,
                "Alt" => alt = true,
                other => key = egui::Key::ALL.iter().copied().find(|k| k.name() == other),
            }
        }
        key.map(|key| KeyCombo {
            ctrl,
            shift,
            alt,
            key,
        })
    }
}

/// The fixed set of built-in actions FileMan exposes for shortcuts and the
/// toolbar. Adding a new user-facing command means adding a variant here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    Copy,
    Cut,
    Paste,
    Delete,
    Rename,
    NewFolder,
    NewFile,
    CopyFilename,
    CopyFolderPath,
    ExtractHere,
    ExtractTo,
    ToggleFavourite,
    GoBack,
    GoForward,
    GoUp,
    NewTab,
    CloseTab,
    Refresh,
    Find,
    ToggleSettings,
    SelectAll,
}

impl Action {
    pub const ALL: [Action; 21] = [
        Action::Copy,
        Action::Cut,
        Action::Paste,
        Action::Delete,
        Action::Rename,
        Action::NewFolder,
        Action::NewFile,
        Action::CopyFilename,
        Action::CopyFolderPath,
        Action::ExtractHere,
        Action::ExtractTo,
        Action::ToggleFavourite,
        Action::GoBack,
        Action::GoForward,
        Action::GoUp,
        Action::NewTab,
        Action::CloseTab,
        Action::Refresh,
        Action::Find,
        Action::ToggleSettings,
        Action::SelectAll,
    ];

    /// Stable string id for DB storage — never changes even if `label` does.
    pub fn id(self) -> &'static str {
        match self {
            Action::Copy => "copy",
            Action::Cut => "cut",
            Action::Paste => "paste",
            Action::Delete => "delete",
            Action::Rename => "rename",
            Action::NewFolder => "new_folder",
            Action::NewFile => "new_file",
            Action::CopyFilename => "copy_filename",
            Action::CopyFolderPath => "copy_folder_path",
            Action::ExtractHere => "extract_here",
            Action::ExtractTo => "extract_to",
            Action::ToggleFavourite => "toggle_favourite",
            Action::GoBack => "go_back",
            Action::GoForward => "go_forward",
            Action::GoUp => "go_up",
            Action::NewTab => "new_tab",
            Action::CloseTab => "close_tab",
            Action::Refresh => "refresh",
            Action::Find => "find",
            Action::ToggleSettings => "toggle_settings",
            Action::SelectAll => "select_all",
        }
    }

    pub fn from_id(id: &str) -> Option<Action> {
        Action::ALL.into_iter().find(|a| a.id() == id)
    }

    pub fn label(self) -> &'static str {
        match self {
            Action::Copy => "Copy",
            Action::Cut => "Cut",
            Action::Paste => "Paste",
            Action::Delete => "Delete",
            Action::Rename => "Rename",
            Action::NewFolder => "New Folder",
            Action::NewFile => "New File",
            Action::CopyFilename => "Copy Filename",
            Action::CopyFolderPath => "Copy Folder Path",
            Action::ExtractHere => "Extract Here",
            Action::ExtractTo => "Extract to...",
            Action::ToggleFavourite => "Toggle Favourite",
            Action::GoBack => "Back",
            Action::GoForward => "Forward",
            Action::GoUp => "Up",
            Action::NewTab => "New Tab",
            Action::CloseTab => "Close Tab",
            Action::Refresh => "Refresh",
            Action::Find => "Find",
            Action::ToggleSettings => "Settings",
            Action::SelectAll => "Select All",
        }
    }

    /// Hardcoded default shortcuts, matching what FileMan bound before the
    /// rebindable-shortcuts system existed. An action can have more than one
    /// (e.g. Copy Filename is both F3 and the Explorer-style Ctrl+Shift+C).
    pub fn default_shortcuts(self) -> &'static [KeyCombo] {
        match self {
            Action::Copy => &[KeyCombo {
                ctrl: true,
                shift: false,
                alt: false,
                key: egui::Key::C,
            }],
            Action::Cut => &[KeyCombo {
                ctrl: true,
                shift: false,
                alt: false,
                key: egui::Key::X,
            }],
            Action::Paste => &[KeyCombo {
                ctrl: true,
                shift: false,
                alt: false,
                key: egui::Key::V,
            }],
            Action::Find => &[KeyCombo {
                ctrl: true,
                shift: false,
                alt: false,
                key: egui::Key::F,
            }],
            Action::Refresh => &[KeyCombo {
                ctrl: false,
                shift: false,
                alt: false,
                key: egui::Key::F5,
            }],
            Action::GoUp => &[KeyCombo {
                ctrl: false,
                shift: false,
                alt: false,
                key: egui::Key::Backspace,
            }],
            Action::Rename => &[KeyCombo {
                ctrl: false,
                shift: false,
                alt: false,
                key: egui::Key::F2,
            }],
            Action::CopyFilename => &[
                KeyCombo {
                    ctrl: false,
                    shift: false,
                    alt: false,
                    key: egui::Key::F3,
                },
                KeyCombo {
                    ctrl: true,
                    shift: true,
                    alt: false,
                    key: egui::Key::C,
                },
            ],
            Action::CopyFolderPath => &[KeyCombo {
                ctrl: false,
                shift: false,
                alt: false,
                key: egui::Key::F4,
            }],
            Action::Delete => &[KeyCombo {
                ctrl: false,
                shift: false,
                alt: false,
                key: egui::Key::Delete,
            }],
            Action::SelectAll => &[KeyCombo {
                ctrl: true,
                shift: false,
                alt: false,
                key: egui::Key::A,
            }],
            _ => &[],
        }
    }
}

/// Either a built-in [`Action`] or a user-defined "open with `<exe>`" custom
/// action (referencing a `custom_actions.id` row). The unit both the
/// shortcut map and the toolbar layout are built from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionRef {
    Builtin(Action),
    Custom(i64),
}

impl ActionRef {
    fn to_id(self) -> String {
        match self {
            ActionRef::Builtin(a) => a.id().to_string(),
            ActionRef::Custom(id) => format!("custom:{id}"),
        }
    }

    fn from_id(id: &str) -> Option<ActionRef> {
        if let Some(rest) = id.strip_prefix("custom:") {
            return rest.parse().ok().map(ActionRef::Custom);
        }
        Action::from_id(id).map(ActionRef::Builtin)
    }

    /// Display label, resolving a custom action's name from `custom_actions`
    /// (falling back to a placeholder if the row was since deleted).
    pub fn label(self, custom_actions: &[CustomAction]) -> String {
        match self {
            ActionRef::Builtin(a) => a.label().to_string(),
            ActionRef::Custom(id) => custom_actions
                .iter()
                .find(|c| c.id == id)
                .map(|c| c.label.clone())
                .unwrap_or_else(|| "(deleted action)".to_string()),
        }
    }
}

/// A user-defined "open with `<exe>`" toolbar/shortcut action.
#[derive(Debug, Clone, PartialEq)]
pub struct CustomAction {
    pub id: i64,
    pub label: String,
    pub exe_path: String,
}

/// Where a binding/layout row applies: a specific user, or the shared
/// global default (used to seed new users and as the fallback tier).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Global,
    User(i64),
}

impl Scope {
    fn to_key(self) -> String {
        match self {
            Scope::Global => "global".to_string(),
            Scope::User(id) => format!("user:{id}"),
        }
    }
}

pub fn init_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS custom_actions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            label TEXT NOT NULL,
            exe_path TEXT NOT NULL,
            scope TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS bindings (
            scope TEXT NOT NULL,
            key_combo TEXT NOT NULL,
            action_id TEXT NOT NULL,
            PRIMARY KEY (scope, key_combo)
        );
        CREATE TABLE IF NOT EXISTS toolbar_layout (
            scope TEXT NOT NULL,
            position INTEGER NOT NULL,
            action_id TEXT NOT NULL,
            PRIMARY KEY (scope, position)
        );
        CREATE TABLE IF NOT EXISTS ext_overrides (
            user_id INTEGER NOT NULL,
            ext TEXT NOT NULL,
            exe_path TEXT NOT NULL,
            PRIMARY KEY (user_id, ext)
        );",
    )?;

    // Seed the global toolbar layout with today's actual button order, once,
    // so existing behavior doesn't change until a user customizes it.
    let has_global_layout: i64 = conn.query_row(
        "SELECT COUNT(*) FROM toolbar_layout WHERE scope = 'global'",
        [],
        |r| r.get(0),
    )?;
    if has_global_layout == 0 {
        let default_toolbar = [
            Action::Copy,
            Action::Cut,
            Action::Paste,
            Action::Delete,
            Action::Rename,
            Action::ExtractHere,
            Action::ExtractTo,
            Action::CopyFilename,
            Action::CopyFolderPath,
            Action::ToggleFavourite,
            Action::Find,
            Action::NewFolder,
            Action::NewFile,
        ];
        set_layout(
            conn,
            Scope::Global,
            &default_toolbar.map(ActionRef::Builtin),
        )?;
    }

    Ok(())
}

/// Lists a user's custom "open with" actions (global scope + their own).
pub fn list_custom_actions(conn: &Connection, user_id: i64) -> Vec<CustomAction> {
    let mut stmt = match conn.prepare(
        "SELECT id, label, exe_path FROM custom_actions WHERE scope = 'global' OR scope = ?1 ORDER BY id",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map(rusqlite::params![Scope::User(user_id).to_key()], |row| {
        Ok(CustomAction {
            id: row.get(0)?,
            label: row.get(1)?,
            exe_path: row.get(2)?,
        })
    })
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

/// Adds a custom "open with `<exe>`" action, scoped to `user_id`.
pub fn add_custom_action(
    conn: &Connection,
    user_id: i64,
    label: &str,
    exe_path: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO custom_actions (label, exe_path, scope) VALUES (?1, ?2, ?3)",
        rusqlite::params![label, exe_path, Scope::User(user_id).to_key()],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Removes a custom action by row id.
pub fn remove_custom_action(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM custom_actions WHERE id = ?1",
        rusqlite::params![id],
    )?;
    Ok(())
}

/// Builds the effective shortcut map for `user_id`: each `Action`'s hardcoded
/// default, overridden by any global binding, overridden by any per-user
/// binding.
pub fn load_shortcut_map(conn: &Connection, user_id: i64) -> HashMap<KeyCombo, ActionRef> {
    let mut map: HashMap<KeyCombo, ActionRef> = HashMap::new();
    for action in Action::ALL {
        for combo in action.default_shortcuts() {
            map.insert(*combo, ActionRef::Builtin(action));
        }
    }
    for scope_key in [Scope::Global.to_key(), Scope::User(user_id).to_key()] {
        if let Ok(mut stmt) =
            conn.prepare("SELECT key_combo, action_id FROM bindings WHERE scope = ?1")
        {
            let rows: Vec<(String, String)> = stmt
                .query_map(rusqlite::params![scope_key], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })
                .map(|rows| rows.filter_map(|r| r.ok()).collect())
                .unwrap_or_default();
            for (combo_str, action_id) in rows {
                let Some(combo) = KeyCombo::parse(&combo_str) else {
                    continue;
                };
                if action_id == UNBOUND_SENTINEL {
                    // Explicitly cleared — overrides the hardcoded default
                    // (or a global binding) for this combo.
                    map.remove(&combo);
                } else if let Some(action) = ActionRef::from_id(&action_id) {
                    map.insert(combo, action);
                }
            }
        }
    }
    map
}

/// Binds `combo` to `action` at the given scope. Rejects (returning the
/// action already bound there) if `combo` is already taken by something
/// else in that same scope's *explicit* bindings — callers surface this as
/// a status message rather than silently overwriting.
pub fn set_binding(
    conn: &Connection,
    scope: Scope,
    combo: KeyCombo,
    action: ActionRef,
) -> Result<Option<ActionRef>> {
    let scope_key = scope.to_key();
    let combo_str = combo.to_string();
    let existing: Option<String> = conn
        .query_row(
            "SELECT action_id FROM bindings WHERE scope = ?1 AND key_combo = ?2",
            rusqlite::params![scope_key, combo_str],
            |row| row.get(0),
        )
        .ok();
    if let Some(existing_id) = existing {
        if existing_id != action.to_id() {
            return Ok(ActionRef::from_id(&existing_id));
        }
    }
    conn.execute(
        "INSERT INTO bindings (scope, key_combo, action_id) VALUES (?1, ?2, ?3)
         ON CONFLICT(scope, key_combo) DO UPDATE SET action_id=?3",
        rusqlite::params![scope_key, combo_str, action.to_id()],
    )?;
    Ok(None)
}

/// Sentinel `action_id` marking a combo as explicitly unbound — distinct from
/// simply having no row, which would let a hardcoded default (or a broader
/// scope's binding) keep applying.
const UNBOUND_SENTINEL: &str = "none";

/// Clears whatever is bound to `combo` at `scope` (default or explicit),
/// so it no longer triggers any action.
pub fn clear_binding(conn: &Connection, scope: Scope, combo: KeyCombo) -> Result<()> {
    conn.execute(
        "INSERT INTO bindings (scope, key_combo, action_id) VALUES (?1, ?2, ?3)
         ON CONFLICT(scope, key_combo) DO UPDATE SET action_id=?3",
        rusqlite::params![scope.to_key(), combo.to_string(), UNBOUND_SENTINEL],
    )?;
    Ok(())
}

/// Reads the toolbar layout for `scope`, ordered by position.
pub fn get_layout(conn: &Connection, scope: Scope) -> Vec<ActionRef> {
    let mut stmt = match conn
        .prepare("SELECT action_id FROM toolbar_layout WHERE scope = ?1 ORDER BY position")
    {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map(rusqlite::params![scope.to_key()], |row| {
        row.get::<_, String>(0)
    })
    .map(|rows| {
        rows.filter_map(|r| r.ok())
            .filter_map(|id| ActionRef::from_id(&id))
            .collect()
    })
    .unwrap_or_default()
}

/// Overwrites the toolbar layout for `scope`.
pub fn set_layout(conn: &Connection, scope: Scope, actions: &[ActionRef]) -> Result<()> {
    let scope_key = scope.to_key();
    conn.execute(
        "DELETE FROM toolbar_layout WHERE scope = ?1",
        rusqlite::params![scope_key],
    )?;
    for (position, action) in actions.iter().enumerate() {
        conn.execute(
            "INSERT INTO toolbar_layout (scope, position, action_id) VALUES (?1, ?2, ?3)",
            rusqlite::params![scope_key, position as i64, action.to_id()],
        )?;
    }
    Ok(())
}

/// The effective toolbar for `user_id`: their own layout if they have one,
/// otherwise the global default.
pub fn load_toolbar(conn: &Connection, user_id: i64) -> Vec<ActionRef> {
    let user_layout = get_layout(conn, Scope::User(user_id));
    if !user_layout.is_empty() {
        user_layout
    } else {
        get_layout(conn, Scope::Global)
    }
}

/// Extension (without the leading dot, lowercased) a user has pinned to a
/// specific program, so opening such a file skips the Windows default-app
/// association. E.g. always launching Excel for `.xlsm`, even if another
/// program (WPS, etc.) currently owns that extension in Windows.
pub fn list_ext_overrides(conn: &Connection, user_id: i64) -> Vec<(String, String)> {
    let mut stmt = match conn
        .prepare("SELECT ext, exe_path FROM ext_overrides WHERE user_id = ?1 ORDER BY ext")
    {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map(rusqlite::params![user_id], |row| {
        Ok((row.get(0)?, row.get(1)?))
    })
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

/// The exe pinned to `ext` for `user_id`, if any.
pub fn get_ext_override(conn: &Connection, user_id: i64, ext: &str) -> Option<String> {
    conn.query_row(
        "SELECT exe_path FROM ext_overrides WHERE user_id = ?1 AND ext = ?2",
        rusqlite::params![user_id, ext.to_lowercase()],
        |row| row.get(0),
    )
    .ok()
}

/// Pins `ext` to always open with `exe_path` for `user_id`.
pub fn set_ext_override(conn: &Connection, user_id: i64, ext: &str, exe_path: &str) -> Result<()> {
    let ext = ext.trim_start_matches('.').to_lowercase();
    conn.execute(
        "INSERT INTO ext_overrides (user_id, ext, exe_path) VALUES (?1, ?2, ?3)
         ON CONFLICT(user_id, ext) DO UPDATE SET exe_path=?3",
        rusqlite::params![user_id, ext, exe_path],
    )?;
    Ok(())
}

/// Removes `ext`'s override for `user_id`, restoring the Windows default.
pub fn remove_ext_override(conn: &Connection, user_id: i64, ext: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM ext_overrides WHERE user_id = ?1 AND ext = ?2",
        rusqlite::params![user_id, ext],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_db;

    #[test]
    fn key_combo_round_trips_through_string() {
        let combo = KeyCombo::new(true, true, false, egui::Key::X);
        let s = combo.to_string();
        assert_eq!(s, "Ctrl+Shift+X");
        assert_eq!(KeyCombo::parse(&s), Some(combo));
    }

    #[test]
    fn action_id_round_trips() {
        for action in Action::ALL {
            assert_eq!(Action::from_id(action.id()), Some(action));
        }
    }

    #[test]
    fn set_binding_rejects_a_conflicting_combo_in_the_same_scope() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        init_tables(&conn).unwrap();

        let combo = KeyCombo::ctrl(egui::Key::K);
        set_binding(
            &conn,
            Scope::User(1),
            combo,
            ActionRef::Builtin(Action::Copy),
        )
        .unwrap();
        let conflict = set_binding(
            &conn,
            Scope::User(1),
            combo,
            ActionRef::Builtin(Action::Cut),
        )
        .unwrap();
        assert_eq!(conflict, Some(ActionRef::Builtin(Action::Copy)));
    }

    #[test]
    fn set_binding_allows_rebinding_the_same_action_to_the_same_combo() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        init_tables(&conn).unwrap();

        let combo = KeyCombo::ctrl(egui::Key::K);
        set_binding(
            &conn,
            Scope::User(1),
            combo,
            ActionRef::Builtin(Action::Copy),
        )
        .unwrap();
        let result = set_binding(
            &conn,
            Scope::User(1),
            combo,
            ActionRef::Builtin(Action::Copy),
        )
        .unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn default_shortcuts_include_select_all_and_copy_path() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        init_tables(&conn).unwrap();

        let map = load_shortcut_map(&conn, 1);
        // Ctrl+A selects everything in the view.
        assert_eq!(
            map.get(&KeyCombo::ctrl(egui::Key::A)),
            Some(&ActionRef::Builtin(Action::SelectAll))
        );
        // Ctrl+Shift+C copies the filename with its full path (the
        // plain Ctrl+C stays the file-copy action).
        assert_eq!(
            map.get(&KeyCombo::new(true, true, false, egui::Key::C)),
            Some(&ActionRef::Builtin(Action::CopyFilename))
        );
        assert_eq!(
            map.get(&KeyCombo::ctrl(egui::Key::C)),
            Some(&ActionRef::Builtin(Action::Copy))
        );
    }

    #[test]
    fn shortcut_map_merge_precedence_user_over_global_over_default() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        init_tables(&conn).unwrap();

        // Default: Ctrl+C is Copy.
        let map = load_shortcut_map(&conn, 1);
        assert_eq!(
            map.get(&KeyCombo::ctrl(egui::Key::C)),
            Some(&ActionRef::Builtin(Action::Copy))
        );

        // Global override: Ctrl+C becomes Find for everyone.
        set_binding(
            &conn,
            Scope::Global,
            KeyCombo::ctrl(egui::Key::C),
            ActionRef::Builtin(Action::Find),
        )
        .unwrap();
        let map = load_shortcut_map(&conn, 1);
        assert_eq!(
            map.get(&KeyCombo::ctrl(egui::Key::C)),
            Some(&ActionRef::Builtin(Action::Find))
        );

        // Per-user override: user 1 gets Copy back, user 2 still sees Find.
        set_binding(
            &conn,
            Scope::User(1),
            KeyCombo::ctrl(egui::Key::C),
            ActionRef::Builtin(Action::Copy),
        )
        .unwrap();
        let map1 = load_shortcut_map(&conn, 1);
        let map2 = load_shortcut_map(&conn, 2);
        assert_eq!(
            map1.get(&KeyCombo::ctrl(egui::Key::C)),
            Some(&ActionRef::Builtin(Action::Copy))
        );
        assert_eq!(
            map2.get(&KeyCombo::ctrl(egui::Key::C)),
            Some(&ActionRef::Builtin(Action::Find))
        );
    }

    #[test]
    fn toolbar_layout_round_trips_and_falls_back_to_global() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        init_tables(&conn).unwrap();

        // No per-user layout yet: falls back to the seeded global default.
        let toolbar = load_toolbar(&conn, 1);
        assert!(!toolbar.is_empty());
        assert_eq!(toolbar[0], ActionRef::Builtin(Action::Copy));

        // Setting a per-user layout takes precedence.
        set_layout(&conn, Scope::User(1), &[ActionRef::Builtin(Action::Find)]).unwrap();
        let toolbar = load_toolbar(&conn, 1);
        assert_eq!(toolbar, vec![ActionRef::Builtin(Action::Find)]);
    }

    #[test]
    fn ext_override_round_trips_and_normalizes_case_and_dot() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        init_tables(&conn).unwrap();

        assert_eq!(get_ext_override(&conn, 1, "xlsm"), None);
        set_ext_override(&conn, 1, ".XLSM", "C:\\Excel.exe").unwrap();
        assert_eq!(
            get_ext_override(&conn, 1, "xlsm"),
            Some("C:\\Excel.exe".to_string())
        );
        assert_eq!(get_ext_override(&conn, 2, "xlsm"), None);

        set_ext_override(&conn, 1, "xlsm", "C:\\Excel2.exe").unwrap();
        assert_eq!(list_ext_overrides(&conn, 1).len(), 1);
        assert_eq!(
            get_ext_override(&conn, 1, "xlsm"),
            Some("C:\\Excel2.exe".to_string())
        );

        remove_ext_override(&conn, 1, "xlsm").unwrap();
        assert_eq!(get_ext_override(&conn, 1, "xlsm"), None);
    }

    #[test]
    fn custom_actions_are_scoped_to_their_user() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        init_tables(&conn).unwrap();

        add_custom_action(&conn, 1, "Notepad", "C:\\Windows\\notepad.exe").unwrap();
        let user1 = list_custom_actions(&conn, 1);
        let user2 = list_custom_actions(&conn, 2);
        assert_eq!(user1.len(), 1);
        assert_eq!(user2.len(), 0);

        remove_custom_action(&conn, user1[0].id).unwrap();
        assert!(list_custom_actions(&conn, 1).is_empty());
    }
}
