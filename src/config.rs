use rusqlite::{Connection, Result};

/// Where a config value is written: a specific user's override, or the
/// shared global default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Global,
    User(i64),
}

/// Reads `key` for `user_id`: a per-user override if one exists, otherwise
/// the global default, otherwise `None`.
pub fn get(conn: &Connection, user_id: i64, key: &str) -> Option<String> {
    conn.query_row(
        "SELECT value FROM user_settings WHERE user_id = ?1 AND key = ?2",
        rusqlite::params![user_id, key],
        |row| row.get(0),
    )
    .ok()
    .or_else(|| {
        conn.query_row(
            "SELECT value FROM global_settings WHERE key = ?1",
            rusqlite::params![key],
            |row| row.get(0),
        )
        .ok()
    })
}

/// Writes `key` = `value` at the given scope.
pub fn set(conn: &Connection, scope: Scope, key: &str, value: &str) -> Result<()> {
    match scope {
        Scope::Global => {
            conn.execute(
                "INSERT INTO global_settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value=?2",
                rusqlite::params![key, value],
            )?;
        }
        Scope::User(user_id) => {
            conn.execute(
                "INSERT INTO user_settings (user_id, key, value) VALUES (?1, ?2, ?3)
                 ON CONFLICT(user_id, key) DO UPDATE SET value=?3",
                rusqlite::params![user_id, key, value],
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_db;

    #[test]
    fn falls_back_to_global_when_no_user_override_exists() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        assert_eq!(get(&conn, 1, "theme"), Some("system".to_string()));
    }

    #[test]
    fn user_override_takes_precedence_over_global() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        set(&conn, Scope::Global, "theme", "light").unwrap();
        set(&conn, Scope::User(1), "theme", "dark").unwrap();
        assert_eq!(get(&conn, 1, "theme"), Some("dark".to_string()));
        // A different user still sees the global value.
        assert_eq!(get(&conn, 2, "theme"), Some("light".to_string()));
    }

    #[test]
    fn unknown_key_with_no_global_default_returns_none() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        assert_eq!(get(&conn, 1, "does_not_exist"), None);
    }
}
