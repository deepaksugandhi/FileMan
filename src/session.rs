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

pub fn save_session(
    conn: &Connection,
    window: &WindowGeometry,
    panes: &[Pane],
    active_pane: usize,
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;

    tx.execute(
        "INSERT INTO window_state (id, width, height, pos_x, pos_y, monitor_name)
         VALUES (1, ?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(id) DO UPDATE SET width=?1, height=?2, pos_x=?3, pos_y=?4, monitor_name=?5",
        params![
            window.width,
            window.height,
            window.pos_x,
            window.pos_y,
            window.monitor_name
        ],
    )?;

    tx.execute("DELETE FROM panes", [])?;
    for (pane_idx, pane) in panes.iter().enumerate() {
        for (tab_idx, tab) in pane.tabs.iter().enumerate() {
            tx.execute(
                "INSERT INTO panes (pane_index, tab_index, path, is_active_tab, sort_col, sort_asc)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    pane_idx as i64,
                    tab_idx as i64,
                    tab.path.to_string_lossy(),
                    (tab_idx == pane.active_tab) as i64,
                    tab.sort_col,
                    tab.sort_asc as i64
                ],
            )?;
        }
    }

    tx.execute(
        "INSERT INTO app_state (id, active_pane) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET active_pane=?1",
        params![active_pane as i64],
    )?;

    tx.commit()?;
    Ok(())
}

pub fn load_session(conn: &Connection) -> Result<Option<LoadedSession>> {
    let window = conn
        .query_row(
            "SELECT width, height, pos_x, pos_y, monitor_name FROM window_state WHERE id = 1",
            [],
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
        "SELECT pane_index, path, is_active_tab, sort_col, sort_asc
         FROM panes ORDER BY pane_index, tab_index",
    )?;
    let rows: Vec<(i64, String, bool, String, bool)> = stmt
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get::<_, i64>(2)? == 1,
                row.get(3)?,
                row.get::<_, i64>(4)? == 1,
            ))
        })?
        .collect::<Result<Vec<_>>>()?;

    if rows.is_empty() {
        return Ok(None);
    }

    let pane_count = rows.iter().map(|(idx, _, _, _, _)| *idx).max().unwrap() as usize + 1;
    let mut panes: Vec<Option<Pane>> = (0..pane_count).map(|_| None).collect();

    for (pane_idx, path, is_active, sort_col, sort_asc) in rows {
        let pane_idx = pane_idx as usize;
        let pane = panes[pane_idx].get_or_insert_with(|| Pane {
            tabs: Vec::new(),
            active_tab: 0,
        });
        let resolved_path = nearest_existing_ancestor(&PathBuf::from(path));
        let mut tab = Tab::new(resolved_path);
        tab.sort_col = sort_col;
        tab.sort_asc = sort_asc;
        pane.tabs.push(tab);
        if is_active {
            pane.active_tab = pane.tabs.len() - 1;
        }
    }

    let panes: Vec<Pane> = panes
        .into_iter()
        .map(|p| p.unwrap_or_else(|| Pane::new(PathBuf::from("C:\\"))))
        .collect();

    let active_pane = conn
        .query_row(
            "SELECT active_pane FROM app_state WHERE id = 1",
            [],
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

        save_session(&conn, &window, &[pane0, pane1], 1).unwrap();

        let loaded = load_session(&conn).unwrap().expect("session should exist");

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
        assert!(load_session(&conn).unwrap().is_none());
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

        let loaded = load_session(&conn).unwrap().expect("session should exist");
        assert_eq!(loaded.panes[0].tabs[0].sort_col, "size");
        assert!(!loaded.panes[0].tabs[0].sort_asc);
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
    fn load_session_clamps_out_of_range_active_pane() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // Save with only 1 pane, but manually corrupt active_pane to be out of range.
        let pane0 = Pane::new(PathBuf::from("C:\\"));
        save_session(
            &conn,
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
        conn.execute("UPDATE app_state SET active_pane = 5 WHERE id = 1", [])
            .unwrap();

        let loaded = load_session(&conn).unwrap().expect("session should exist");
        assert_eq!(loaded.active_pane, 0); // clamped to the only valid index
    }
}
