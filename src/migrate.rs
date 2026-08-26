//! Settings migration: export all user settings to a portable JSON file and
//! import them again on another user account or another machine.
//!
//! Covers the four settings stores:
//! - `user_settings` / `global_settings` (theme, fonts, tab layout, ...)
//! - `bindings` (keyboard shortcuts, global + per-user scopes)
//! - `toolbar_layout` (button order, global + per-user scopes)
//! - `custom_actions` (per-user "open with" actions)
//!
//! Session state (open tabs, pane layout) is deliberately excluded — it is
//! workspace data, not configuration.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

/// Bump when the format changes; importers accept `version <= FORMAT_VERSION`.
pub const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Kv {
    key: String,
    value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Binding {
    combo: String,
    action_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CustomActionEntry {
    label: String,
    exe_path: String,
}

/// The complete portable settings payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsFile {
    pub version: u32,
    pub exported_at: String,
    pub source_user: Option<String>,
    /// Per-user config rows (`user_settings`) for the exporting account.
    user_config: Vec<Kv>,
    /// Shared config defaults (`global_settings`).
    #[serde(default)]
    global_config: Vec<Kv>,
    #[serde(default)]
    bindings_user: Vec<Binding>,
    #[serde(default)]
    bindings_global: Vec<Binding>,
    #[serde(default)]
    toolbar_user: Vec<String>,
    #[serde(default)]
    toolbar_global: Vec<String>,
    #[serde(default)]
    custom_actions: Vec<CustomActionEntry>,
}

/// How many rows each importer pass wrote, for the status toast.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ImportSummary {
    pub config_values: usize,
    pub bindings: usize,
    pub toolbars: usize,
    pub custom_actions: usize,
}

impl ImportSummary {
    pub fn describe(self) -> String {
        format!(
            "Imported {} config values, {} shortcuts, {} toolbar(s), {} custom action(s)",
            self.config_values, self.bindings, self.toolbars, self.custom_actions
        )
    }
}

fn scope_key_user(user_id: i64) -> String {
    format!("user:{user_id}")
}

/// Gathers every settings row relevant to `user_id` into a portable struct.
pub fn collect(conn: &Connection, user_id: i64, source_user: Option<String>) -> SettingsFile {
    let user_scope = scope_key_user(user_id);
    let query_bindings = |scope: &str| -> Vec<Binding> {
        conn.prepare_cached(
            "SELECT key_combo, action_id FROM bindings WHERE scope = ?1 ORDER BY key_combo",
        )
        .and_then(|mut s| {
            s.query_map([scope], |row| {
                Ok(Binding {
                    combo: row.get(0)?,
                    action_id: row.get(1)?,
                })
            })
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
        })
        .unwrap_or_default()
    };
    let query_toolbar = |scope: &str| -> Vec<String> {
        conn.prepare_cached(
            "SELECT action_id FROM toolbar_layout WHERE scope = ?1 ORDER BY position",
        )
        .and_then(|mut s| {
            s.query_map([scope], |row| row.get::<_, String>(0))
                .map(|rows| rows.filter_map(|r| r.ok()).collect())
        })
        .unwrap_or_default()
    };
    let query_custom = |scope: &str| -> Vec<CustomActionEntry> {
        conn.prepare_cached(
            "SELECT label, exe_path FROM custom_actions WHERE scope = ?1 ORDER BY id",
        )
        .and_then(|mut s| {
            s.query_map([scope], |row| {
                Ok(CustomActionEntry {
                    label: row.get(0)?,
                    exe_path: row.get(1)?,
                })
            })
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
        })
        .unwrap_or_default()
    };

    let user_config: Vec<Kv> = conn
        .prepare_cached("SELECT key, value FROM user_settings WHERE user_id = ?1 ORDER BY key")
        .and_then(|mut s| {
            s.query_map([user_id], |row| {
                Ok(Kv {
                    key: row.get(0)?,
                    value: row.get(1)?,
                })
            })
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
        })
        .unwrap_or_default();
    let global_config: Vec<Kv> = conn
        .prepare_cached("SELECT key, value FROM global_settings ORDER BY key")
        .and_then(|mut s| {
            s.query_map([], |row| {
                Ok(Kv {
                    key: row.get(0)?,
                    value: row.get(1)?,
                })
            })
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
        })
        .unwrap_or_default();

    SettingsFile {
        version: FORMAT_VERSION,
        exported_at: chrono::Utc::now().to_rfc3339(),
        source_user,
        user_config,
        global_config,
        bindings_user: query_bindings(&user_scope),
        bindings_global: query_bindings("global"),
        toolbar_user: query_toolbar(&user_scope),
        toolbar_global: query_toolbar("global"),
        custom_actions: query_custom(&user_scope),
    }
}

