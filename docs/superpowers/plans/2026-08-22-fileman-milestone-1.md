# Speed FileMan Milestone 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a working Speed FileMan window with dual panes, tabs, a folder-tree
sidebar, directory listing/navigation, and SQLite-backed session save/restore —
covering `FileMan_SPEC.md` §3 (Core Windowing & Panes) and §4 (Session Persistence,
basic tier).

**Architecture:** Single-crate Rust binary. `eframe`/`egui` for the UI (immediate
mode, no webview). `rusqlite` (bundled SQLite) for session storage. UI code lives in
`app.rs`; everything else (`fs_entry.rs`, `tab.rs`, `pane.rs`, `tree.rs`, `db.rs`,
`session.rs`) is plain Rust with no `egui` dependency, so it's unit-testable without
spinning up a window.

**Tech Stack:** Rust (stable), `eframe`/`egui`, `rusqlite` (bundled feature),
`tempfile` (dev-dependency for filesystem tests).

**Scope note (read before starting):** Per `FileMan_SPEC.md` §4, window position
should ideally remember *which monitor* it was on, and DPI scaling should follow
`egui`'s automatic per-monitor handling. This plan persists window **size** (and, if
straightforward once the exact `eframe` version is pinned in Task 1, position) but
does **not** implement full monitor-identity matching or live DPI-changed event
handling — that requires confirming exact `winit`/`eframe` monitor APIs against the
version actually resolved by `cargo add`, which isn't safe to hardcode sight unseen.
DPI scaling itself needs no extra code (`egui` handles it by default; we just must
not undo that by storing physical-pixel sizes — this plan stores logical/point
sizes throughout). Full monitor-ID persistence is a follow-up plan once Task 1's
lockfile shows the resolved `eframe` version.

Multi-window taskbar badges (§11), multi-user profiles (§5), global/per-user config
(§6), shortcuts (§7), file operations (§8), and advanced features (§9) are **out of
scope for this plan** — they get their own follow-up plans.

---

## File Structure

- `Cargo.toml` — crate manifest, created by `cargo init`.
- `src/main.rs` — entry point: opens the DB, loads any saved session, launches the
  `eframe` window.
- `src/app.rs` — `FileManApp`, the `eframe::App` implementation: layout (tree
  sidebar + two panes), input handling, dirty-flag-triggered autosave.
- `src/fs_entry.rs` — `FsEntry` struct + `list_dir()`: reads a directory into a
  sorted `Vec<FsEntry>`. No `egui` dependency.
- `src/tab.rs` — `Tab` struct: current path, back/forward history, sort/view state.
  No `egui` dependency.
- `src/pane.rs` — `Pane` struct: a list of `Tab`s + which one is active, with
  open/close-tab operations. No `egui` dependency.
- `src/tree.rs` — `list_drives()`: enumerates available drive letters.
- `src/db.rs` — `open_db()` / `init_db()`: SQLite connection + schema creation.
- `src/session.rs` — `WindowGeometry`, `LoadedSession`, `save_session()`,
  `load_session()`: serializes/deserializes app state to/from the DB.

---

### Task 1: Project Scaffold

**Files:**
- Create: `Cargo.toml` (via `cargo init`)
- Create: `src/main.rs`

- [ ] **Step 1: Initialize git and the cargo project**

Run (from `E:\Projects\FileMan`):
```bash
git init
cargo init --name fileman
```
Expected: `Cargo.toml` and `src/main.rs` (a default "Hello, world!" one) are
created; `git status` shows them as untracked.

- [ ] **Step 2: Add dependencies**

Run:
```bash
cargo add eframe egui
cargo add rusqlite --features bundled
cargo add tempfile --dev
```
Expected: `Cargo.toml` now lists `eframe`, `egui`, `rusqlite` under
`[dependencies]` and `tempfile` under `[dev-dependencies]`. Note the exact
versions `cargo add` resolves (shown in its output) — needed later if we revisit
the monitor-identity follow-up plan.

- [ ] **Step 3: Write a minimal window**

Replace `src/main.rs` with:
```rust
use eframe::egui;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions::default();
    eframe::run_native(
        "Speed FileMan",
        options,
        Box::new(|_cc| Ok(Box::new(FileManApp::default()))),
    )
}

#[derive(Default)]
struct FileManApp {}

impl eframe::App for FileManApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.label("Speed FileMan");
        });
    }
}
```

