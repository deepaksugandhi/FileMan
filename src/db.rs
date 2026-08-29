use rusqlite::{Connection, Result};

/// True if `table` already has a column named `column` (used to make each
/// migration below idempotent — skip it if a previous run already applied
/// it).
fn has_column(conn: &Connection, table: &str, column: &str) -> bool {
    let mut stmt = match conn.prepare(&format!("PRAGMA table_info({table})")) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let names: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();
    names.iter().any(|n| n == column)
}

/// One-time migration folding `user_id` into each per-user table's primary
/// key, so two users' rows (e.g. both `pane_index=0,tab_index=0`) don't
/// collide. All pre-existing data becomes user 1's, matching the seeded
/// default user. Idempotent via `has_column`.
fn migrate_to_multi_user(conn: &Connection) -> Result<()> {
    if !has_column(conn, "window_state", "user_id") {
        conn.execute_batch(
            "
            ALTER TABLE window_state RENAME TO window_state_old;
            CREATE TABLE window_state (
                user_id INTEGER PRIMARY KEY,
                width REAL NOT NULL,
                height REAL NOT NULL,
                pos_x REAL,
                pos_y REAL,
                monitor_name TEXT
            );
            INSERT INTO window_state (user_id, width, height, pos_x, pos_y, monitor_name)
                SELECT 1, width, height, pos_x, pos_y, monitor_name FROM window_state_old;
            DROP TABLE window_state_old;
            ",
        )?;
    }

    if !has_column(conn, "panes", "user_id") {
        conn.execute_batch(
            "
            ALTER TABLE panes RENAME TO panes_old;
            CREATE TABLE panes (
                user_id INTEGER NOT NULL,
                pane_index INTEGER NOT NULL,
                tab_index INTEGER NOT NULL,
                path TEXT NOT NULL,
                is_active_tab INTEGER NOT NULL DEFAULT 0,
                sort_col TEXT NOT NULL DEFAULT 'name',
                sort_asc INTEGER NOT NULL DEFAULT 1,
                col_widths TEXT NOT NULL DEFAULT '220 140 90 60',
                view_mode TEXT NOT NULL DEFAULT 'details',
                locked INTEGER NOT NULL DEFAULT 0,
                custom_name TEXT,
                PRIMARY KEY (user_id, pane_index, tab_index)
            );
            INSERT INTO panes (user_id, pane_index, tab_index, path, is_active_tab, sort_col, sort_asc, col_widths, view_mode, locked)
                SELECT 1, pane_index, tab_index, path, is_active_tab, sort_col, sort_asc, col_widths, view_mode, locked FROM panes_old;
            DROP TABLE panes_old;
            ",
        )?;
    }

    if !has_column(conn, "app_state", "user_id") {
        conn.execute_batch(
            "
            ALTER TABLE app_state RENAME TO app_state_old;
            CREATE TABLE app_state (
                user_id INTEGER PRIMARY KEY,
                active_pane INTEGER NOT NULL DEFAULT 0,
                theme TEXT NOT NULL DEFAULT 'system',
                font_size REAL NOT NULL DEFAULT 14.0,
                font_family TEXT NOT NULL DEFAULT 'Inter',
                split_ratio REAL NOT NULL DEFAULT 0.5
            );
            INSERT INTO app_state (user_id, active_pane, theme, font_size, font_family, split_ratio)
                SELECT 1, active_pane, theme, font_size, font_family, split_ratio FROM app_state_old;
            DROP TABLE app_state_old;
            ",
        )?;
    }

    if !has_column(conn, "favourites", "user_id") {
        conn.execute_batch(
            "
            ALTER TABLE favourites RENAME TO favourites_old;
            CREATE TABLE favourites (
                user_id INTEGER NOT NULL,
                path TEXT NOT NULL,
                sort_order INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (user_id, path)
            );
            INSERT INTO favourites (user_id, path, sort_order)
                SELECT 1, path, sort_order FROM favourites_old;
            DROP TABLE favourites_old;
            ",
        )?;
    }

    Ok(())
}

