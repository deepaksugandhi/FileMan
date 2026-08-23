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
    Ok(())
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
