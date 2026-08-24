# FileMan — P0/P1/P2 implementation plan

## Context

`FileMan_SPEC.md` §15 tracks the app against the original PRD. The bug-fix
passes and P2 archive/search/progress work are done. What's left, grouped by
the SPEC's own priority tiers:

- **P0** (blocking correctness/UX issues): directory listing blocks the UI
  thread every frame, the pane split is fixed (no resizable divider), window
  position isn't tied to a specific monitor.
- **P1** (PRD items never started): multi-user profiles, global-vs-per-user
  config, configurable shortcuts *and* a fully customizable toolbar (per
  user's explicit choice over the smaller "shortcuts only" option).
- **P2 gap**: `search::recursive_search` exists but has no caller — Find
  currently uses a separate, synchronous, UI-blocking duplicate
  (`FileManApp::find_files`).

This plan implements all of them, in the order below (each tier de-risks the
next: P1's per-user session load reuses P0's listing-cache invalidation
hooks; P1.3's Action registry is what both the shortcut rebinder and the
toolbar editor are built on).

## P0.1 — Non-blocking, cached directory listing

- `Tab` (tab.rs) gains `listing: Vec<FsEntry>` and `listing_dirty: bool`
  (`true` in `Tab::new`). `navigate_to`/`go_back`/`go_forward` already reset
  selection/filter — add `listing_dirty = true` there too.
- `FileManApp` (app.rs) gains `listing_jobs: [Option<ListingJob>; 2]` (one
  slot per pane — only the active tab of each pane needs a live listing).
  `ListingJob { dir: PathBuf, rx: mpsc::Receiver<io::Result<Vec<FsEntry>>> }`,
  spawned with `thread::spawn` (same shape as `progress::copy_items_bg`).
- Per pane, per frame: if `listing_dirty` and no job in flight, spawn one and
  clear the flag; if a job's receiver has a result, and the active tab's path
  still matches the job's dir (else the user navigated away — discard),
  store it into `tab.listing`. Call `ctx.request_repaint()` while any job is
  pending so results land promptly even without input.
- Replace the current `crate::fs_entry::list_dir(&current_path)` call (used
  directly in the render loop, app.rs ~1124) with reading `tab.listing`.
- After any `fs_ops`/`archive`/paste/delete mutation of a directory, mark
  `listing_dirty = true` on every tab (both panes) whose path equals the
  affected directory, plus an F5 refresh binding (added to the global
  shortcut block, app.rs ~617) that does the same for the active tab.
- Touches: `tab.rs`, `app.rs`. No schema change.

## P0.2 — Resizable pane split

- Add `split_ratio: f32` (default 0.5) to `FileManApp`; persist via a new
  `app_state.split_ratio` column + `db::get_split_ratio`/`set_split_ratio`
  (same upsert pattern as `set_theme`).
- Replace `ui.columns(2, |columns| { ... })` (app.rs:916) with a manual
  layout: compute `total_rect = ui.available_rect_before_wrap()`, split it
  into `left_rect` / `divider_rect` (6px, `Sense::drag()`) / `right_rect`
  using `self.split_ratio`, clamped to `0.15..=0.85`. Render each pane's
  existing `.group(|ui| {...})` content via
  `ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {...})` (confirmed
  available in installed egui 0.36.1). Dragging the divider updates
  `split_ratio` from `drag_delta().x / total_rect.width()` and sets
  `self.dirty = true`.
- Touches: `app.rs`, `db.rs`, `session.rs` (persist/load `split_ratio`
  alongside the existing window/pane save call).

## P0.3 — Monitor-aware window position

- `eframe::Frame::window_handle()` (available in the `ui()` method's
  `frame` param) returns a `raw-window-handle` `WindowHandle`, giving the
  real HWND on Windows (`RawWindowHandle::Win32(Win32WindowHandle)`). Add
  `raw-window-handle = "0.6"` (already pinned transitively at 0.6.2 —
  matches) and extend the existing `windows` dependency with
  `Win32_Graphics_Gdi` (for `MonitorFromWindow`/`GetMonitorInfoW`) and
  `Win32_Foundation`.
- New helper (in `main.rs` or a small `monitor.rs`): given the HWND, call
  `MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST)` then
  `GetMonitorInfoW` to read `szDevice` (e.g. `"\\.\DISPLAY1"`) and the work
  area rect — this is the real stable-ish device name the SPEC asked for.
- On persist (`FileManApp::persist`), compute the monitor name once per
  position change and store it in `WindowGeometry.monitor_name` (column
  already exists).
- On restore (`main.rs`, before `run_native`): enumerate monitors
  (`EnumDisplayMonitors`) and their device names/work areas. If the saved
  `monitor_name` matches one still connected, use the saved `pos_x/pos_y`
  clamped into that monitor's work area; otherwise center on the primary
  monitor. This satisfies both SPEC bullets (match-by-identifier, and
  don't-restore-off-screen).
- Touches: `main.rs`, `session.rs` (no schema change — columns exist).

## P1.1 — Multi-user profiles

- New `users` table (`id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT UNIQUE
  NOT NULL, created_at TEXT NOT NULL, is_default INTEGER NOT NULL DEFAULT
  0`), seeded with one `"Default"` user on first run.
- `window_state`, `panes`, `app_state`, `favourites` all need a `user_id`
  scope. Since `panes`' PK is `(pane_index, tab_index)` and
  `window_state`/`app_state` are singleton rows (`id INTEGER PRIMARY KEY
  CHECK (id=1)`), a plain `ALTER TABLE ADD COLUMN` isn't enough — two
  users' `pane_index=0,tab_index=0` rows would collide. `init_db` performs a
  one-time table-recreate migration for each of the four tables, guarded by
  `PRAGMA table_info(...)` (skip if `user_id` already present, so this is
  idempotent like the existing `ALTER ... ADD COLUMN` migrations):
  `RENAME TO ..._old` → `CREATE` with `user_id` folded into the primary key
  → `INSERT INTO new SELECT 1, * FROM ..._old` (existing data becomes user
  1's) → `DROP TABLE ..._old`. This is schema-only migration on a local
  SQLite file; existing session data is preserved, not discarded.
- `session.rs`: `save_session`/`load_session` take a `user_id: i64` param
  and filter every query by it.
- New `user.rs`: `list_users`, `create_user`, `default_user_id`, and a
  `switch_to(app, conn, user_id)` helper used by app.rs that persists the
  current user's state, loads the target's session, and replaces
  `panes`/`active_pane`/`split_ratio` (theme/font ride along for free once
  P1.2 makes `app_state` per-user via the same migration).
- UI: a combo box in the toolbar row next to ⚙ Settings, listing users +
  "New User…" (opens a `Dialog::NewUser { name }` text-entry dialog, same
  shape as the existing `NewFolder`/`NewFile` dialogs).
- Touches: `db.rs`, `session.rs`, new `user.rs`, `app.rs`.

## P1.2 — Two-tier config (global vs. per-user)

- New tables: `global_settings(key TEXT PRIMARY KEY, value TEXT)`,
  `user_settings(user_id INTEGER, key TEXT, value TEXT, PRIMARY
  KEY(user_id,key))`. Seed `global_settings` with today's hardcoded
  defaults (`theme='system'`, `font_size='14.0'`, `font_family='Inter'`) on
  first run.
- New `config.rs`: `get(conn, user_id, key) -> Option<String>` (checks
  `user_settings` then falls back to `global_settings`), `set(conn, scope,
  key, value)` where `scope` is `Scope::Global | Scope::User(id)`.
- Replace `db::get_theme`/`set_theme`/`get_font_size`/`set_font_size`/
  `get_font_family`/`set_font_family` call sites in `app.rs` with
  `config::get`/`set` using keys `"theme"`/`"font_size"`/`"font_family"`,
  writing to `Scope::User(current_user_id)` (the existing Settings dialog
  becomes the per-user override surface — no separate global-settings admin
  UI in this pass; global values stay at their seeded defaults unless a
  later need justifies an editor for them).
- Touches: new `config.rs`, `db.rs`, `app.rs`.

## P1.3 — Action registry, rebindable shortcuts, and a customizable toolbar

This is the largest single piece; it replaces both today's hardcoded
`if ctrl && key_pressed(...)` shortcut chain (app.rs ~617-635) and the
hardcoded toolbar button block (app.rs ~648-765) with one data-driven
system.

- New `actions.rs`:
  - `Action` enum — the fixed built-in registry: `Copy, Cut, Paste, Delete,
    Rename, NewFolder, NewFile, CopyFilename, CopyFolderPath, ExtractHere,
    ExtractTo, ToggleFavourite, GoBack, GoForward, GoUp, NewTab, CloseTab,
    Refresh, Find, ToggleSettings`. Each has `.id()` (stable string for DB
    storage), `.label()`, `.default_shortcut() -> Option<KeyCombo>`.
  - `KeyCombo { ctrl, shift, alt, key: egui::Key }` with
    `to_string()`/`parse()` (e.g. `"Ctrl+Shift+X"`) for DB round-tripping
    and display in the rebind UI.
  - `ActionRef` — `Builtin(Action) | Custom(i64)` (references
    `custom_actions.id`) — the unit both the shortcut map and the toolbar
    layout are built from.
  - `custom_actions` table (`id INTEGER PRIMARY KEY, label TEXT, exe_path
    TEXT, scope TEXT`) for user-defined "open with `<app>`" buttons,
    launched via `std::process::Command::new(exe_path).arg(selected_path)`.
  - `bindings` table (`scope TEXT, key_combo TEXT, action_id TEXT, PRIMARY
    KEY(scope, key_combo)`); `load_shortcut_map(conn, user_id) ->
    HashMap<KeyCombo, ActionRef>` merges user rows over global rows over
    each `Action`'s hardcoded default. `set_binding(conn, scope, combo,
    action)` rejects (returns the conflicting action) if `combo` is already
    bound to something else in that scope — surfaced as a status message.
  - `toolbar_layout` table (`scope TEXT, position INTEGER, action_id TEXT,
    PRIMARY KEY(scope, position))`; `get_layout`/`set_layout`, seeded with
    today's actual toolbar order as the global default so existing behavior
    doesn't change until a user customizes it.
  - `dispatch(app: &mut FileManApp, action: ActionRef)` — the single match
    that calls the existing methods (`copy_selection`, `cut_selection`,
    `begin_rename`, ... already implemented in `app.rs`) or, for
    `Custom(id)`, spawns the stored executable.
- `app.rs` changes:
  - `self.shortcut_map: HashMap<KeyCombo, ActionRef>` and
    `self.toolbar_actions: Vec<ActionRef>`, both loaded in `new()` and
    reloaded on user switch / after a rebind or toolbar edit.
  - The hardcoded shortcut `if` chain becomes: iterate `shortcut_map`,
    check each combo against `i.modifiers`/`i.key_pressed`, call
    `actions::dispatch` on match.
  - The hardcoded toolbar `ui.horizontal(|ui| { ui.button("Copy")... })`
    block becomes a loop over `self.toolbar_actions` rendering
    `ui.button(action_ref.label(&self.custom_actions))` →
    `actions::dispatch` on click. Enabled/disabled state (e.g. Extract
    buttons only for a single selected archive) stays a per-`Action` check
    inside the loop, same conditions as today, just table-driven instead of
    copy-pasted per button.
  - New Settings sections:
    - **Shortcuts**: list of all `Action`s with their current combo and a
      "Rebind" button; clicking sets `self.capturing_shortcut_for =
      Some(action)`, and the next frame's key event (read from
      `ctx.input(|i| i.events.clone())`) is captured, conflict-checked via
      `set_binding`, and saved.
    - **Toolbar**: an ordered checklist of all `ActionRef`s (builtins +
      the user's custom "open with" actions) with ▲/▼ reorder buttons and
      an include/exclude checkbox — no new drag-and-drop dependency, saved
      to `toolbar_layout` on any change.
    - **Custom Actions**: an "Add…" button opens an `rfd` file picker for
      the target executable plus a label field, appending a row to
      `custom_actions` (and, implicitly, made available to add to the
      toolbar/shortcuts).
- Touches: new `actions.rs`, `db.rs`, `app.rs` (toolbar construction,
  global shortcut handling, and Settings window all route through the new
  registry).

## P2 — Wire up recursive search, drop the duplicate blocking walk

- `FileManApp::find_files` (app.rs ~219, a synchronous recursive
  `std::fs::read_dir` walk) duplicates `search::recursive_search` and
  blocks the UI thread on every keystroke of the Find dialog — delete it.
- `Dialog::Find` search now triggers `search::recursive_search` on a
  background thread (same channel-polling shape as P0.1's listing jobs):
  `find_job: Option<mpsc::Receiver<Vec<FsEntry>>>` on `FileManApp`, spawned
  when the user presses Enter/a Search button in the dialog, polled each
  frame (with `ctx.request_repaint()` while pending) to fill
  `Dialog::Find.results`.
- Touches: `app.rs` (delete `find_files`, rewire the Find dialog's trigger
  and result-population), `search.rs` unchanged (already correct, just
  finally called).

## Verification

- `cargo build` after each tier (P0, then P1.1, P1.2, P1.3, then P2) — keep
  the tree compiling at each checkpoint rather than one big-bang change.
- `cargo test` — existing suite (tab/pane/session/db/progress/search tests)
  must keep passing; add tests alongside new logic the same way the
  existing modules do:
  - P0.1: a test that `listing_dirty` starts `true` and is set by
    `navigate_to`/`go_back`/`go_forward`.
  - P0.3: a pure-logic test for the clamp-into-work-area math (no real
    HWND needed in tests).
  - P1.1: `db.rs`/`session.rs` tests round-tripping two users' sessions
    without cross-contamination (mirrors the existing
    `round_trips_panes_and_window` test shape).
  - P1.2: `config.rs` tests for user-override-falls-back-to-global-falls-
    back-to-None precedence.
  - P1.3: `actions.rs` tests for conflict rejection on `set_binding` and
    for shortcut-map merge precedence (user > global > default).
- Manual run (`cargo run`) to confirm: dragging the pane divider resizes
  live and survives a restart; a large directory no longer freezes the UI
  on navigate; creating a second user and switching between them keeps
  sessions/theme separate; rebinding a shortcut and reordering the toolbar
  both persist across restart; Ctrl+F actually returns recursive results
  without freezing.
