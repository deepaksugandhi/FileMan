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
            PRIMARY KEY (pane_index, tab_index)
        );
        CREATE TABLE IF NOT EXISTS app_state (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            active_pane INTEGER NOT NULL DEFAULT 0
        );
        ",
    )
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
}
