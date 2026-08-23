use rusqlite::{Connection, Result};

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
    Ok(())
}

/// Reads the saved theme preference ("dark" / "light" / "system"), if any.
pub fn get_theme(conn: &Connection) -> Option<String> {
    conn.query_row("SELECT theme FROM app_state WHERE id = 1", [], |row| {
        row.get(0)
    })
    .ok()
}

/// Persists the theme preference, creating the app_state row if it doesn't
/// exist yet (this can be called before the first session save).
pub fn set_theme(conn: &Connection, theme: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO app_state (id, active_pane, theme) VALUES (1, 0, ?1)
         ON CONFLICT(id) DO UPDATE SET theme=?1",
        rusqlite::params![theme],
    )?;
    Ok(())
}

/// Reads the saved font size, if any.
pub fn get_font_size(conn: &Connection) -> Option<f32> {
    conn.query_row("SELECT font_size FROM app_state WHERE id = 1", [], |row| {
        row.get(0)
    })
    .ok()
}

/// Persists the font size preference.
pub fn set_font_size(conn: &Connection, size: f32) -> Result<()> {
    conn.execute(
        "INSERT INTO app_state (id, active_pane, theme, font_size, font_family) VALUES (1, 0, 'system', ?1, 'Inter')
         ON CONFLICT(id) DO UPDATE SET font_size=?1",
        rusqlite::params![size],
    )?;
    Ok(())
}

/// Reads the saved font family, if any.
pub fn get_font_family(conn: &Connection) -> Option<String> {
    conn.query_row("SELECT font_family FROM app_state WHERE id = 1", [], |row| {
        row.get(0)
    })
    .ok()
}

/// Persists the font family preference.
pub fn set_font_family(conn: &Connection, family: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO app_state (id, active_pane, theme, font_size, font_family) VALUES (1, 0, 'system', 14.0, ?1)
         ON CONFLICT(id) DO UPDATE SET font_family=?1",
        rusqlite::params![family],
    )?;
    Ok(())
}

/// Returns all favourite folder paths, ordered by sort_order.
pub fn get_favourites(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT path FROM favourites ORDER BY sort_order")
        .unwrap();
    stmt.query_map([], |row| row.get(0))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect()
}

/// Adds a path to favourites if not already present.
pub fn add_favourite(conn: &Connection, path: &str) -> Result<()> {
    let max_order: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(sort_order), 0) FROM favourites",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    conn.execute(
        "INSERT OR IGNORE INTO favourites (path, sort_order) VALUES (?1, ?2)",
        rusqlite::params![path, max_order + 1],
    )?;
    Ok(())
}

/// Removes a path from favourites.
pub fn remove_favourite(conn: &Connection, path: &str) -> Result<()> {
    conn.execute("DELETE FROM favourites WHERE path = ?1", rusqlite::params![path])?;
    Ok(())
}

/// Returns true if the given path is in favourites.
pub fn is_favourite(conn: &Connection, path: &str) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM favourites WHERE path = ?1",
        rusqlite::params![path],
        |row| row.get::<_, i64>(0),
    )
    .unwrap_or(0)
        > 0
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
            "INSERT INTO panes (pane_index, tab_index, path) VALUES (0, 0, 'C:\\')",
            [],
        )
        .unwrap();
        let (col, asc): (String, i64) = conn
            .query_row(
                "SELECT sort_col, sort_asc FROM panes WHERE pane_index = 0",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(col, "name");
        assert_eq!(asc, 1);
    }
}
