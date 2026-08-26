use rusqlite::{Connection, Result};

/// A user profile: distinct saved session, favourites, and settings.
#[derive(Debug, Clone, PartialEq)]
pub struct User {
    pub id: i64,
    pub name: String,
}

/// Lists all user profiles, ordered by id (creation order).
pub fn list_users(conn: &Connection) -> Vec<User> {
    let mut stmt = match conn.prepare("SELECT id, name FROM users ORDER BY id") {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map([], |row| {
        Ok(User {
            id: row.get(0)?,
            name: row.get(1)?,
        })
    })
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

/// Creates a new user profile with the given display name.
pub fn create_user(conn: &Connection, name: &str) -> Result<i64> {
    conn.execute(
        "INSERT INTO users (name, created_at) VALUES (?1, datetime('now'))",
        rusqlite::params![name],
    )?;
    Ok(conn.last_insert_rowid())
}

/// The id of the default user (seeded by `db::init_db`), falling back to the
/// first user by id if the `is_default` flag was somehow lost.
pub fn default_user_id(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT id FROM users WHERE is_default = 1 ORDER BY id LIMIT 1",
        [],
        |row| row.get(0),
    )
    .or_else(|_| {
        conn.query_row("SELECT id FROM users ORDER BY id LIMIT 1", [], |row| {
            row.get(0)
        })
    })
    .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_db;

    #[test]
    fn default_user_id_returns_the_seeded_default() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        assert_eq!(default_user_id(&conn), 1);
    }

    #[test]
    fn create_user_adds_a_new_profile() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let id = create_user(&conn, "Alice").unwrap();
        let users = list_users(&conn);
        assert_eq!(users.len(), 2);
        assert!(users.iter().any(|u| u.id == id && u.name == "Alice"));
    }

    #[test]
    fn list_users_is_ordered_by_creation() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        create_user(&conn, "Bob").unwrap();
        create_user(&conn, "Alice").unwrap();
        let users = list_users(&conn);
        let names: Vec<&str> = users.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(names, ["Default", "Bob", "Alice"]);
    }
}
