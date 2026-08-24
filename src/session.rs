use crate::pane::Pane;
use crate::tab::Tab;
use rusqlite::{Connection, Result, params};
use std::path::{Path, PathBuf};

/// Returns `path` unchanged if it exists, otherwise walks up the ancestor
/// chain until it finds one that does, falling back to the drive root (or
/// whatever the topmost ancestor is) if nothing along the way exists either.
///
/// Used when restoring a saved session: a saved path may point at a deleted
/// folder or an unmounted drive, per FileMan_SPEC.md §4.
pub fn nearest_existing_ancestor(path: &Path) -> PathBuf {
    let mut current = path;
    loop {
        if current.exists() {
            return current.to_path_buf();
        }
        match current.parent() {
            Some(parent) if parent != current => current = parent,
            _ => return current.to_path_buf(), // reached the root; return it even if it doesn't exist (nothing else to fall back to)
        }
    }
}

pub struct WindowGeometry {
    pub width: f32,
    pub height: f32,
    pub pos_x: Option<f32>,
    pub pos_y: Option<f32>,
    pub monitor_name: Option<String>,
}

pub struct LoadedSession {
    pub window: Option<WindowGeometry>,
    pub panes: Vec<Pane>,
    pub active_pane: usize,
}

/// Serializes column widths as space-separated floats for DB storage.
fn format_col_widths(widths: &[f32; 4]) -> String {
    widths
        .iter()
        .map(|w| format!("{w:.1}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parses the stored column-widths string, falling back to defaults on any
/// malformed or missing data.
fn parse_col_widths(raw: &str) -> [f32; 4] {
    let mut out = crate::tab::DEFAULT_COL_WIDTHS;
    for (slot, part) in out.iter_mut().zip(raw.split_whitespace()) {
        if let Ok(w) = part.parse::<f32>() {
            *slot = w.clamp(20.0, 4000.0);
        }
    }
    out
}

pub fn save_session(
    conn: &Connection,
    user_id: i64,
    window: &WindowGeometry,
    panes: &[Pane],
    active_pane: usize,
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;

    tx.execute(
        "INSERT INTO window_state (user_id, width, height, pos_x, pos_y, monitor_name)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(user_id) DO UPDATE SET width=?2, height=?3, pos_x=?4, pos_y=?5, monitor_name=?6",
        params![
            user_id,
            window.width,
            window.height,
            window.pos_x,
            window.pos_y,
            window.monitor_name
        ],
    )?;

    tx.execute("DELETE FROM panes WHERE user_id = ?1", params![user_id])?;
    for (pane_idx, pane) in panes.iter().enumerate() {
        for (tab_idx, tab) in pane.tabs.iter().enumerate() {
            tx.execute(
                "INSERT INTO panes (user_id, pane_index, tab_index, path, is_active_tab, sort_col, sort_asc, col_widths, view_mode)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    user_id,
                    pane_idx as i64,
                    tab_idx as i64,
                    tab.path.to_string_lossy(),
                    (tab_idx == pane.active_tab) as i64,
                    tab.sort_col,
                    tab.sort_asc as i64,
                    format_col_widths(&tab.col_widths),
                    tab.view_mode.as_str(),
                ],
            )?;
        }
    }

    tx.execute(
        "INSERT INTO app_state (user_id, active_pane) VALUES (?1, ?2)
         ON CONFLICT(user_id) DO UPDATE SET active_pane=?2",
        params![user_id, active_pane as i64],
    )?;

    tx.commit()?;
    Ok(())
}

pub fn load_session(conn: &Connection, user_id: i64) -> Result<Option<LoadedSession>> {
    let window = conn
        .query_row(
            "SELECT width, height, pos_x, pos_y, monitor_name FROM window_state WHERE user_id = ?1",
            params![user_id],
            |row| {
                Ok(WindowGeometry {
                    width: row.get(0)?,
                    height: row.get(1)?,
                    pos_x: row.get(2)?,
                    pos_y: row.get(3)?,
                    monitor_name: row.get(4)?,
                })
            },
        )
        .ok();

    let mut stmt = conn.prepare(
        "SELECT pane_index, path, is_active_tab, sort_col, sort_asc, col_widths, view_mode
         FROM panes WHERE user_id = ?1 ORDER BY pane_index, tab_index",
    )?;
    let rows: Vec<(i64, String, bool, String, bool, String, String)> = stmt
        .query_map(params![user_id], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get::<_, i64>(2)? == 1,
                row.get(3)?,
                row.get::<_, i64>(4)? == 1,
                row.get(5)?,
                row.get(6)?,
            ))
        })?
        .collect::<Result<Vec<_>>>()?;

    if rows.is_empty() {
        return Ok(None);
    }

    let pane_count = rows
        .iter()
        .map(|(idx, ..)| *idx)
        .max()
        .unwrap() as usize
        + 1;
    let mut panes: Vec<Option<Pane>> = (0..pane_count).map(|_| None).collect();

    for (pane_idx, path, is_active, sort_col, sort_asc, col_widths, view_mode) in rows {
        let pane_idx = pane_idx as usize;
        let pane = panes[pane_idx].get_or_insert_with(|| Pane {
            tabs: Vec::new(),
            active_tab: 0,
            address_bar: String::new(),
        });
        let resolved_path = nearest_existing_ancestor(&PathBuf::from(path));
        let mut tab = Tab::new(resolved_path);
        tab.sort_col = sort_col;
        tab.sort_asc = sort_asc;
        tab.col_widths = parse_col_widths(&col_widths);
        tab.view_mode = crate::tab::ViewMode::from_str(&view_mode);
        pane.tabs.push(tab);
        if is_active {
            pane.active_tab = pane.tabs.len() - 1;
        }
    }

    // Set each pane's address bar to its active tab's path
    for pane_opt in &mut panes {
        if let Some(pane) = pane_opt {
            if let Some(active_tab) = pane.tabs.get(pane.active_tab) {
                pane.address_bar = active_tab.path.display().to_string();
            }
        }
    }

    let panes: Vec<Pane> = panes
        .into_iter()
        .map(|p| p.unwrap_or_else(|| Pane::new(PathBuf::from("C:\\"))))
        .collect();

    let active_pane = conn
        .query_row(
            "SELECT active_pane FROM app_state WHERE user_id = ?1",
            params![user_id],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0) as usize;
    let active_pane = active_pane.min(panes.len().saturating_sub(1));

    Ok(Some(LoadedSession {
        window,
        panes,
        active_pane,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_db;

    #[test]
    fn round_trips_panes_and_window() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let mut pane0 = Pane::new(PathBuf::from("C:\\Users"));
        pane0.open_tab(PathBuf::from("C:\\Windows"));
        let pane1 = Pane::new(PathBuf::from("D:\\"));

        let window = WindowGeometry {
            width: 1200.0,
            height: 800.0,
            pos_x: Some(50.0),
            pos_y: Some(60.0),
            monitor_name: Some("\\\\.\\DISPLAY1".to_string()),
        };

        save_session(&conn, 1, &window, &[pane0, pane1], 1).unwrap();

        let loaded = load_session(&conn, 1).unwrap().expect("session should exist");

        assert_eq!(loaded.panes.len(), 2);
        assert_eq!(loaded.panes[0].tabs.len(), 2);
        assert_eq!(loaded.panes[0].tabs[1].path, PathBuf::from("C:\\Windows"));
        assert_eq!(loaded.panes[0].active_tab, 1);
        assert_eq!(loaded.panes[1].tabs[0].path, PathBuf::from("D:\\"));
        assert_eq!(loaded.active_pane, 1);
        assert_eq!(loaded.window.unwrap().width, 1200.0);
    }

    #[test]
    fn returns_none_when_no_session_saved() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        assert!(load_session(&conn, 1).unwrap().is_none());
    }

    #[test]
    fn round_trips_per_tab_sort_settings() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let mut pane0 = Pane::new(PathBuf::from("C:\\Users"));
        pane0.tabs[0].sort_col = "size".to_string();
        pane0.tabs[0].sort_asc = false;

        save_session(
            &conn,
            1,
            &WindowGeometry {
                width: 1000.0,
                height: 700.0,
                pos_x: None,
                pos_y: None,
                monitor_name: None,
            },
            &[pane0],
            0,
        )
        .unwrap();

        let loaded = load_session(&conn, 1).unwrap().expect("session should exist");
        assert_eq!(loaded.panes[0].tabs[0].sort_col, "size");
        assert!(!loaded.panes[0].tabs[0].sort_asc);
    }

    #[test]
    fn round_trips_column_widths() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let mut pane0 = Pane::new(PathBuf::from("C:\\Users"));
        pane0.tabs[0].col_widths = [300.5, 120.0, 80.0, 45.0];

        save_session(
            &conn,
            1,
            &WindowGeometry {
                width: 1000.0,
                height: 700.0,
                pos_x: None,
                pos_y: None,
                monitor_name: None,
            },
            &[pane0],
            0,
        )
        .unwrap();

        let loaded = load_session(&conn, 1).unwrap().expect("session should exist");
        assert_eq!(loaded.panes[0].tabs[0].col_widths, [300.5, 120.0, 80.0, 45.0]);
    }

    #[test]
    fn parse_col_widths_falls_back_to_defaults_on_garbage() {
        assert_eq!(
            super::parse_col_widths("not numbers at all"),
            crate::tab::DEFAULT_COL_WIDTHS
        );
        // Partial garbage keeps the valid prefix and defaults the rest.
        let parsed = super::parse_col_widths("100 oops 50");
        assert_eq!(parsed[0], 100.0);
        assert_eq!(parsed[2], 50.0);
        assert_eq!(parsed[3], crate::tab::DEFAULT_COL_WIDTHS[3]);
    }

    #[test]
    fn nearest_existing_ancestor_returns_path_unchanged_when_it_exists() {
        let dir = tempfile::tempdir().unwrap();
        let resolved = nearest_existing_ancestor(dir.path());
        assert_eq!(resolved, dir.path());
    }

    #[test]
    fn nearest_existing_ancestor_falls_back_to_existing_parent() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing_subfolder");
        let resolved = nearest_existing_ancestor(&missing);
        assert_eq!(resolved, dir.path());
    }

    #[test]
    fn nearest_existing_ancestor_falls_back_several_levels_to_existing_parent() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("a").join("b").join("c");
        let resolved = nearest_existing_ancestor(&missing);
        assert_eq!(resolved, dir.path());
    }

    #[test]
    fn nearest_existing_ancestor_terminates_on_a_fully_nonexistent_path() {
        // Don't assume any particular real drive letter is absent (drive
        // presence varies by machine). Instead exercise the "no parent"
        // termination case directly with a relative, synthetic path that
        // certainly doesn't exist as given: a relative path's ancestor chain
        // bottoms out at "" (whose parent is itself), so this proves the
        // loop terminates via the `parent != current` guard rather than
        // hanging, matching how a nonexistent drive root would behave.
        let path = PathBuf::from("definitely_missing_xyz/also_missing/leaf");
        let resolved = nearest_existing_ancestor(&path);
        // Must terminate and return *something* rather than looping forever.
        // It should be an ancestor of (or equal to) the original path.
        assert!(path.starts_with(&resolved) || resolved == path);
    }

    #[test]
    fn two_users_sessions_round_trip_without_cross_contamination() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO users (id, name, created_at) VALUES (2, 'Alice', datetime('now'))",
            [],
        )
        .unwrap();

        // Use real existing directories: load_session resolves a saved path
        // that no longer exists to its nearest existing ancestor, so a
        // fictitious path wouldn't round-trip exactly.
        let dir1 = tempfile::tempdir().unwrap();
        let dir2 = tempfile::tempdir().unwrap();

        let window = WindowGeometry {
            width: 1000.0,
            height: 700.0,
            pos_x: None,
            pos_y: None,
            monitor_name: None,
        };
        save_session(&conn, 1, &window, &[Pane::new(dir1.path().to_path_buf())], 0).unwrap();
        save_session(&conn, 2, &window, &[Pane::new(dir2.path().to_path_buf())], 0).unwrap();

        let loaded1 = load_session(&conn, 1).unwrap().expect("user 1 session");
        let loaded2 = load_session(&conn, 2).unwrap().expect("user 2 session");
        assert_eq!(loaded1.panes[0].tabs[0].path, dir1.path());
        assert_eq!(loaded2.panes[0].tabs[0].path, dir2.path());
    }

    #[test]
    fn load_session_clamps_out_of_range_active_pane() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // Save with only 1 pane, but manually corrupt active_pane to be out of range.
        let pane0 = Pane::new(PathBuf::from("C:\\"));
        save_session(
            &conn,
            1,
            &WindowGeometry {
                width: 800.0,
                height: 600.0,
                pos_x: None,
                pos_y: None,
                monitor_name: None,
            },
            &[pane0],
            0,
        )
        .unwrap();
        conn.execute("UPDATE app_state SET active_pane = 5 WHERE user_id = 1", [])
            .unwrap();

        let loaded = load_session(&conn, 1).unwrap().expect("session should exist");
        assert_eq!(loaded.active_pane, 0); // clamped to the only valid index
    }
}