pub fn init_db(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS window_state (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            width REAL NOT NULL,
            height REAL NOT NULL,
            pos_x REAL,
            pos_y REAL,
            monitor_name TEXT
        );
        CREATE TABLE IF NOT EXISTS panes (
            pane_index INTEGER NOT NULL,
            tab_index INTEGER NOT NULL,
            path TEXT NOT NULL,
            is_active_tab INTEGER NOT NULL DEFAULT 0,
            sort_col TEXT NOT NULL DEFAULT 'name',
            sort_asc INTEGER NOT NULL DEFAULT 1,
            col_widths TEXT NOT NULL DEFAULT '220 140 90 60',
            locked INTEGER NOT NULL DEFAULT 0,
            custom_name TEXT,
            PRIMARY KEY (pane_index, tab_index)
        );
        CREATE TABLE IF NOT EXISTS app_state (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            active_pane INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS favourites (
            path TEXT PRIMARY KEY,
            sort_order INTEGER NOT NULL DEFAULT 0
        );
        ",
    )?;
    // Migration for DBs created before sort settings existed. Errors are
    // ignored: "duplicate column name" just means the column is already there.
    let _ = conn.execute(
        "ALTER TABLE panes ADD COLUMN sort_col TEXT NOT NULL DEFAULT 'name'",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE panes ADD COLUMN sort_asc INTEGER NOT NULL DEFAULT 1",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE panes ADD COLUMN col_widths TEXT NOT NULL DEFAULT '220 140 90 60'",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE app_state ADD COLUMN theme TEXT NOT NULL DEFAULT 'system'",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE app_state ADD COLUMN font_size REAL NOT NULL DEFAULT 14.0",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE app_state ADD COLUMN font_family TEXT NOT NULL DEFAULT 'Inter'",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE panes ADD COLUMN view_mode TEXT NOT NULL DEFAULT 'details'",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE panes ADD COLUMN locked INTEGER NOT NULL DEFAULT 0",
        [],
    );
    let _ = conn.execute("ALTER TABLE panes ADD COLUMN custom_name TEXT", []);
    let _ = conn.execute(
        "ALTER TABLE app_state ADD COLUMN split_ratio REAL NOT NULL DEFAULT 0.5",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE app_state ADD COLUMN tree_width REAL NOT NULL DEFAULT 200.0",
        [],
    );

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS users (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT UNIQUE NOT NULL,
            created_at TEXT NOT NULL,
            is_default INTEGER NOT NULL DEFAULT 0
        );",
    )?;
    migrate_to_multi_user(conn)?;
    let user_count: i64 = conn.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?;
    if user_count == 0 {
        conn.execute(
            "INSERT INTO users (id, name, created_at, is_default) VALUES (1, 'Default', datetime('now'), 1)",
            [],
        )?;
    }

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS global_settings (
            key TEXT PRIMARY KEY,
            value TEXT
        );
        CREATE TABLE IF NOT EXISTS user_settings (
            user_id INTEGER NOT NULL,
            key TEXT NOT NULL,
            value TEXT,
            PRIMARY KEY (user_id, key)
        );
        CREATE TABLE IF NOT EXISTS recent_items (
            user_id INTEGER NOT NULL,
            path TEXT NOT NULL,
            is_dir INTEGER NOT NULL DEFAULT 0,
            accessed_at TEXT NOT NULL,
            PRIMARY KEY (user_id, path)
        );",
    )?;
    for (key, value) in [
        ("theme", "system"),
        ("font_size", "14.0"),
        ("font_family", "Inter"),
    ] {
        conn.execute(
            "INSERT OR IGNORE INTO global_settings (key, value) VALUES (?1, ?2)",
            rusqlite::params![key, value],
        )?;
    }

    Ok(())
}