- [ ] **Step 4: Verify it builds and runs**

Run: `cargo run`
Expected: compiles cleanly; a window titled "Speed FileMan" opens showing the
label. If the closure signature in Step 3 doesn't match the resolved `eframe`
version's `AppCreator` type, the compiler error will name the expected signature
— adjust the closure to match, then re-run.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/main.rs .gitignore
git commit -m "chore: scaffold eframe project"
```
(If no `.gitignore` exists yet, run `cargo init` output already created one via
Step 1 — confirm `target/` is in it before committing.)

---

### Task 2: Directory Listing (`fs_entry.rs`)

**Files:**
- Create: `src/fs_entry.rs`
- Modify: `src/main.rs` (add `mod fs_entry;`)

- [ ] **Step 1: Write the failing tests**

Create `src/fs_entry.rs`:
```rust
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq)]
pub struct FsEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

pub fn list_dir(_dir: &Path) -> io::Result<Vec<FsEntry>> {
    unimplemented!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    #[test]
    fn lists_dirs_first_then_files_alphabetically() {
        let dir = tempfile::tempdir().unwrap();
        File::create(dir.path().join("b.txt")).unwrap();
        File::create(dir.path().join("a.txt")).unwrap();
        fs::create_dir(dir.path().join("z_folder")).unwrap();

        let entries = list_dir(dir.path()).unwrap();

        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].name, "z_folder");
        assert!(entries[0].is_dir);
        assert_eq!(entries[1].name, "a.txt");
        assert!(!entries[1].is_dir);
        assert_eq!(entries[2].name, "b.txt");
    }

    #[test]
    fn errors_on_missing_dir() {
        let result = list_dir(Path::new("Z:\\definitely_missing_path_xyz"));
        assert!(result.is_err());
    }
}
```

Add to `src/main.rs` (near the top): `mod fs_entry;`

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test fs_entry`
Expected: FAIL — `lists_dirs_first_then_files_alphabetically` panics with
"not implemented" (from `unimplemented!()`).

- [ ] **Step 3: Implement `list_dir`**

Replace the `list_dir` function body in `src/fs_entry.rs`:
```rust
pub fn list_dir(dir: &Path) -> io::Result<Vec<FsEntry>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        entries.push(FsEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            path: entry.path(),
            is_dir: metadata.is_dir(),
            size: metadata.len(),
            modified: metadata.modified().ok(),
        });
    }
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test fs_entry`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add src/fs_entry.rs src/main.rs
git commit -m "feat: add directory listing"
```

---

### Task 3: Tab Navigation (`tab.rs`)

**Files:**
- Create: `src/tab.rs`
- Modify: `src/main.rs` (add `mod tab;`)

- [ ] **Step 1: Write the failing tests**

Create `src/tab.rs`:
```rust
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub enum ViewMode {
    List,
    Details,
    Icons,
}

#[derive(Debug, Clone)]
pub struct Tab {
    pub path: PathBuf,
    history_back: Vec<PathBuf>,
    history_forward: Vec<PathBuf>,
    pub sort_col: String,
    pub sort_asc: bool,
    pub view_mode: ViewMode,
}

impl Tab {
    pub fn new(path: PathBuf) -> Self {
        Tab {
            path,
            history_back: Vec::new(),
            history_forward: Vec::new(),
            sort_col: "name".to_string(),
            sort_asc: true,
            view_mode: ViewMode::Details,
        }
    }

    pub fn navigate_to(&mut self, _new_path: PathBuf) {
        unimplemented!()
    }

    pub fn go_back(&mut self) -> bool {
        unimplemented!()
    }

