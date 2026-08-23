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
