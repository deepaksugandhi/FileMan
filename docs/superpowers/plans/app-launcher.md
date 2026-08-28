# Application Launcher — Implementation Plan

## Overview

Add an Application Launcher to the 2nd toolbar row: a search/filter text input
for users to type and find apps, plus configurable direct-launch buttons for
pinned apps. Configuration lives in a new "App Launcher" settings page.

## Architecture

### Data Model

**New table: `launcher_apps`** (persisted in SQLite alongside existing tables)

```sql
CREATE TABLE IF NOT EXISTS launcher_apps (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    label TEXT NOT NULL,          -- Display name ("VS Code")
    exe_path TEXT NOT NULL,       -- Full path to executable
    args TEXT DEFAULT '',         -- Optional launch arguments
    scope TEXT NOT NULL           -- 'global' or 'user:N'
);
```

**New struct in `actions.rs`:**

```rust
pub struct LauncherApp {
    pub id: i64,
    pub label: String,
    pub exe_path: String,
    pub args: String,
}
```

**New fields on `FileManApp` (app.rs):**

```rust
/// Configured launcher apps (from DB).
launcher_apps: Vec<LauncherApp>,
/// Text in the launcher search/filter input.
launcher_filter: String,
/// Icons loaded for launcher apps, keyed by exe path.
launcher_icons: HashMap<String, Option<egui::TextureHandle>>,
/// Draft label for the settings add-new-app form.
new_launcher_label: String,
/// Draft exe path for the settings add-new-app form.
new_launcher_exe: Option<PathBuf>,
/// Draft args for the settings add-new-app form.
new_launcher_args: String,
```

### DB Functions (actions.rs)

| Function | Description |
|---|---|
| `list_launcher_apps(conn, user_id) -> Vec<LauncherApp>` | List global + user-scoped apps |
| `add_launcher_app(conn, user_id, label, exe_path, args) -> Result<()>` | Insert new app |
| `remove_launcher_app(conn, id) -> Result<()>` | Delete by id |
| `update_launcher_app(conn, id, label, exe_path, args) -> Result<()>` | Edit existing app |

All functions added to `actions.rs::init_tables` for the new table.

### UI Changes

#### 2nd Toolbar Row (app.rs ~line 4765)

Current state: Shows custom actions (exe "open with" buttons) in a horizontal row.

New layout:

```
[ 🔍 Search apps... ]  [ VS Code ]  [ Notepad++ ]  [ Chrome ]  ... (filtered)
```

- **Search input** (`TextEdit::singleline`) with hint text "Search apps..."
  - Lives on the LEFT side of the row
  - `launcher_filter` string, updated on each keystroke
  - When non-empty, filters the direct-launch buttons to show only matching
    apps (case-insensitive substring match on label)
- **Direct-launch buttons** — shown to the right of the search input
  - Each button shows the app's icon (loaded via `icon_cache`) + label
  - Clicked → launches `exe_path` with optional `args`
  - Unfiltered when search is empty; filtered when search has text
- **Custom actions** (existing "open with" buttons) remain visible after
  the launcher buttons (or interleaved — TBD, but keeping them separate
  is cleaner)

#### Launch Behavior

When a launcher button is clicked:
1. Use `std::process::Command::new(&app.exe_path)` to spawn the process
2. If `args` is non-empty, split and pass as arguments
3. No file argument is passed (unlike custom actions which pass the selected file)
4. Show status toast: "Launched {label}"

### Settings Page

**New `SettingsPage::AppLauncher` variant** added to the enum.

**Settings page UI** (`settings_page_app_launcher` method):

1. Description text: "Configure applications that appear as quick-launch buttons on the toolbar. Use the search box on the toolbar to filter and launch any configured app."

2. **List of configured apps** — same pattern as Custom Actions page:
   - Row: icon + label + exe_path + args + Remove button
   - Edit capability: clicking label/exe makes them editable inline (optional — can start with remove-only)

3. **Add-app form** (grouped in `Frame::group`):
   - Name field (TextEdit)
   - Program path (Browse button + rfd::FileDialog + display)
   - Arguments field (TextEdit, optional)
   - Add button

**Nav rail icon**: A rocket/launch icon (simple vector shape).

## Files to Modify

| File | Changes |
|---|---|
| `src/actions.rs` | Add `LauncherApp` struct, DB functions, table creation in `init_tables` |
| `src/app.rs` | Add fields to `FileManApp`, add `SettingsPage::AppLauncher`, render 2nd row with search + buttons, settings page, launch logic, load/save state |

## Implementation Steps

1. **DB layer** (`actions.rs`): Add `LauncherApp` struct, `init_tables` extension, CRUD functions
2. **App state** (`app.rs`): Add new fields to `FileManApp` struct and initialize in `new()` / `switch_user()`
3. **2nd toolbar row** (`app.rs`): Modify the `!self.custom_actions.is_empty()` block (line 4765) to render search input + filtered launcher buttons + custom action buttons
4. **Settings enum** (`app.rs`): Add `AppLauncher` to `SettingsPage` enum
5. **Settings nav** (`app.rs`): Add nav entry and icon in `show_settings_window` and `paint_nav_icon`
6. **Settings page** (`app.rs`): Implement `settings_page_app_launcher()` method
7. **Settings dispatch** (`app.rs`): Wire up the new page in the match arm at line 2828
8. **Launch logic** (`app.rs`): Handle button clicks → `std::process::Command`
9. **Verify**: `cargo check` and `cargo test`

## Decisions (confirmed)

- **Standalone launch only** — no file argument passed (unlike custom actions)
- **Label-only search** — filter matches display name, not exe path
- **Keyboard shortcut** for search focus — out of scope for v1