    pub fn go_forward(&mut self) -> bool {
        unimplemented!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigate_to_updates_path_and_records_history() {
        let mut tab = Tab::new(PathBuf::from("C:\\a"));
        tab.navigate_to(PathBuf::from("C:\\a\\b"));
        assert_eq!(tab.path, PathBuf::from("C:\\a\\b"));
    }

    #[test]
    fn go_back_returns_to_previous_path() {
        let mut tab = Tab::new(PathBuf::from("C:\\a"));
        tab.navigate_to(PathBuf::from("C:\\a\\b"));
        assert!(tab.go_back());
        assert_eq!(tab.path, PathBuf::from("C:\\a"));
    }

    #[test]
    fn go_back_on_empty_history_returns_false_and_keeps_path() {
        let mut tab = Tab::new(PathBuf::from("C:\\a"));
        assert!(!tab.go_back());
        assert_eq!(tab.path, PathBuf::from("C:\\a"));
    }

    #[test]
    fn go_forward_after_go_back_returns_to_next_path() {
        let mut tab = Tab::new(PathBuf::from("C:\\a"));
        tab.navigate_to(PathBuf::from("C:\\a\\b"));
        tab.go_back();
        assert!(tab.go_forward());
        assert_eq!(tab.path, PathBuf::from("C:\\a\\b"));
    }

    #[test]
    fn navigate_to_clears_forward_history() {
        let mut tab = Tab::new(PathBuf::from("C:\\a"));
        tab.navigate_to(PathBuf::from("C:\\a\\b"));
        tab.go_back();
        tab.navigate_to(PathBuf::from("C:\\a\\c"));
        assert!(!tab.go_forward());
    }
}
```

Add to `src/main.rs`: `mod tab;`

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test tab::`
Expected: FAIL — panics with "not implemented" from the `unimplemented!()` bodies.

- [ ] **Step 3: Implement navigation**

Replace the three method bodies in `src/tab.rs`:
```rust
    pub fn navigate_to(&mut self, new_path: PathBuf) {
        self.history_back.push(self.path.clone());
        self.history_forward.clear();
        self.path = new_path;
    }

    pub fn go_back(&mut self) -> bool {
        if let Some(prev) = self.history_back.pop() {
            self.history_forward.push(self.path.clone());
            self.path = prev;
            true
        } else {
            false
        }
    }

    pub fn go_forward(&mut self) -> bool {
        if let Some(next) = self.history_forward.pop() {
            self.history_back.push(self.path.clone());
            self.path = next;
            true
        } else {
            false
        }
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test tab::`
Expected: PASS (5 tests).

- [ ] **Step 5: Commit**

```bash
git add src/tab.rs src/main.rs
git commit -m "feat: add tab navigation history"
```

---

### Task 4: Pane Tab Management (`pane.rs`)

**Files:**
- Create: `src/pane.rs`
- Modify: `src/main.rs` (add `mod pane;`)

- [ ] **Step 1: Write the failing tests**

Create `src/pane.rs`:
```rust
use crate::tab::Tab;
use std::path::PathBuf;

pub struct Pane {
    pub tabs: Vec<Tab>,
    pub active_tab: usize,
}

impl Pane {
    pub fn new(initial_path: PathBuf) -> Self {
        Pane {
            tabs: vec![Tab::new(initial_path)],
            active_tab: 0,
        }
    }

    pub fn active_tab(&self) -> &Tab {
        &self.tabs[self.active_tab]
    }

    pub fn active_tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active_tab]
    }

    pub fn open_tab(&mut self, _path: PathBuf) {
        unimplemented!()
    }

    pub fn close_tab(&mut self, _index: usize) {
        unimplemented!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_tab_adds_and_activates_it() {
        let mut pane = Pane::new(PathBuf::from("C:\\a"));
        pane.open_tab(PathBuf::from("C:\\b"));
        assert_eq!(pane.tabs.len(), 2);
        assert_eq!(pane.active_tab, 1);
        assert_eq!(pane.active_tab().path, PathBuf::from("C:\\b"));
    }

    #[test]
    fn close_tab_removes_it_and_keeps_valid_active_index() {
        let mut pane = Pane::new(PathBuf::from("C:\\a"));
        pane.open_tab(PathBuf::from("C:\\b"));
        pane.close_tab(1);
        assert_eq!(pane.tabs.len(), 1);
        assert_eq!(pane.active_tab, 0);
    }

    #[test]
    fn close_tab_refuses_to_close_last_remaining_tab() {
        let mut pane = Pane::new(PathBuf::from("C:\\a"));
        pane.close_tab(0);
        assert_eq!(pane.tabs.len(), 1);
    }
}
```

Add to `src/main.rs`: `mod pane;`

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test pane::`
Expected: FAIL — "not implemented" panics.

- [ ] **Step 3: Implement `open_tab` / `close_tab`**

```rust
    pub fn open_tab(&mut self, path: PathBuf) {
        self.tabs.push(Tab::new(path));
        self.active_tab = self.tabs.len() - 1;
    }