/// Reads the saved pane-split ratio (fraction of width given to the left
/// pane), if any.
pub fn get_split_ratio(conn: &Connection, user_id: i64) -> Option<f32> {
    conn.query_row(
        "SELECT split_ratio FROM app_state WHERE user_id = ?1",
        rusqlite::params![user_id],
        |row| row.get(0),
    )
    .ok()
}

/// Persists the pane-split ratio, creating the app_state row if needed.
pub fn set_split_ratio(conn: &Connection, user_id: i64, ratio: f32) -> Result<()> {
    conn.execute(
        "INSERT INTO app_state (user_id, active_pane, split_ratio) VALUES (?1, 0, ?2)
         ON CONFLICT(user_id) DO UPDATE SET split_ratio=?2",
        rusqlite::params![user_id, ratio],
    )?;
    Ok(())
}

/// Reads the saved folder-tree panel width, if any.
pub fn get_tree_width(conn: &Connection, user_id: i64) -> Option<f32> {
    conn.query_row(
        "SELECT tree_width FROM app_state WHERE user_id = ?1",
        rusqlite::params![user_id],
        |row| row.get(0),
    )
    .ok()
}

/// Persists the folder-tree panel width, creating the app_state row if needed.
pub fn set_tree_width(conn: &Connection, user_id: i64, width: f32) -> Result<()> {
    conn.execute(
        "INSERT INTO app_state (user_id, active_pane, tree_width) VALUES (?1, 0, ?2)
         ON CONFLICT(user_id) DO UPDATE SET tree_width=?2",
        rusqlite::params![user_id, width],
    )?;
    Ok(())
}

/// Returns all favourite folder paths for `user_id`, ordered by sort_order.
pub fn get_favourites(conn: &Connection, user_id: i64) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT path FROM favourites WHERE user_id = ?1 ORDER BY sort_order")
        .unwrap();
    stmt.query_map(rusqlite::params![user_id], |row| row.get(0))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect()
}

/// Adds a path to favourites if not already present.
pub fn add_favourite(conn: &Connection, user_id: i64, path: &str) -> Result<()> {
    let max_order: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(sort_order), 0) FROM favourites WHERE user_id = ?1",
            rusqlite::params![user_id],
            |row| row.get(0),
        )
        .unwrap_or(0);
    conn.execute(
        "INSERT OR IGNORE INTO favourites (user_id, path, sort_order) VALUES (?1, ?2, ?3)",
        rusqlite::params![user_id, path, max_order + 1],
    )?;
    Ok(())
}

/// Removes a path from favourites.
pub fn remove_favourite(conn: &Connection, user_id: i64, path: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM favourites WHERE user_id = ?1 AND path = ?2",
        rusqlite::params![user_id, path],
    )?;
    Ok(())
}

/// Returns true if the given path is in favourites.
pub fn is_favourite(conn: &Connection, user_id: i64, path: &str) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM favourites WHERE user_id = ?1 AND path = ?2",
        rusqlite::params![user_id, path],
        |row| row.get::<_, i64>(0),
    )
    .unwrap_or(0)
        > 0
}

// ---------------------------------------------------------------------------
// Recent items
// ---------------------------------------------------------------------------

/// A recently accessed file or folder.
#[derive(Debug, Clone)]
pub struct RecentItem {
    pub path: String,
    pub is_dir: bool,
    #[allow(dead_code)]
    pub accessed_at: String,
}

const RECENT_LIMIT: i64 = 50;

