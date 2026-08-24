# Speed FileMan — Requirements & Specification

Version: 1.0 (draft) — expands `FileMan_PRD.md` into implementable requirements.


## 15. Next Implementation Plan (Phase 2)

Snapshot as of 2026-08-23. Implemented so far: dual-pane/tabs, session persistence
(window size/position, tabs, sort, column widths, per-tab view mode), copy/cut/
paste/delete/rename/new, sidebar folder tree, dark/light/system theming, a
Settings window (theme + font size/family), a tab context menu, a persistent
right-click file context menu, a duplicate-name-on-paste prompt, and List/
Details/Icons view modes.


### P0 — Fix before building on top of them

1. **Non-blocking, cached directory listing.**
   - Problem: `crate::fs_entry::list_dir(&current_path)` (app.rs, inside the
     per-pane render block) runs synchronously on the UI thread on *every
     frame*, for both panes, even when nothing changed.
   - Plan: add a `listing: Vec<FsEntry>` + `listing_dirty: bool` cache to
     `Tab` (or a sibling struct held by `Pane`). Re-run `list_dir` only when
     `listing_dirty` is set (on `navigate_to`/`go_back`/`go_forward`, on app
     start, after any `fs_ops` call that touches the tab's directory, and on
     an explicit refresh action/hotkey — e.g. F5).
   - For the "must not block the UI thread" half of SPEC §12: move the actual
     read to a background thread using `std::sync::mpsc` (or a small
     `std::thread::spawn` + channel per request), poll the channel each frame
     in `ui()`, and show a lightweight "Loading…" state in the pane while
     waiting. Cancel/ignore a stale in-flight read if the user navigates
     again before it completes (tag each request with a generation counter).
   - Touches: `tab.rs` (or a new `listing.rs`), `pane.rs`, `app.rs`.

2. **Resizable pane split** (SPEC §3).
   - Replace `ui.columns(2, |columns| { ... })` with a manually laid-out pair
     of rects plus a thin draggable divider `Sense::drag()` widget between
     them (egui has no built-in split-pane container in 0.36).
   - Add `split_ratio: f32` (default 0.5) to `FileManApp`, clamp to e.g.
     `0.15..=0.85` while dragging, persist it (new `app_state.split_ratio`
     column + `db::get/set_split_ratio`, same pattern as `theme`/`font_size`).
   - Touches: `app.rs`, `db.rs`.

3. **Monitor-aware window position** (SPEC §4, the part not covered by the
   bug-fix pass above).
   - egui/eframe 0.36 doesn't expose a stable per-monitor device name, so
     this needs `winit`'s `Window::available_monitors()` /
     `Window::current_monitor()` via `eframe::Frame`'s raw window handle (or
     add `winit` as a direct dependency and call the Win32
     `EnumDisplayMonitors`/`GetMonitorInfo` APIs directly, matching the SPEC's
     original suggestion).
   - Persist a monitor identifier string alongside `pos_x`/`pos_y`
     (`window_state.monitor_name`, already in the schema). On restore: if
     that monitor is still connected, use the saved position relative to its
     work area; otherwise fall back to a centered position on the primary
     monitor. Clamp on-screen if the saved position would be mostly
     off-monitor (e.g. after a resolution change).
   - Touches: `main.rs`, `session.rs`.

### P1 — Core PRD items not yet started

1. **Multi-user profiles** (SPEC §5).
   - New `users` table (`id`, `name`, `created_at`, `is_default`); seed a
     default profile on first run in `db::init_db`.
   - Add `user_id` to `window_state`, `panes`, `app_state` (or split
     `app_state` into a per-user table) — all keyed by the active user.
   - Add a user switcher (combo box in the top toolbar, next to Settings).
     Switching: persist the current user's state, load the target user's
     saved session/config, replace `self.panes`/`self.active_pane`/theme/font
     wholesale.
   - Touches: `db.rs`, `session.rs`, `app.rs`; a new `user.rs` is reasonable
     once this grows past a couple of functions.

