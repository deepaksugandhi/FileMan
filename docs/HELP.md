# FileMan User Manual

A dual-pane file manager for Windows, built with Rust + egui.

---

## Getting Started

### Interface Layout

FileMan uses a three-part layout:

- **Folder Tree** (left panel) — hierarchical view of your drives and folders, with a Favourites section at the top.
- **Dual Panes** (center) — two independent file browsers, each with its own tab strip, address bar, filter, and navigation controls.
- **Toolbar** (top) — command buttons for common operations, plus a user switcher and Settings access.

### Switching Users

Click the user dropdown (top-right) to switch between profiles. Each user has their own settings, favourites, toolbar layout, and shortcuts. Click "New User..." to create a new profile.

---

## Navigation

### Address Bar

Type a path directly into the address bar and press **Enter** to navigate. The bar shows the current folder path and updates as you browse.

### Navigation Buttons

- **Back** (Alt+Left) — return to the previous folder.
- **Forward** (Alt+Right) — go forward if you went back.
- **Up** (Backspace) — go to the parent folder.

### Folder Tree

The left panel shows your drives and folders. Click any folder to navigate there. The tree auto-expands to follow your current path. Favourites appear at the top for quick access.

### Tabs

Each pane supports multiple tabs. Open a new tab with the **+ Tab** button or close one with the **x** on hover. Right-click a tab for more options (duplicate, close, pin). Pinned tabs resist accidental navigation.

---

## Viewing Files

### View Modes

Switch between three layouts via **Settings > View**:

- **Details** — columns for name, date modified, type, and size. Click column headers to sort.
- **List** — compact single-column list with name, date, and size.
- **Icons** — grid of large icons with filenames, suited for image-heavy folders.

Each file shows the same associated app icon Windows Explorer uses for its file type (e.g. `.txt` files show Notepad's icon). Folders keep the folder glyph.

### Filtering

Type in the filter box (next to the Up button) to narrow the visible files. The filter is case-insensitive and matches against filenames. Click the red **x** to clear.

### Sorting

In Details view, click any column header to sort by that column. Click again to reverse the sort order. A small arrow indicates the active sort column.

---

## File Operations

### Toolbar Commands

| Button | Shortcut | Description |
|--------|----------|-------------|
| Copy | Ctrl+C | Copy selected files to clipboard |
| Cut | Ctrl+X | Cut selected files to clipboard |
| Paste | Ctrl+V | Paste files from clipboard |
| Delete | Del | Send selected files to the Recycle Bin |
| Rename | F2 | Rename the selected file or folder |
| New Folder | — | Create a new folder in the current directory |
| New File | — | Create a new empty file |
| Copy Filename | — | Copy the full path of the selected file |
| Copy Folder Path | — | Copy the current folder path |
| Find | Ctrl+F | Open the Find dialog to search files |
| Refresh | F5 | Reload the current folder listing |

### Context Menu

Right-click any file or folder to access:

- Copy, Cut, Paste
- Rename, Delete
- New Folder, New File
- Extract Here / Extract to... (for archives)
- Copy Filename, Copy Folder Path
- Open With... (choose an application)
- Open in Windows Explorer (folders only)
- Add to Favourites (folders only)

### Drag and Drop

Drag files between panes to copy or move them. Hold **Ctrl** while dropping to force a copy.

---

## Favourites

Right-click any folder and select **Add to Favourites** to pin it to the Folder Tree. Favourites appear under a dedicated section at the top of the tree for quick access. Right-click a favourite to remove it.

---

## Custom Actions

Custom actions let you open files with any application. Each action launches a chosen program with the selected file as its argument.

### Adding a Custom Action

1. Open **Settings > Custom Actions**.
2. Enter a name for the action.
3. Click **Browse** to select the program executable.
4. Click **Add**.

Custom action buttons appear on the second toolbar row, each showing the program's icon.

---

## Settings

Open Settings via the **gear icon** in the toolbar.

### Appearance

- **Theme** — switch between Light and Dark mode.
- **Font Family** — choose from Inter, Segoe UI, Arial, Helvetica, Times New Roman, or Courier New.
- **Font Size** — adjust text size from 8px to 24px.
- **Tab Layout** — set tabs to stack horizontally or vertically.

### Keyboard Shortcuts

All built-in actions have rebindable keyboard shortcuts. Click **Rebind** next to any action, then press the new key combination. Press **Escape** to cancel.

Default shortcuts:

| Action | Default |
|--------|---------|
| Copy | Ctrl+C |
| Cut | Ctrl+X |
| Paste | Ctrl+V |
| Find | Ctrl+F |
| Refresh | F5 |
| Go Up | Backspace |
| Rename | F2 |
| Copy Filename | F3 |
| Copy Folder Path | F4 |

### Toolbar

Customize which buttons appear on the main toolbar row. Use the arrow buttons to reorder, or uncheck items to hide them. Available buttons that are not on the toolbar can be added from the list below.

### View

Choose the default listing layout (Details, List, or Icons). Changes apply to the active tab immediately.

### Advanced

- **Default Folder Explorer** — make FileMan the default folder handler on Windows, or restore the Windows default.
- **Export/Import Settings** — save all your settings (theme, fonts, shortcuts, toolbar, custom actions) to a JSON file, or load settings from another FileMan installation.

---

## Multi-User Support

FileMan supports multiple user profiles on the same PC. Each user has independent:

- Theme, font, and tab layout preferences
- Keyboard shortcut bindings
- Toolbar layout and custom actions
- Favourites

Switch users via the dropdown in the top-right corner. Create new profiles with "New User...".

---

## Keyboard Reference

| Key | Action |
|-----|--------|
| Ctrl+C | Copy |
| Ctrl+X | Cut |
| Ctrl+V | Paste |
| Ctrl+F | Find |
| F2 | Rename |
| F3 | Copy Filename |
| F4 | Copy Folder Path |
| F5 | Refresh |
| Backspace | Go Up |
| Alt+Left | Go Back |
| Alt+Right | Go Forward |
| Enter | Confirm / Open |
| Escape | Cancel / Close dialog |
| Delete | Delete selected |

---

## Tips

- **Pinned tabs** won't navigate away when you double-click a folder — unpin first.
- **Filter** is per-tab, so each pane/tab filters independently.
- The **pane divider** can be dragged to resize left/right panes.
- Press **Esc** to close any dialog including the Help window.
- Settings are saved per user and persist across sessions.
- Use **Export/Import** in Advanced settings to transfer your setup to another machine.
