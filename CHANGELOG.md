# Changelog

## v0.1.5 (2026-08-26)

- Right-click context menu gains "Properties", opening Windows' native file/folder properties dialog.
- New Settings > File Types page: pin a file extension to always open with a specific program, bypassing whatever Windows currently has set as the default (e.g. always open `.xlsm` with Excel).
- Window/taskbar title now shows the active folder name before "FileMan" (e.g. "Reports - FileMan"), updated live as you navigate.
- Fixed a build-blocking bug in the breadcrumb address bar and tab-rename dialog.

## v0.1.4 (2026-08-26)

- Universal tab sorting: sort column/direction now applies consistently across new and existing tabs.
- Fixed address-bar Enter-to-navigate, network-drive delete, and the renamed-tab marker.
- Replaced window-exit polling with native `IDropTarget` for dragging files out of FileMan, fixing drag-out reliability (including under RDP).

## v0.1.3 (2026-08-25)

- Clickable breadcrumb address bar (click a segment to navigate, click past the end to copy the full path).
- Performance: caching for sorting, filtering, and icon loading.
- Fixed progress-modal sizing/item count, external drag & drop, tab renaming, and multi-folder creation.

## v0.1.2 (2026-08-25)

- Shell file-type icons in file listings.
- Version label shown in the settings nav rail.

## v0.1.1 (2026-08-25)

- Help dialog and user manual.
- Delete confirmation dialog with a permanent-delete fallback when Recycle Bin isn't available.
- View-mode settings (Details/List/Icons).
- Fixed Ctrl+C/X/V shortcuts, centered modal dialogs, and default focus on dialog OK/input fields.
- CI: installer is now built and attached automatically on GitHub release.

## v0.1.0 (2026-08-24)

Initial release — dual-pane Windows file manager built on egui:

- Dual-pane browsing with independent tabs, navigation history, and session autosave/restore.
- Sortable, resizable columns (name/modified/size/archive) persisted per tab.
- Sidebar folder tree synced to the active pane, with collapse/expand and highlight of the active node.
- File operations: copy/cut/paste/move/delete/rename/new, with multi-select.
- Context menus, favourites, "Open With", per-pane address bar, and a Find dialog.
- Win11-style UI pass: vertical tabs with a resizable sidebar, move-tab-between-panes, option to register as the default folder explorer.
- Per-instance taskbar icon coloring, SQLite-backed settings with migrations, and an Inno Setup Windows installer.