2. **Two-tier config** (SPEC §6) — depends on (1).
   - Split the existing single-row `app_state` (theme/font) into
     `global_settings(key, value)` and `user_settings(user_id, key, value)`.
   - Resolution: read `user_settings` first, fall back to `global_settings`,
     fall back to the hardcoded default. Wrap in a small `config::get(conn,
     user_id, key)` / `config::set(conn, scope, key, value)` helper so
     Settings-dialog code doesn't special-case every key.
   - Touches: `db.rs` (new tables + migration), a new `config.rs`, `app.rs`
     (Settings window reads/writes through the new helper instead of the
     current `get_theme`/`set_theme`-style one-off functions).

3. **Configurable shortcuts & action buttons** (SPEC §7).
   - Define an `Action` enum (Copy, Cut, Paste, Rename, Delete, NewFolder,
     NewFile, CopyPath, GoBack, GoForward, GoUp, NewTab, CloseTab, SwitchPane,
     OpenWith(PathBuf), ...) — this becomes the single source of truth
     replacing today's scattered `if ctrl && i.key_pressed(...)` checks.
   - `bindings` table: `scope` ('global' or a `user_id`), `key_combo` (stored
     as e.g. `"Ctrl+X"`), `action_id`. Load into an in-memory
     `HashMap<KeyboardShortcut, Action>` per active user (user bindings
     override global for the same action; conflict = same combo bound twice
     in one scope, rejected at bind time with a status message).
   - Rebind UI: a new Settings tab/section listing every `Action` with its
     current shortcut and a "press a new combo" capture field.
   - Toolbar/context-menu buttons become data-driven off the same `Action`
     enum instead of hardcoded `if ui.button("Copy").clicked() {
     self.copy_selection() }` call sites, so a custom action button
     (including "open with `<app>`" launching `std::process::Command`) can be
     added without new match arms elsewhere.
   - Touches: new `actions.rs`, `db.rs`, `app.rs` (toolbar/menu construction
     + global shortcut handling both move to go through the registry).

### P2 — File-operation depth (SPEC §8)

- **Archive extraction**: `.zip` via the `zip` crate, `.tar.gz` via `tar` +
  `flate2`. Add a toolbar/context-menu action enabled only when the
  selection is a single archive file; extract into the current folder by
  default, with a "choose destination" variant using `rfd`.
- **Search/filter**: a text field above the file table filtering the
  in-memory `entries` by substring/glob on `name` (no disk I/O beyond the
  existing listing) for the non-recursive case; a recursive mode reuses the
  P0 background-thread listing infrastructure to walk subdirectories off the
  UI thread.
- **Progress indicator**: `fs_ops::copy_item`/`move_item` currently block the
  UI thread for the whole operation. Move large-payload copy/move/delete
  calls onto a background thread with a progress channel (bytes or file
  count done vs. total), show a determinate progress bar in a modal while
  in flight, and keep the current synchronous path for small
  operations (e.g. under some size/count threshold) to avoid overengineering
  the common case.

### P3 — Advanced features (SPEC §9, §11)

- **Batch rename**: pattern editor (find/replace, sequential numbering, case
  change) operating on the current multi-selection, with a live preview
  table (old name → new name) before committing via `fs_ops::rename_item`
  in a loop.
- **Metadata-only preview pane**: a collapsible side panel showing size,
  created/modified/accessed timestamps, attributes, and full path for the
  current single-item selection — no new dependency, `std::fs::metadata`
  covers all of it.
- **Checksum/compare**: MD5/SHA-256 (via the `sha2`/`md5` crates already
  named in §2) computed on a background thread for one or two selected
  files, with a match/mismatch result shown once both hashes are ready.
- **Multi-window + taskbar badge** (§11): requires each `eframe`/`egui`
  window to own its own native HWND (multiple `eframe::run_native` /
  viewport-per-window rather than the current single-viewport app), then
  `ITaskbarList3::SetOverlayIcon` via the `windows` crate's COM bindings to
  paint a colored dot per window in open-order.