/// Writes the export as pretty-printed JSON.
pub fn write_to_path(file: &SettingsFile, path: &std::path::Path) -> Result<(), String> {
    let json = serde_json::to_string_pretty(file).map_err(|e| format!("Serialize failed: {e}"))?;
    std::fs::write(path, json).map_err(|e| format!("Write failed: {e}"))
}

/// Reads and validates an export file.
pub fn read_from_path(path: &std::path::Path) -> Result<SettingsFile, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("Read failed: {e}"))?;
    let file: SettingsFile = serde_json::from_str(&text)
        .map_err(|e| format!("Not a valid FileMan settings file: {e}"))?;
    if file.version > FORMAT_VERSION {
        return Err(format!(
            "Settings file version {} is newer than this app supports ({FORMAT_VERSION})",
            file.version
        ));
    }
    Ok(file)
}

/// Applies a settings file to `user_id`. Config values and bindings are
/// upserted; a non-empty toolbar layout replaces the target scope's layout;
/// custom actions are appended unless an identical label+exe pair exists.
pub fn import_into(
    conn: &Connection,
    user_id: i64,
    file: &SettingsFile,
) -> Result<ImportSummary, String> {
    let mut summary = ImportSummary::default();
    let user_scope = scope_key_user(user_id);

    let mut apply_cfg = |kvs: &[Kv], scope: crate::config::Scope, count: &mut usize| {
        for kv in kvs {
            if crate::config::set(conn, scope, &kv.key, &kv.value).is_ok() {
                *count += 1;
            }
        }
    };
    apply_cfg(
        &file.user_config,
        crate::config::Scope::User(user_id),
        &mut summary.config_values,
    );
    // Only overwrite shared defaults when the exporter actually had any;
    // otherwise a minimal per-user export would wipe nothing but could add
    // stale globals from an unrelated machine. Appending only is safest.
    for kv in &file.global_config {
        if crate::config::get(conn, user_id, &kv.key).is_none() {
            let _ = crate::config::set(conn, crate::config::Scope::Global, &kv.key, &kv.value);
        }
    }

    let upsert_binding = |bindings: &[Binding], scope: &str, count: &mut usize| {
        for b in bindings {
            let ok = conn
                .execute(
                    "INSERT INTO bindings (scope, key_combo, action_id) VALUES (?1, ?2, ?3)
                     ON CONFLICT(scope, key_combo) DO UPDATE SET action_id=?3",
                    rusqlite::params![scope, b.combo, b.action_id],
                )
                .is_ok();
            if ok {
                *count += 1;
            }
        }
    };
    upsert_binding(&file.bindings_user, &user_scope, &mut summary.bindings);
    upsert_binding(&file.bindings_global, "global", &mut summary.bindings);

    let set_toolbar = |ids: &[String], scope: &str, count: &mut usize| {
        if ids.is_empty() {
            return;
        }
        if conn
            .execute("DELETE FROM toolbar_layout WHERE scope = ?1", [scope])
            .is_ok()
        {
            for (pos, id) in ids.iter().enumerate() {
                let _ = conn.execute(
                    "INSERT INTO toolbar_layout (scope, position, action_id) VALUES (?1, ?2, ?3)",
                    rusqlite::params![scope, pos as i64, id],
                );
            }
            *count += 1;
        }
    };
    set_toolbar(&file.toolbar_user, &user_scope, &mut summary.toolbars);
    set_toolbar(&file.toolbar_global, "global", &mut summary.toolbars);

    for ca in &file.custom_actions {
        let already = conn
            .query_row(
                "SELECT COUNT(*) FROM custom_actions WHERE scope = ?1 AND label = ?2 AND exe_path = ?3",
                rusqlite::params![user_scope, ca.label, ca.exe_path],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(1);
        if already == 0 {
            if conn
                .execute(
                    "INSERT INTO custom_actions (label, exe_path, scope) VALUES (?1, ?2, ?3)",
                    rusqlite::params![ca.label, ca.exe_path, user_scope],
                )
                .is_ok()
            {
                summary.custom_actions += 1;
            }
        }
    }

    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_db;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        crate::actions::init_tables(&conn).unwrap();
        conn
    }

    #[test]
    fn round_trips_between_users_on_the_same_database() {
        let conn = setup();
        crate::config::set(&conn, crate::config::Scope::User(1), "theme", "dark").unwrap();
        crate::config::set(&conn, crate::config::Scope::Global, "font_size", "15").unwrap();
        crate::actions::add_custom_action(&conn, 1, "Notepad", "C:\\Windows\\notepad.exe").unwrap();

        let file = collect(&conn, 1, Some("Alice".into()));
        assert_eq!(file.version, FORMAT_VERSION);
        import_into(&conn, 2, &file).unwrap();

        // User 2 received user-scoped config...
        assert_eq!(
            crate::config::get(&conn, 2, "theme"),
            Some("dark".to_string())
        );
        // ...the global default was added (it did not exist before)...
        assert_eq!(
            crate::config::get(&conn, 2, "font_size"),
            Some("15".to_string())
        );
        // ...and the custom action landed in user 2's scope exactly once.
        let customs = crate::actions::list_custom_actions(&conn, 2);
        assert_eq!(customs.len(), 1);
        assert_eq!(customs[0].label, "Notepad");
    }

    #[test]
    fn importing_twice_does_not_duplicate_custom_actions() {
        let conn = setup();
        crate::actions::add_custom_action(&conn, 1, "Notepad", "C:\\Windows\\notepad.exe").unwrap();
        let file = collect(&conn, 1, None);
        import_into(&conn, 2, &file).unwrap();
        import_into(&conn, 2, &file).unwrap();
        assert_eq!(crate::actions::list_custom_actions(&conn, 2).len(), 1);
    }

    #[test]
    fn toolbar_layout_round_trips_between_machines() {
        let conn = setup();
        crate::actions::set_layout(
            &conn,
            crate::actions::Scope::User(1),
            &[
                crate::actions::ActionRef::Builtin(crate::actions::Action::Find),
                crate::actions::ActionRef::Builtin(crate::actions::Action::Copy),
            ],
        )
        .unwrap();

        let file = collect(&conn, 1, None);
        let json = serde_json::to_string_pretty(&file).unwrap();
        let parsed: SettingsFile = serde_json::from_str(&json).unwrap();

        let other_machine = setup();
        let summary = import_into(&other_machine, 7, &parsed).unwrap();
        assert!(summary.toolbars >= 1);

        let toolbar = crate::actions::load_toolbar(&other_machine, 7);
        assert_eq!(toolbar.len(), 2);
        assert_eq!(
            toolbar[0],
            crate::actions::ActionRef::Builtin(crate::actions::Action::Find)
        );
    }

    #[test]
    fn rejects_files_from_a_newer_format_version() {
        let conn = setup();
        let file = collect(&conn, 1, None);
        let newer = format!(
            "{{\"version\":{},\"exported_at\":\"x\",\"source_user\":null,\"user_config\":[]}}",
            FORMAT_VERSION + 1
        );
        let path =
            std::env::temp_dir().join(format!("fileman-mig-test-{}.json", std::process::id()));
        std::fs::write(&path, newer).unwrap();
        assert!(read_from_path(&path).is_err());
        let _ = std::fs::remove_file(&path);

        // The real export still reads back fine.
        let good = std::env::temp_dir().join(format!("fileman-mig-ok-{}.json", std::process::id()));
        write_to_path(&file, &good).unwrap();
        assert!(read_from_path(&good).is_ok());
        let _ = std::fs::remove_file(&good);
    }

    #[test]
    fn binding_rows_round_trip_with_their_scopes() {
        let conn = setup();
        crate::actions::set_binding(
            &conn,
            crate::actions::Scope::User(1),
            crate::actions::KeyCombo::ctrl(egui::Key::K),
            crate::actions::ActionRef::Custom(99),
        )
        .unwrap();
        let file = collect(&conn, 1, None);
        assert_eq!(file.bindings_user.len(), 1);

        let other = setup();
        import_into(&other, 5, &file).unwrap();
        let map = crate::actions::load_shortcut_map(&other, 5);
        let combo = crate::actions::KeyCombo::ctrl(egui::Key::K);
        assert_eq!(
            map.get(&combo),
            Some(&crate::actions::ActionRef::Custom(99))
        );
    }
}