/// Records a path as recently accessed, updating the timestamp if it already
/// exists. Trims the oldest entries beyond [`RECENT_LIMIT`].
pub fn add_recent_item(conn: &Connection, user_id: i64, path: &str, is_dir: bool) {
    let _ = conn.execute(
        "INSERT INTO recent_items (user_id, path, is_dir, accessed_at)
         VALUES (?1, ?2, ?3, datetime('now'))
         ON CONFLICT(user_id, path) DO UPDATE SET
             is_dir = excluded.is_dir,
             accessed_at = excluded.accessed_at",
        rusqlite::params![user_id, path, is_dir as i64],
    );
    // Trim oldest entries beyond the cap.
    let _ = conn.execute(
        "DELETE FROM recent_items
         WHERE user_id = ?1
           AND path NOT IN (
               SELECT path FROM recent_items
               WHERE user_id = ?1
               ORDER BY accessed_at DESC
               LIMIT ?2
           )",
        rusqlite::params![user_id, RECENT_LIMIT],
    );
}

/// Returns the most recently accessed items for `user_id`, newest first.
pub fn get_recent_items(conn: &Connection, user_id: i64, limit: usize) -> Vec<RecentItem> {
    let mut stmt = match conn.prepare(
        "SELECT path, is_dir, accessed_at FROM recent_items
         WHERE user_id = ?1
         ORDER BY accessed_at DESC
         LIMIT ?2",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map(rusqlite::params![user_id, limit as i64], |row| {
        Ok(RecentItem {
            path: row.get(0)?,
            is_dir: row.get::<_, i64>(1)? != 0,
            accessed_at: row.get(2)?,
        })
    })
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

/// Removes a single item from the recent list.
#[allow(dead_code)]
pub fn remove_recent_item(conn: &Connection, user_id: i64, path: &str) {
    let _ = conn.execute(
        "DELETE FROM recent_items WHERE user_id = ?1 AND path = ?2",
        rusqlite::params![user_id, path],
    );
}

/// Clears all recent items for `user_id`.
pub fn clear_recent_items(conn: &Connection, user_id: i64) {
    let _ = conn.execute(
        "DELETE FROM recent_items WHERE user_id = ?1",
        rusqlite::params![user_id],
    );
}

