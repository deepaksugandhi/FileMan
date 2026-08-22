use crate::pane::Pane;
use crate::tab::Tab;
use rusqlite::{params, Connection, Result};
use std::path::PathBuf;

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
        params![window.width, window.height, window.pos_x, window.pos_y, window.monitor_name],
    )?;

    tx.execute("DELETE FROM panes", [])?;
    for (pane_idx, pane) in panes.iter().enumerate() {
        for (tab_idx, tab) in pane.tabs.iter().enumerate() {
            tx.execute(
                "INSERT INTO panes (pane_index, tab_index, path, is_active_tab)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    pane_idx as i64,
                    tab_idx as i64,
                    tab.path.to_string_lossy(),
                    (tab_idx == pane.active_tab) as i64
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
        "SELECT pane_index, path, is_active_tab FROM panes ORDER BY pane_index, tab_index",
    )?;
    let rows: Vec<(i64, String, bool)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get::<_, i64>(2)? == 1)))?
        .collect::<Result<Vec<_>>>()?;

    if rows.is_empty() {
        return Ok(None);
    }

    let pane_count = rows.iter().map(|(idx, _, _)| *idx).max().unwrap() as usize + 1;
    let mut panes: Vec<Option<Pane>> = (0..pane_count).map(|_| None).collect();

    for (pane_idx, path, is_active) in rows {
        let pane_idx = pane_idx as usize;
        let pane = panes[pane_idx].get_or_insert_with(|| Pane {
            tabs: Vec::new(),
            active_tab: 0,
        });
        pane.tabs.push(Tab::new(PathBuf::from(path)));
        if is_active {
            pane.active_tab = pane.tabs.len() - 1;
        }
    }

    let panes: Vec<Pane> = panes
        .into_iter()
        .map(|p| p.unwrap_or_else(|| Pane::new(PathBuf::from("C:\\"))))
        .collect();

    let active_pane = conn
        .query_row("SELECT active_pane FROM app_state WHERE id = 1", [], |row| {
            row.get::<_, i64>(0)
        })
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
    fn load_session_clamps_out_of_range_active_pane() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // Save with only 1 pane, but manually corrupt active_pane to be out of range.
        let pane0 = Pane::new(PathBuf::from("C:\\"));
        save_session(
            &conn,
            &WindowGeometry { width: 800.0, height: 600.0, pos_x: None, pos_y: None, monitor_name: None },
            &[pane0],
            0,
        )
        .unwrap();
        conn.execute("UPDATE app_state SET active_pane = 5 WHERE id = 1", []).unwrap();

        let loaded = load_session(&conn).unwrap().expect("session should exist");
        assert_eq!(loaded.active_pane, 0); // clamped to the only valid index
    }
}
