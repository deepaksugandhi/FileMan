# Speed FileMan — Requirements & Specification

Version: 1.0 (draft) — expands `FileMan_PRD.md` into implementable requirements.

## 1. Overview

Speed FileMan is a Windows desktop file manager built as a faster, more configurable
alternative to Windows File Explorer.

- **Platform**: Windows only (v1).
- **Language/Runtime**: Rust (stable toolchain).
- **UI**: [egui](https://github.com/emilk/egui) via `eframe` — immediate-mode, pure Rust,
  no webview and no Node/npm dependency.
- **Storage**: SQLite via `rusqlite` (bundled, no external DB server).
- **Build**: `cargo build` / `cargo run` only. No Visual Studio project files, no MSBuild
  step. (Satisfies PRD item 1.)

## 2. Tech Stack & Build

| Concern | Choice |
|---|---|
| UI framework | `egui` + `eframe` |
| Local DB | `rusqlite` (bundled SQLite) |
| Directory traversal | `std::fs` / `walkdir` |
| Filesystem watching (optional, for live refresh) | `notify` |
| Checksums | `sha2` (+ `md5` if MD5 is required) |
| Archive extraction | `zip` crate for `.zip`; `tar` + `flate2` for `.tar.gz` |
| Safe delete (Recycle Bin) | `trash` crate |
| Native file/folder pickers (if needed) | `rfd` |

Build/run must work with a plain `cargo build --release` / `cargo run` — no IDE or
Visual Studio dependency, per PRD item 1.

## 3. Core Windowing & Panes (PRD item 2)

- Dual-pane layout: left and right panes, resizable split.
- Each pane supports multiple tabs.
- Each tab tracks: current directory path, navigation history (back/forward), sort
  order, view mode.
- One pane/tab is "active" at all times (highlighted); keyboard and mouse actions
  target the active pane/tab.
- Shared folder-tree sidebar (see Section 10) is visible regardless of which pane is
  active; clicking a tree node navigates the currently active pane/tab.

## 4. Session Persistence (PRD item 3)

On app close, persist to the current user's session record:
- Window size and position.
- **Monitor identity** the window was on (see Multi-Monitor Memory below).
- Pane split ratio.
- Per pane: list of open tabs (path + order), active tab index.
- Which pane is active.
- Per tab: sort column/direction, view mode (list/details/icons).

On app launch, restore the last session automatically for the active user profile.
If a saved path no longer exists (deleted/unmounted drive), fall back to that path's
nearest existing ancestor, or the drive root if nothing is reachable.

### Multi-Monitor Window Position Memory

Each window's position must be tied to the specific monitor it was on, not just raw
screen coordinates — coordinates alone break when monitors are added, removed, or
rearranged.

- Store per window: monitor identifier (stable ID, e.g. Windows' device name from
  `GetMonitorInfo`/`EnumDisplayMonitors`, not index), position relative to that
  monitor's work area, and size.
- On restore, match by monitor identifier first. If that monitor is still connected,
  place the window at the saved relative position/size on it.
- If the saved monitor is no longer connected (removed/disconnected), fall back to
  the primary monitor and use a default centered position — do not restore
  off-screen.
- If the saved position would be fully or mostly off the current monitor's work area
  (e.g. resolution changed to something smaller), clamp the window back on-screen
  rather than leaving it inaccessible.
- Applies per window when multiple windows are open (§11) — each window remembers
  its own monitor independently.

### Per-Monitor DPI Scaling

Monitors may run at different DPI/scale factors (e.g. a 100% laptop panel next to a
150% external 4K display). The window must render crisply and at the correct size
on whichever monitor it's on, including when dragged between monitors live.

- `egui`/`eframe` (via `winit`) already reads Windows' per-monitor DPI (v2) and
  exposes a `pixels_per_point` scale factor — no custom DPI-detection code needed,
  just consume it.
- Re-fetch and apply the scale factor whenever the window receives a
  moved/DPI-changed event (`winit`'s `ScaleFactorChanged`), so dragging a window
  across monitors rescales UI immediately rather than only on next launch.
- When restoring a saved window position/size (see Multi-Monitor Window Position
  Memory above), store logical (DPI-independent) size, not physical pixels, so the
  window looks the same relative size regardless of which monitor's scale factor
  applies at restore time.
- No manual bitmap/asset scaling required — `egui` draws vector UI, so this is
  limited to trusting the reported scale factor correctly on window move/restore.

## 5. Multi-User Profiles (PRD item 4)

- Users are profiles created inside the app — unrelated to Windows OS user accounts.
- No authentication/password (like browser profiles) — switching is a simple
  dropdown/menu action.
- A default profile is created automatically on first run.
- Each profile owns its own: session state (Section 4), config overrides
  (Section 6), shortcuts/buttons (Section 7), bookmarks.
- Switching users reloads that profile's saved session and config immediately.

## 6. Configuration System (PRD items 5 & 6)

- **Two-tier config**: global (applies to all users) and per-user (overrides global).
- Precedence: a per-user value overrides the global value for that key; an unset
  per-user value falls back to the global default.
- Config categories include: keyboard shortcuts, custom action buttons, default view
  mode, default sort, theme/appearance.
- **Shared folder tree** (PRD item 5): the folder-tree control and its
  expand/collapse state are shared structure used by every pane and tab — not a
  per-tab tree — though each user profile may have its own bookmarked roots.

### Suggested SQLite schema (tables, not final DDL)

- `users` — id, name, created_at, is_default.
- `sessions` — user_id, window geometry, pane split, active pane.
- `tabs` — session_id, pane, position, path, sort_col, sort_dir, view_mode.
- `global_settings` — key, value.
- `user_settings` — user_id, key, value.
- `shortcuts` — scope (global/user_id), key_combo, action_id.
- `actions` — user-defined action buttons: id, label, action_type, target_app_path
  (nullable), scope.

DB location: `%APPDATA%\FileMan\fileman.db`.

## 7. Configurable Shortcuts & Actions (PRD item 7)

- A fixed **action registry** lists every bindable action, e.g.: copy, copy path,
  copy full path, cut, paste, rename, delete, new tab, close tab, switch pane,
  new folder, open with <app>, refresh, go up, go back/forward.
- Each action can be bound to:
  - A keyboard shortcut (user-configurable, per global or per-user scope).
  - A toolbar/context-menu button (user-configurable, same scope rules).
- The "open with specific app" action stores a target executable path alongside the
  action definition.
- Conflicts (same shortcut bound twice in the same scope) must be surfaced to the
  user when they attempt to bind it.

## 8. File Operations (beyond original PRD — confirmed in scope)

- Copy, move, rename, create folder, create file.
- Delete sends to Recycle Bin (via `trash` crate) rather than permanent delete by
  default; a "permanent delete" variant may exist as a separate bound action.
- Multi-select support for all of the above, with a progress indicator for
  operations expected to take noticeable time (large files/many files).
- Archive extraction: `.zip` and `.tar.gz` extraction to the current folder or a
  chosen destination.
- Search/filter: filter the current folder's listing by name pattern; optional
  recursive search under the current folder.

## 9. Advanced Features (beyond original PRD — confirmed in scope)

- **Batch rename**: apply a pattern (find/replace, sequential numbering, case
  change) across a multi-selection, with a preview of resulting names before
  committing.
- **Preview pane**: metadata only — size, created/modified/accessed timestamps,
  attributes (read-only/hidden/system), full path. No text or image content
  rendering in v1.
- **Checksum/compare**: compute MD5 and/or SHA-256 for a selected file, or compute
  and compare hashes for two selected files, surfacing a match/mismatch result.

## 10. Folder Tree (PRD item 5)

- Single shared tree control listing drives and folder hierarchy.
- Lazy-loaded: child nodes are read on expand, not recursively pre-scanned.
- Tree selection highlight reflects the active pane/tab's current directory;
  navigating a pane/tab updates the highlighted tree node.

## 11. Multi-Window Taskbar Differentiation (new requirement)

The app supports opening multiple top-level windows (e.g., a second Speed FileMan
window alongside the first). Each window's taskbar entry should be visually
distinguishable by color.

- Windows does not expose an API to recolor a taskbar button's background fill —
  that chrome is owned by the shell/theme, not the app.
- Implementation: each window gets a distinct **overlay badge icon** (small colored
  dot) drawn on its taskbar icon, via `ITaskbarList3::SetOverlayIcon` (Win32 COM
  API). This requires each `eframe`/`egui` window to have its own native HWND and
  its own taskbar entry (not merged/grouped).
- Color assignment: assign colors from a fixed palette in window-open order (e.g.
  1st = blue, 2nd = green, 3rd = orange, ...), cycling if more windows are open than
  palette colors. Color assignment is not persisted across restarts — it's purely a
  live differentiator.
- Out of scope: recoloring the taskbar button fill itself, and building a custom
  replacement taskbar — not pursued given the overlay-badge approach meets the
  underlying need (telling windows apart at a glance) with a standard API.

## 12. Non-Functional Requirements

- Directory listings must not block the UI thread — load large directories
  (e.g., 10k+ entries) on a background thread/channel and stream results into the
  UI.
- App must run without administrator elevation for normal file operations within
  the user's own accessible folders.
- Ship as a single portable executable for v1 — no installer required.

## 13. Out of Scope for v1

- Cross-platform support (Linux/macOS) — no OS-abstraction layer required yet.
- Authentication/passwords for user profiles.
- Plugin/extension system.
- Cloud storage integration (OneDrive/Google Drive/etc. beyond what Explorer
  already mounts as local paths).
- Inline text/image content preview (metadata-only preview per Section 9).

## 14. Open Questions / Future Considerations

Deferred rather than silently dropped — revisit before/if they become blocking:

- Network drive (UNC path) navigation behavior and performance.
- Symbolic link / junction handling (follow vs. show as link).
- Long path support (`\\?\` prefix) for paths beyond MAX_PATH.
- Whether "permanent delete" (bypassing Recycle Bin) should be a first-class bound
  action or an advanced/hidden option.
- Whether cross-platform support becomes a v2 goal, which would require
  revisiting the drive-tree and path-handling design now rather than later.

## Traceability to original PRD

| PRD item | Spec section |
|---|---|
| 1. Rust, no Visual Studio build | §1, §2 |
| 2. Dual panes, multiple tabs | §3 |
| 3. Save/restore session | §4 |
| 4. Per-user configuration, user switch | §5 |
| 5. Common folder tree per pane/tab | §3, §6, §10 |
| 6. Global vs. per-user configs | §6 |
| 7. Configurable shortcuts & buttons | §7 |

## 15. Next Implementation Plan (Phase 2)

Snapshot as of 2026-08-23. Implemented so far: dual-pane/tabs, session persistence
(window size/position, tabs, sort, column widths, per-tab view mode), copy/cut/
paste/delete/rename/new, sidebar folder tree, dark/light/system theming, a
Settings window (theme + font size/family), a tab context menu, a persistent
right-click file context menu, a duplicate-name-on-paste prompt, and List/
Details/Icons view modes.

Grouped by priority — earlier tiers unblock or de-risk later ones.

### Bug-fix pass — done (2026-08-23)

The six bugs/regressions found reviewing the previous change are resolved,
except the one flagged as deliberate:

1. ✅ **Font-family picker** — `apply_fonts` (app.rs) now reads the matching
   `.ttf` from `%WINDIR%\Fonts` (Segoe UI, Arial, Times New Roman, Courier
   New) at runtime and swaps it into the egui font atlas; falls back to the
   embedded Inter with a status message if the file isn't found. Re-applied
   reactively (once per family change), not hardcoded at startup in
   `main.rs` anymore.
2. ✅ **Right-click file context menu vanishing** — replaced the hand-rolled
   `egui::Area` (driven by a one-frame-only flag) with egui's built-in
   `Response::context_menu`, which owns its own open/close state across
   frames. Right-clicking an unselected entry now also selects it first, so
   the menu acts on the right item.
   *(Not in scope for this pass: full per-item action menu on the sidebar folder tree.)*
3. ⛔ **Skipped, deliberate.** The orange-fill / black-text active-tab
   highlight is an intentional visual choice, not a bug — left as-is.
4. ✅ **Per-tab view mode reinstated** — `ViewMode::{Details,List,Icons}` is
   back on `Tab`, with a real toggle row per pane and three genuinely
   different renderers (table / plain list / wrapped icon grid), all sharing
   the same selection, navigation, and context-menu plumbing. Persisted via
   a new `panes.view_mode` column.
5. ✅ **Ctrl+scroll zoom** — now calls egui's native `Context::set_zoom_factor`
   via `InputState::zoom_delta()` instead of hand-editing `font_size`. This
   scales the *whole* UI (spacing, row heights, icons) together, so nothing
   clips, and egui already excludes ctrl-held wheel events from
   `smooth_scroll_delta`, so there's no more double-scroll. Table row/header
   height are now also derived from the font-size setting
   (`(font_size * 1.3).max(18.0)`), so the Settings dialog's font-size
   slider can't cause clipping either.
6. ✅ **Window position persistence** — `persist()` now captures
   `ViewportInfo::outer_rect` each frame it changes and writes real
   `pos_x`/`pos_y`; `main.rs` applies them via `ViewportBuilder::with_position`
   on restore. `monitor_name` and monitor-aware clamping/fallback are **not**
   included — egui 0.36's `ViewportInfo` has no stable monitor identifier, so
   that piece stays as its own P0 task below (needs raw `winit`/Win32 access).

### Bug-fix pass 2 — done (2026-08-23)

Found reviewing the P2 file-operation-depth additions (`archive.rs`,
`search.rs`, `progress.rs`):

1. ✅ **`extract_here()` had a leftover, buggy branch** that spawned a
   background copy of the archive onto itself (same source and destination
   folder) and immediately discarded the handle before falling back to a
   synchronous `extract_archive` call anyway. Removed; extraction is
   synchronous (archives are typically small; revisit if that stops holding).
2. ✅ **Background copy/move/delete are now actually wired up.**
   `paste_clipboard`/`delete_selection` used to call the old synchronous
   `fs_ops::copy_item`/`move_item`/`delete_to_trash` directly, so the
   progress modal never appeared for a real user action. They now go through
   new `progress::copy_items_bg`/`move_items_bg`/`delete_to_trash_bg`
   (batch versions, one background op for the whole paste/delete). The
   one-collision-at-a-time `DuplicateName` dialog UX is preserved by a cheap
   `Path::exists` pre-check before handing the batch to the background
   thread — no recursive walk needed for that part.
3. ⛔ **Skipped, deliberate** (from pass 1) — orange/black active-tab
   highlight, unchanged.
4. ✅ **Per-pane search filter is now per-tab.** Was a single
   `FileManApp::search_query` field shared by both panes' text fields — typing
   in one pane's filter overwrote the other's. Moved to `Tab::filter`,
   alongside `sort_col`/`view_mode`; cleared on navigation like the
   selection is.
5. ✅ **Background op file-counting moved off the caller thread.** The old
   `copy_item_bg`/`move_item_bg` called `count_item` (a recursive directory
   walk) synchronously before spawning — for a large tree this just moved
   the UI freeze earlier instead of removing it. The new batch functions
   count inside the spawned thread and start by reporting a "Counting…"
   state.
6. ✅ **`is_archive` no longer accepts a bare `.gz`.** It matched any `.gz`
   extension, but `extract_archive` only handles `.tar.gz`/`.tgz` — a plain
   single-file gzip would show as extractable in the UI and then fail with a
   decode error. Now only `.zip`, `.tar`, `.tar.gz`, `.tgz` are recognized.
   (The "keep all folders visible while filtering" behavior in
   `filter_entries` was flagged as a design question, not a bug — left
   unchanged pending confirmation.)

Also fixed while in `progress.rs`: `copy_item_recursive`'s per-file copy
didn't check for an existing destination file the way the original
`fs_ops::copy_file` did, so a nested name collision partway through a
background directory copy would silently overwrite instead of erroring —
added the same `AlreadyExists` check back.

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

### P2 — File-operation depth (SPEC §8) — done, with gaps noted

- ✅ **Archive extraction**: `.zip` (`zip` crate) and `.tar`/`.tar.gz`/`.tgz`
  (`tar` + `flate2`) via `archive.rs`. Toolbar and context-menu actions
  ("Extract Here" / "Extract to…", the latter via `rfd`), enabled only when
  the selection is a single supported archive. Runs synchronously — fine for
  typical archive sizes; revisit (background + progress, same pattern as
  copy/move) if that stops holding.
- ✅ **Search/filter**: `search::filter_entries` does an in-memory,
  case-insensitive substring match on `name`, per-tab (`Tab::filter`).
  Directories are always kept regardless of the filter so navigation still
  works — confirm this is the desired behavior; Explorer-style filtering
  usually hides non-matching folders too.
  - **Not yet wired up**: `search::recursive_search`/`walk_recursive` exist
    (background-thread recursive search infrastructure) but have no caller
    or UI toggle yet — still open.
- ✅ **Progress indicator**: `progress::copy_items_bg`/`move_items_bg`/
  `delete_to_trash_bg` run copy/cut-paste/delete on a background thread with
  a live progress modal (`egui::ProgressBar`), file-counting included on the
  background thread so it doesn't block the UI either. Wired into
  `paste_clipboard`/`delete_selection`.
  - **Not yet covered**: `rename_item`/`create_folder`/`create_file` and
    archive extraction still run synchronously — fine at their typical
    scale (single item / one archive), but worth background-izing later if
    that changes.

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