pub fn open_db(path: &std::path::Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    init_db(&conn)?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_expected_tables() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table'")
            .unwrap();
        let names: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert!(names.contains(&"window_state".to_string()));
        assert!(names.contains(&"panes".to_string()));
        assert!(names.contains(&"app_state".to_string()));
        assert!(names.contains(&"users".to_string()));
    }

    #[test]
    fn init_db_migrates_a_legacy_panes_table_without_sort_columns() {
        let conn = Connection::open_in_memory().unwrap();
        // Create the pre-sort-settings schema by hand.
        conn.execute_batch(
            "CREATE TABLE panes (
                pane_index INTEGER NOT NULL,
                tab_index INTEGER NOT NULL,
                path TEXT NOT NULL,
                is_active_tab INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (pane_index, tab_index)
            );",
        )
        .unwrap();

        init_db(&conn).unwrap();

        // The new columns must exist and defaults must apply.
        conn.execute(
            "INSERT INTO panes (user_id, pane_index, tab_index, path) VALUES (1, 0, 0, 'C:\\')",
            [],
        )
        .unwrap();
        let (col, asc): (String, i64) = conn
            .query_row(
                "SELECT sort_col, sort_asc FROM panes WHERE user_id = 1 AND pane_index = 0",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(col, "name");
        assert_eq!(asc, 1);
    }

    #[test]
    fn init_db_seeds_a_default_user() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let (name, is_default): (String, i64) = conn
            .query_row("SELECT name, is_default FROM users WHERE id = 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(name, "Default");
        assert_eq!(is_default, 1);
    }

    #[test]
    fn init_db_migrates_legacy_single_user_data_to_user_1() {
        let conn = Connection::open_in_memory().unwrap();
        // Simulate a pre-multi-user DB: create the old (no user_id) schema
        // and populate it, then run init_db to migrate.
        conn.execute_batch(
            "CREATE TABLE window_state (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                width REAL NOT NULL,
                height REAL NOT NULL,
                pos_x REAL,
                pos_y REAL,
                monitor_name TEXT
            );
            CREATE TABLE panes (
                pane_index INTEGER NOT NULL,
                tab_index INTEGER NOT NULL,
                path TEXT NOT NULL,
                is_active_tab INTEGER NOT NULL DEFAULT 0,
                sort_col TEXT NOT NULL DEFAULT 'name',
                sort_asc INTEGER NOT NULL DEFAULT 1,
                col_widths TEXT NOT NULL DEFAULT '220 140 90 60',
                view_mode TEXT NOT NULL DEFAULT 'details',
                PRIMARY KEY (pane_index, tab_index)
            );
            CREATE TABLE app_state (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                active_pane INTEGER NOT NULL DEFAULT 0,
                theme TEXT NOT NULL DEFAULT 'system',
                font_size REAL NOT NULL DEFAULT 14.0,
                font_family TEXT NOT NULL DEFAULT 'Inter',
                split_ratio REAL NOT NULL DEFAULT 0.5
            );
            CREATE TABLE favourites (
                path TEXT PRIMARY KEY,
                sort_order INTEGER NOT NULL DEFAULT 0
            );
            INSERT INTO window_state (id, width, height, pos_x, pos_y, monitor_name)
                VALUES (1, 1200.0, 800.0, 10.0, 20.0, '\\\\.\\DISPLAY1');
            INSERT INTO panes (pane_index, tab_index, path) VALUES (0, 0, 'C:\\Users');
            INSERT INTO app_state (id, active_pane, theme) VALUES (1, 1, 'dark');
            INSERT INTO favourites (path) VALUES ('D:\\Projects');
            ",
        )
        .unwrap();

        init_db(&conn).unwrap();

        let width: f32 = conn
            .query_row(
                "SELECT width FROM window_state WHERE user_id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(width, 1200.0);
        let path: String = conn
            .query_row("SELECT path FROM panes WHERE user_id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(path, "C:\\Users");
        let theme: String = conn
            .query_row("SELECT theme FROM app_state WHERE user_id = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(theme, "dark");
        let fav: String = conn
            .query_row("SELECT path FROM favourites WHERE user_id = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(fav, "D:\\Projects");
    }

    #[test]
    fn favourites_are_scoped_per_user() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO users (id, name, created_at) VALUES (2, 'Alice', datetime('now'))",
            [],
        )
        .unwrap();

        add_favourite(&conn, 1, "C:\\one").unwrap();
        add_favourite(&conn, 2, "C:\\two").unwrap();
        assert_eq!(get_favourites(&conn, 1), vec!["C:\\one".to_string()]);
        assert_eq!(get_favourites(&conn, 2), vec!["C:\\two".to_string()]);
    }

    #[test]
    fn recent_items_are_scoped_per_user() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO users (id, name, created_at) VALUES (2, 'Alice', datetime('now'))",
            [],
        )
        .unwrap();

        add_recent_item(&conn, 1, "C:\\folder", true);
        add_recent_item(&conn, 2, "D:\\other", false);
        let u1 = get_recent_items(&conn, 1, 50);
        let u2 = get_recent_items(&conn, 2, 50);
        assert_eq!(u1.len(), 1);
        assert_eq!(u1[0].path, "C:\\folder");
        assert!(u1[0].is_dir);
        assert_eq!(u2.len(), 1);
        assert_eq!(u2[0].path, "D:\\other");
        assert!(!u2[0].is_dir);
    }

    #[test]
    fn recent_items_are_capped_at_limit() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        for i in 0..60 {
            add_recent_item(&conn, 1, &format!("C:\\dir{i}"), true);
        }
        let items = get_recent_items(&conn, 1, 100);
        assert!(
            items.len() <= RECENT_LIMIT as usize,
            "expected at most {RECENT_LIMIT} items, got {}",
            items.len()
        );
    }

    #[test]
    fn init_db_seeds_global_setting_defaults() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let theme: String = conn
            .query_row(
                "SELECT value FROM global_settings WHERE key = 'theme'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(theme, "system");
    }
}
