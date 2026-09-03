# Changelog

## v0.1.13 (2026-09-03)

- Pressing `*` anywhere activates the filter input — the asterisk is no longer inserted into the filter text, it just gives the filter box focus so you can start typing immediately.
- Clicking a tab in the opposite pane now switches focus to that pane (previously only clicking on a file row switched panes).
- Paste conflict dialog gains an edit-name mode: click "Save as Copy" to rename the incoming file before confirming.
- Bulk Rename: select two or more files, right-click, and choose "Bulk Rename..." — two modes: **Find & Replace** (replace specific characters with another character or blank) and **Edit Names** (scrollable list of input boxes to rename each file individually).
- Fixed a terminal/console window briefly flashing when launching files or apps through FileMan (cmd.exe, openwith.exe, custom actions, app/file launchers now use `CREATE_NO_WINDOW`).
- **File-link tabs**: right-click any file and choose "Pin as Tab" to pin a quick-launch file link alongside your folder tabs. File tabs show a 📄 icon and open the file when clicked. Right-click a file tab and choose "Pin Tab" to lock it and prevent accidental removal. File tabs persist across sessions.

## v0.1.12 (2026-09-01)

- The file listing now auto-filters as you type, Explorer-style — no need to click into the filter box first. Typed characters jump straight into the active tab's filter and give it focus.
- Tabs in the same pane whose folder path or (possibly renamed) label match the current filter text are highlighted with a distinct amber fill, so an already-open match is one click away.
- Fixed Esc not clearing the filter box's text (it only exited focus before).
- Switching to a background tab now marks it dirty so it re-lists immediately, picking up any external changes made while it wasn't active.

## v0.1.11 (2026-08-29)

- Performance: cached drive list at startup (was querying every frame), debounced session persist to 500ms coalescing, moved context-menu path construction inside menu closures, replaced per-frame SQL `is_favourite` lookup with in-memory set, eliminated `to_vec()` deep-clone of cached listings per frame, resolved file icons lazily per drawn row instead of bulk pre-resolving, added `show_rows` virtual scrolling to the List view, applied egui style block only when font size changes, eliminated `to_lowercase()` allocations in sort comparisons, cheaper sidebar tree nodes (pass `&Path` directly as id salt, take/put-back of child vecs), and enabled LTO for the release profile.

## v0.1.10 (2026-08-29)

- New "🕒 Recent" toolbar button: a dropdown of recently opened files and folders (per user, capped at 50), click an entry to jump straight there, or Clear Recent to wipe the list.
- Recent history is stored in SQLite and seeded with the startup folder so it's never blank on first launch.

## v0.1.9 (2026-08-28)

- The "Windows Explorer" right-click submenu now acts on your whole selection instead of only the row you clicked — fixes shell commands like "Combine files in Foxit PDF" only picking up the first file.

## v0.1.8 (2026-08-28)

- Custom Action buttons now send every selected file to the target app (e.g. merging several PDFs in Foxit), not just the first one.
- Drag & drop now accepts attachments dragged straight from mail clients like Outlook, which hand over "virtual files" instead of real paths.
- Creating a single new folder now selects it automatically; press Enter to open the selected file or folder.
- An empty folder shows "There are no files/folder yet here." instead of a stray partial scrollbar.
- Fixed the App/File Launcher search boxes on the toolbar sitting shorter and lower than the buttons beside them.

## v0.1.7 (2026-08-28)

- App Launcher: configure quick-launch apps (Settings > App Launcher), each with an optional toolbar button and a searchable dropdown on the toolbar's second row.
- File Launcher: pin specific files as one-click shortcuts (Settings > File Launcher) that open with Windows' default app for the extension.
- Search dropdowns for both launchers show live filtered results as you type; click an entry to launch it and clear the filter.
- Toolbar buttons now carry prefix icons to tell button types apart at a glance: ⚡ App launcher, 📄 File launch, 🔍 Custom action (Open With), with custom actions in a distinct green/teal style.
- Fixed search-box alignment so it matches button height on the toolbar.

## v0.1.6 (2026-08-27)

- Find Files modal: wider resizable window with roomier Name/Size columns, full-width Modified column, tooltip on truncated search path, Esc to close, auto-focus on the search box, and light-filled Close/Search buttons.
- Settings > Hidden Files: toggle to show/hide hidden files and folders (hidden by default).
- Right-click "Extract Here"/"Extract to..." now only appear for supported archive files (zip, tar, tar.gz/tgz), not every entry.
- Fixed "Open With..." silently launching the default app instead of showing the app chooser.
- Real FileMan icon embedded in the exe/installer/shortcuts and shown on the About page; app/logo artwork cleaned up (transparent background, tightly cropped).

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