    pub fn close_tab(&mut self, index: usize) {
        if self.tabs.len() <= 1 {
            return;
        }
        self.tabs.remove(index);
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        }
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test pane::`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add src/pane.rs src/main.rs
git commit -m "feat: add pane tab management"
```

---

### Task 5: Drive Enumeration (`tree.rs`)

**Files:**
- Create: `src/tree.rs`
- Modify: `src/main.rs` (add `mod tree;`)

- [ ] **Step 1: Write the failing test**

Create `src/tree.rs`:
```rust
use std::path::PathBuf;

// ponytail: brute-force A-Z scan (26 stat calls) instead of the
// GetLogicalDrives Win32 bitmask API — simple and fast enough for a one-shot
// sidebar populate. Switch to the Win32 call if this ever shows up in profiling.
pub fn list_drives() -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn includes_c_drive() {
        let drives = list_drives();
        assert!(drives.contains(&PathBuf::from("C:\\")));
    }
}
```

Add to `src/main.rs`: `mod tree;`

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test tree::`
Expected: FAIL — `list_drives()` returns empty, assertion fails.

- [ ] **Step 3: Implement drive enumeration**

```rust
pub fn list_drives() -> Vec<PathBuf> {
    (b'A'..=b'Z')
        .filter_map(|letter| {
            let path = PathBuf::from(format!("{}:\\", letter as char));
            if path.exists() {
                Some(path)
            } else {
                None
            }
        })
        .collect()
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test tree::`
Expected: PASS. (Relies on `C:\` existing on the machine running the test — true
for any standard Windows install, which is this project's only target platform.)

- [ ] **Step 5: Commit**

```bash
git add src/tree.rs src/main.rs
git commit -m "feat: add drive enumeration for folder tree"
```

---

### Task 6: SQLite Schema (`db.rs`)

**Files:**
- Create: `src/db.rs`
- Modify: `src/main.rs` (add `mod db;`)

- [ ] **Step 1: Write the failing test**

Create `src/db.rs`:
```rust
use rusqlite::{Connection, Result};

pub fn init_db(_conn: &Connection) -> Result<()> {
    unimplemented!()
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
```

Add to `src/main.rs`: `mod db;`

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test db::`
Expected: FAIL — "not implemented" panic.

- [ ] **Step 3: Implement schema creation**

```rust
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
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test db::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/db.rs src/main.rs
git commit -m "feat: add SQLite session schema"
```

---

### Task 7: Session Save/Load (`session.rs`)

**Files:**
- Create: `src/session.rs`
- Modify: `src/main.rs` (add `mod session;`)

- [ ] **Step 1: Write the failing tests**

Create `src/session.rs`:
```rust
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
    _conn: &Connection,
    _window: &WindowGeometry,
    _panes: &[Pane],
    _active_pane: usize,
) -> Result<()> {
    unimplemented!()
}

pub fn load_session(_conn: &Connection) -> Result<Option<LoadedSession>> {
    unimplemented!()
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
}
```

Add to `src/main.rs`: `mod session;`

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test session::`
Expected: FAIL — "not implemented" panics.

- [ ] **Step 3: Implement `save_session`**

```rust
pub fn save_session(
    conn: &Connection,
    window: &WindowGeometry,
    panes: &[Pane],
    active_pane: usize,
) -> Result<()> {
    conn.execute(
        "INSERT INTO window_state (id, width, height, pos_x, pos_y, monitor_name)
         VALUES (1, ?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(id) DO UPDATE SET width=?1, height=?2, pos_x=?3, pos_y=?4, monitor_name=?5",
        params![window.width, window.height, window.pos_x, window.pos_y, window.monitor_name],
    )?;

    conn.execute("DELETE FROM panes", [])?;
    for (pane_idx, pane) in panes.iter().enumerate() {
        for (tab_idx, tab) in pane.tabs.iter().enumerate() {
            conn.execute(
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

    conn.execute(
        "INSERT INTO app_state (id, active_pane) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET active_pane=?1",
        params![active_pane as i64],
    )?;

    Ok(())
}
```

- [ ] **Step 4: Implement `load_session`**

```rust
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

    Ok(Some(LoadedSession {
        window,
        panes,
        active_pane,
    }))
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test session::`
Expected: PASS (2 tests). If it fails because `Pane`'s fields aren't visible from
`session.rs`, confirm `src/pane.rs` declares `pub tabs` and `pub active_tab`
(Task 4, Step 1 already does).

- [ ] **Step 6: Commit**

```bash
git add src/session.rs src/main.rs
git commit -m "feat: add session save/load round trip"
```

---

### Task 8: Wire the UI (dual pane, tree, tabs, autosave)

**Files:**
- Create: `src/app.rs`
- Modify: `src/main.rs` (replace the Task 1 placeholder app with real wiring;
  add `mod app;`)

- [ ] **Step 1: Write `FileManApp`**

Create `src/app.rs`:
```rust
use crate::pane::Pane;
use crate::session::{self, WindowGeometry};
use crate::tree;
use eframe::egui;
use rusqlite::Connection;
use std::path::PathBuf;

pub struct FileManApp {
    conn: Connection,
    panes: Vec<Pane>,
    active_pane: usize,
    dirty: bool,
    last_size: egui::Vec2,
}

impl FileManApp {
    pub fn new(conn: Connection, loaded: Option<session::LoadedSession>) -> Self {
        let (panes, active_pane) = match loaded {
            Some(s) if !s.panes.is_empty() => (s.panes, s.active_pane),
            _ => (
                vec![Pane::new(PathBuf::from("C:\\")), Pane::new(PathBuf::from("C:\\"))],
                0,
            ),
        };
        FileManApp {
            conn,
            panes,
            active_pane,
            dirty: false,
            last_size: egui::vec2(1200.0, 800.0),
        }
    }

    // ponytail: writes on every state-changing action rather than debouncing
    // resize-drag events. SQLite writes here are single-row upserts on a local
    // file, cheap enough for a desktop app; add debouncing only if a real
    // resize-storm shows up as jank.
    fn persist(&mut self) {
        let window = WindowGeometry {
            width: self.last_size.x,
            height: self.last_size.y,
            pos_x: None,
            pos_y: None,
            monitor_name: None,
        };
        let _ = session::save_session(&self.conn, &window, &self.panes, self.active_pane);
    }
}

impl eframe::App for FileManApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let screen = ctx.screen_rect().size();
        if (screen - self.last_size).length() > 1.0 {
            self.last_size = screen;
            self.dirty = true;
        }

        egui::SidePanel::left("folder_tree").show(ctx, |ui| {
            ui.heading("Folders");
            for drive in tree::list_drives() {
                if ui.button(drive.display().to_string()).clicked() {
                    self.panes[self.active_pane]
                        .active_tab_mut()
                        .navigate_to(drive.clone());
                    self.dirty = true;
                }
            }
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.columns(2, |columns| {
                for pane_idx in 0..2 {
                    let is_active = pane_idx == self.active_pane;
                    columns[pane_idx].group(|ui| {
                        if ui.selectable_label(is_active, "active").clicked() {
                            self.active_pane = pane_idx;
                            self.dirty = true;
                        }

                        let pane = &mut self.panes[pane_idx];
                        let mut tab_clicked = None;
                        ui.horizontal(|ui| {
                            for (tab_idx, tab) in pane.tabs.iter().enumerate() {
                                let label = tab
                                    .path
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| tab.path.display().to_string());
                                if ui
                                    .selectable_label(tab_idx == pane.active_tab, label)
                                    .clicked()
                                {
                                    tab_clicked = Some(tab_idx);
                                }
                            }
                        });
                        if let Some(idx) = tab_clicked {
                            pane.active_tab = idx;
                        }

                        let current_path = pane.active_tab().path.clone();
                        ui.label(current_path.display().to_string());

                        if ui.button("Up").clicked() {
                            if let Some(parent) = current_path.parent() {
                                pane.active_tab_mut().navigate_to(parent.to_path_buf());
                                self.dirty = true;
                            }
                        }

                        match crate::fs_entry::list_dir(&current_path) {
                            Ok(entries) => {
                                egui::ScrollArea::vertical().show(ui, |ui| {
                                    for entry in entries {
                                        let label = if entry.is_dir {
                                            format!("[dir] {}", entry.name)
                                        } else {
                                            format!("{} ({} bytes)", entry.name, entry.size)
                                        };
                                        if ui.button(label).clicked() && entry.is_dir {
                                            pane.active_tab_mut().navigate_to(entry.path.clone());
                                            self.dirty = true;
                                        }
                                    }
                                });
                            }
                            Err(err) => {
                                ui.colored_label(egui::Color32::RED, format!("Error: {err}"));
                            }
                        }
                    });
                }
            });
        });

        if self.dirty {
            self.persist();
            self.dirty = false;
        }
    }
}
```

- [ ] **Step 2: Wire it up in `main.rs`**

Replace `src/main.rs` entirely with:
```rust
mod app;
mod db;
mod fs_entry;
mod pane;
mod session;
mod tab;
mod tree;

use eframe::egui;

fn db_path() -> std::path::PathBuf {
    let appdata = std::env::var("APPDATA").expect("APPDATA env var not set");
    let dir = std::path::PathBuf::from(appdata).join("FileMan");
    std::fs::create_dir_all(&dir).expect("failed to create app data dir");
    dir.join("fileman.db")
}

fn main() -> eframe::Result<()> {
    let conn = db::open_db(&db_path()).expect("failed to open database");
    let loaded = session::load_session(&conn).ok().flatten();

    let mut viewport = egui::ViewportBuilder::default().with_inner_size([1200.0, 800.0]);
    if let Some(loaded) = &loaded {
        if let Some(window) = &loaded.window {
            viewport = viewport.with_inner_size([window.width, window.height]);
        }
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "Speed FileMan",
        options,
        Box::new(move |_cc| Ok(Box::new(app::FileManApp::new(conn, loaded)))),
    )
}
```

- [ ] **Step 3: Run the full test suite**

Run: `cargo test`
Expected: all previously-passing unit tests (fs_entry, tab, pane, tree, db,
session — 15 tests total) still PASS. (`app.rs` has no unit tests — it's UI
glue, verified manually in the next step.)

- [ ] **Step 4: Manual verification**

Run: `cargo run`
Expected:
- A window opens with a left sidebar listing drive letters (e.g. `C:\`).
- Two side-by-side panes, each showing `C:\`'s contents with `[dir]` prefixes on
  folders.
- Clicking a folder entry navigates that pane into it; clicking "Up" goes back
  to the parent; clicking a drive in the sidebar navigates the currently-active
  pane (click "active" on the other pane first to switch which one responds).
- No panics when navigating into folders with special characters or denied
  permissions (those should show the red "Error: ..." label instead of
  crashing).

- [ ] **Step 5: Commit**

```bash
git add src/app.rs src/main.rs
git commit -m "feat: wire dual-pane UI with folder tree and autosave"
```

---

### Task 9: Verify Session Restore End-to-End

**Files:** none (manual verification of Tasks 1–8 working together)

- [ ] **Step 1: Create a distinguishable session**

Run: `cargo run`. In the left pane, navigate into some nested folder (e.g.
`C:\Windows\System32`). In the right pane, navigate somewhere else (e.g.
`C:\Users`). Close the window normally.

- [ ] **Step 2: Confirm the DB was written**

Run (PowerShell):
```powershell
Get-Item "$env:APPDATA\FileMan\fileman.db"
```
Expected: the file exists and has a non-zero `Length`.

- [ ] **Step 3: Relaunch and confirm restore**

Run: `cargo run`
Expected: the left pane reopens at `C:\Windows\System32` and the right pane at
`C:\Users` — matching Step 1's navigation, not the `C:\` default.

- [ ] **Step 4: Commit (if any fixes were needed)**

If Steps 1–3 required any code fixes, stage and commit them:
```bash
git add -A
git commit -m "fix: session restore edge cases found in manual verification"
```
If no fixes were needed, skip this step — there's nothing to commit.

---

## Follow-up plans (not in this milestone)

- Multi-monitor identity matching + live DPI-changed rescaling (§4 subsections) —
  needs the exact `eframe`/`winit` version pinned in Task 1 to write real API
  calls instead of guessed ones.
- Multi-user profiles (§5) and global/per-user config (§6).
- Configurable shortcuts & action buttons (§7).
- File operations: copy/move/delete/rename/archive/search (§8).
- Advanced features: batch rename, metadata preview pane, checksum/compare (§9).
- Multi-window taskbar overlay badges (§11).
