# FileMan Performance Plan

Findings from a read-through of the render path (`src/app.rs` `impl eframe::App::ui`)
and its helpers, plus a mechanical implementation plan.

Status: **not started**. Baseline version: v0.1.10 (commit `af3775f`).

---

## Rules for the implementing agent

1. **Work one task at a time, in the order given.** Each task is independent —
   finish, test, and commit it before starting the next.
2. **Line numbers in this document are from v0.1.10 and will drift** as edits
   land. Always re-locate the code by grepping for the quoted **anchor snippet**,
   never by jumping to a line number.
3. **After every task run:**
   ```
   cargo test
   cargo clippy --all-targets -- -D warnings
   ```
   All 118 existing tests must still pass. Do not commit with failures.
4. **Do not refactor beyond the task.** No renames, no reformatting of untouched
   code, no new dependencies, no new abstractions. Shortest diff that works.
5. **Behaviour must not change.** These are pure performance fixes. If a task
   looks like it would change what the user sees, stop and report instead of
   guessing.
6. Keep the existing comment style: a short `//` note explaining *why* where the
   reason is non-obvious. Where a task deliberately accepts a ceiling, mark it
   with a `// ponytail:` comment naming the ceiling, matching the convention
   already used in `src/app.rs:845` and `src/tree.rs:3`.
7. Commit message per task: `Perf: <task title>`. Do not bump the version until
   the last task; then bump to `0.1.11` and add a CHANGELOG entry.

---

## Summary of findings

| # | Finding | File | Severity | Symptom the user feels |
|---|---------|------|----------|------------------------|
| 1 | `list_drives()` called every frame (26 `exists()` syscalls) | `app.rs:5111` | **Critical** | Whole UI stalls, worst with an offline mapped network drive |
| 2 | Full SQLite session write on every dirty frame | `app.rs:6640`, `session.rs:60` | **Critical** | Lag while dragging/resizing the window or the tree divider |
| 3 | Directory listing deep-cloned every frame | `app.rs:3984` | **High** | Big folders feel heavy on every repaint |
| 4 | Icon slots rebuilt for *all* entries every frame | `app.rs:3995`, `icon_cache.rs:49` | **High** | Big folders; plus a hitch on first opening a folder |
| 5 | List and Icons views render every entry (no virtualization) | `app.rs:4269`, `app.rs:4337` | **High** | Those two view modes crawl in big folders |
| 6 | `context_menu_paths` built per visible row per frame | `app.rs:4215`, `4312`, `4392` | **High** | App crawls after Ctrl+A in a large folder |
| 7 | `is_favourite()` SQL query every frame | `app.rs:5163` | Medium | Constant small overhead |
| 8 | Whole `Style` rebuilt 4× every frame | `app.rs:4709`–`4826` | Medium | Constant small overhead |
| 9 | `sort_entries` allocates 2 Strings per comparison | `fs_entry.rs:64`, `:71` | Medium | Sort/navigation pause in big folders |
| 10 | Tree node clones its child Vec + builds an id String per node per frame | `app.rs:1218`, `1228` | Low | Deep/wide expanded trees |
| 11 | No release profile tuning | `Cargo.toml` | Low | A few percent everywhere |

Findings 1, 2, 6, 7 are small, self-contained diffs that remove the most visible
jank — do those first (P0). Findings 3, 4, 5 are the ones that matter for large
directories and need a little restructuring (P1). The rest are cleanup (P2).

---

# P0 — small diffs, biggest visible win

## Task 1 — Cache the drive list instead of scanning A–Z every frame

**Problem.** `src/tree.rs:6` `list_drives()` does 26 `Path::exists()` calls
(one per drive letter). It is called from inside the render loop, so that is 26
filesystem syscalls *per frame*. A mapped network drive that is offline makes
each `exists()` block for hundreds of milliseconds, which freezes the UI.

**Anchor** (`src/app.rs`, inside the folder-tree panel):
```rust
                        for drive in tree::list_drives() {
                            self.show_dir_node(ui, &drive, None, &active_path, force_expand);
                        }
```

**Change.**

1. Add a field to `struct FileManApp` (next to the existing `system_folders`
   field, currently `src/app.rs:232`):
   ```rust
       /// Drive roots for the sidebar. `list_drives` stats all 26 letters, and
       /// an offline mapped drive can block for hundreds of ms — so resolve it
       /// once at startup rather than every frame.
       // ponytail: never refreshed, so a drive plugged in mid-session needs a
       // restart to appear. Add a WM_DEVICECHANGE hook if that becomes annoying.
       drives: Vec<PathBuf>,
   ```
2. Initialise it in `FileManApp::new` next to `system_folders: tree::list_system_folders(),`
   (currently `src/app.rs:732`):
   ```rust
               drives: tree::list_drives(),
   ```
3. Replace the anchor with:
   ```rust
                        for drive in self.drives.clone() {
                            self.show_dir_node(ui, &drive, None, &active_path, force_expand);
                        }
   ```
   (The `.clone()` mirrors the existing `self.system_folders.clone()` /
   `self.favourites.clone()` pattern a few lines above — it is needed because
   `show_dir_node` takes `&mut self`. Cloning a handful of `PathBuf`s per frame
   is trivial next to 26 syscalls.)

**Verify.** Launch the app; the sidebar still lists every drive. `cargo test`.

---

## Task 2 — Debounce the session persist

**Problem.** `src/app.rs:6640`:
```rust
        if self.dirty {
            self.persist();
            self.dirty = false;
        }
```
`persist()` → `session::save_session` (`src/session.rs:60`) opens a transaction,
upserts `window_state`, runs `DELETE FROM panes`, re-inserts every tab, upserts
`app_state`, and commits (an fsync). `self.dirty = true` is set from window move
(`app.rs:4862`), window resize (`app.rs:4855`), tree-divider drag (`app.rs:5153`)
and column resize (`app.rs:4248`) — all of which fire **every frame** during a
drag. So dragging the window does a full transaction plus a disk flush per frame.

**Change.** Coalesce writes to at most one every 500 ms, and always flush the
last pending write when the app closes.

1. Add a field to `struct FileManApp` next to `dirty: bool` (`src/app.rs:176`):
   ```rust
       /// When `dirty` was last flushed to SQLite. Window/divider drags set
       /// `dirty` every frame, and each `persist()` is a full transaction with
       /// an fsync — so coalesce them instead of writing per frame.
       last_persist: std::time::Instant,
   ```
2. Initialise it in `FileManApp::new` next to `dirty: false,` (`src/app.rs:706`):
   ```rust
               last_persist: std::time::Instant::now(),
   ```
3. Replace the anchor block with:
   ```rust
        if self.dirty
            && self.last_persist.elapsed() >= std::time::Duration::from_millis(500)
        {
            self.persist();
            self.last_persist = std::time::Instant::now();
            self.dirty = false;
        }
   ```
4. **Do not drop the pending write on exit.** Add a `save` override to the
   `impl eframe::App for FileManApp` block (same block that contains `fn ui`),
   so a still-dirty state is flushed when the window closes:
   ```rust
       fn save(&mut self, _storage: &mut dyn eframe::Storage) {
           if self.dirty {
               self.persist();
               self.dirty = false;
           }
       }
   ```
   Check the `eframe` 0.36 `App` trait signature before writing this — if
   `save` is not available with that signature, use `on_exit` instead, and if
   neither exists, report back rather than inventing one.

**Verify.** Drag the window around, then drag the tree divider — both should feel
smooth. Close and reopen the app: window position, tree width and column widths
must all still be restored. `cargo test`.

---

## Task 3 — Build context-menu paths only when the menu opens

**Problem.** In all three view modes the selection paths are materialised for
every drawn row on every frame, *before* the menu is ever opened:
```rust
                                                let selection_paths = context_menu_paths(
                                                    pane.active_tab(),
                                                    entry,
                                                    is_selected,
                                                );
                                                styled_context_menu(&row_resp, |ui| {
```
`context_menu_paths` (`src/app.rs:7386`) allocates a `PathBuf` per selected item.
After Ctrl+A in a 10 000-file folder that is 10 000 allocations × ~40 visible
rows, every frame.

**Change.** Move the call inside the `styled_context_menu` closure — that closure
only runs while the menu is actually open. Do this at all **three** call sites:
Details (`app.rs:4215`), List (`app.rs:4312`), Icons (`app.rs:4392`). Pattern:

```rust
                                                styled_context_menu(&row_resp, |ui| {
                                                    let selection_paths = context_menu_paths(
                                                        pane.active_tab(),
                                                        entry,
                                                        is_selected,
                                                    );
                                                    show_entry_context_menu(
                                                        ui,
                                                        &mut row_action,
                                                        &entry.path,
                                                        entry.is_dir,
                                                        &selection_paths,
                                                        &self.shell_menu_hidden,
                                                        &mut self.shell_menu_cache,
                                                    );
                                                });
```

If the borrow checker objects (the closure now borrows `pane` as well as
`self`), the minimal fix is to hoist the needed pieces before the closure
*without* allocating the path list — e.g. capture `let tab_path = pane.active_tab().path.clone();`
and `let selected: &HashSet<String> = ...` — and build the `Vec<PathBuf>` inside.
Keep `context_menu_paths` itself unchanged, and keep its behaviour identical
(multi-selection paths when the row is part of a >1 selection, otherwise just
this entry's path).

**Verify.** Ctrl+A in a large folder, then right-click a row — the menu must
still act on the whole selection. Right-click an unselected row — must act on
that row only. Scrolling after Ctrl+A should now be smooth. `cargo test`.

---

## Task 4 — Answer "is favourite?" from memory, not SQL

**Problem.** `src/app.rs:5163`, inside the toolbar, runs a SQL query per frame:
```rust
                let is_fav = crate::db::is_favourite(&self.conn, self.current_user_id, &current_path.display().to_string());
```

`self.favourites: Vec<String>` already holds exactly this data and is kept in
sync by `add_favourite` / `remove_favourite` / `reload_for_user`
(`src/app.rs:929`, `:939`, `:1015`).

**Change.**
```rust
                let current_path_str = current_path.display().to_string();
                let is_fav = self.favourites.iter().any(|f| f == &current_path_str);
```
Reuse `current_path_str` if the surrounding code re-derives the same string.
Leave `crate::db::is_favourite` in place — it is still used elsewhere and by
tests; do not delete it.

**Verify.** Toggle the favourite button — the icon/label must flip immediately,
and the folder must appear/disappear in the sidebar Favourites section.
`cargo test`.

---

# P1 — large-directory performance

## Task 5 — Stop deep-cloning the listing every frame

**Problem.** `src/app.rs:3981`:
```rust
        let listing_result: Result<Vec<crate::fs_entry::FsEntry>, String> =
            match &pane.active_tab().listing_error {
                Some(err) => Err(err.clone()),
                None => Ok(pane
                    .active_tab_mut()
                    .display_entries(&query, &sort_col, sort_asc)
                    .to_vec()),
            };
```
`Tab::display_cache` (`src/tab.rs:61`) exists specifically so the filter+sort is
not redone every frame — and then `.to_vec()` deep-clones the result anyway.
Each `FsEntry` clone is two heap allocations (`String` name + `PathBuf`), so a
10 000-entry folder costs ~20 000 allocations per pane per repaint.

**Change.** Render from a borrow instead of a clone. The obstacle is that the
render body needs `&mut self` (for `self.file_icons`, `self.shell_menu_cache`, …)
while `entries` borrows out of `self.panes`. The **lowest-risk** way to break
that is a take/put-back around the render block:

```rust
        // Render from the cached listing without deep-cloning it every frame:
        // move the cached vec out, render against a borrow, put it back.
        let mut entries: Vec<crate::fs_entry::FsEntry> = Vec::new();
        let listing_err = pane.active_tab().listing_error.clone();
        if listing_err.is_none() {
            pane.active_tab_mut()
                .display_entries(&query, &sort_col, sort_asc);
            if let Some((_, cached)) = &mut pane.active_tab_mut().display_cache {
                entries = std::mem::take(cached);
            }
        }
```
…render using `&entries`… then, at **every** exit path of the render block, put
it back:
```rust
        if let Some((_, cached)) = &mut pane.active_tab_mut().display_cache {
            *cached = entries;
        }
```

**Critical correctness notes for this task:**
- The put-back must run on **every** path out of the block, including the early
  `loading_empty` / empty-listing branches and the error branch. Prefer
  restructuring so there is exactly one put-back at the end of the block over
  duplicating it. If the code has genuine early `return`s, restructure them into
  a single exit instead of adding a `Drop` guard.
- If the put-back is missed, the tab's cache is left empty while its cache *key*
  still looks valid, so the pane silently renders as an empty folder. **Test this
  explicitly**: navigate into a folder, scroll, click a row, resize a column,
  switch tabs and come back — the listing must never blank out.
- If this take/put-back proves fragile in practice, the acceptable fallback is
  to wrap the cached vec in an `Rc<Vec<FsEntry>>` inside `Tab::display_cache` and
  clone the `Rc` (one refcount bump) instead of the vec. Do **not** fall back to
  `.to_vec()`.

**Verify.** Open a folder with several thousand files; scrolling and hovering
should be visibly lighter. Every interaction listed above must keep the listing
visible. `cargo test`.

---

## Task 6 — Resolve file icons lazily, per drawn row

**Problem.** `src/app.rs:3995`:
```rust
                let entry_icons =
                    crate::icon_cache::ensure_entry_icons(&mut self.file_icons, ctx, &entries);
```
`ensure_entry_icons` (`src/icon_cache.rs:49`) walks **all** entries and per entry
does a `format!` + `to_lowercase` (2 allocations), a `HashMap<String>` lookup and
a `TextureHandle` clone — for the whole directory, every frame, even though the
Details view only draws ~40 rows. On first entry into a folder it also calls
`SHGetFileInfoW` synchronously for each new extension, which is the hitch felt
when opening a folder.

**Change.**

1. Add a method on `FileManApp` (near the other small helpers):
   ```rust
       /// The shell-associated icon for `path`, extracted and cached on first
       /// use. Called per *drawn* row rather than per directory entry, so a
       /// 10k-file folder only pays for the ~40 rows actually on screen.
       fn entry_icon(
           &mut self,
           ctx: &egui::Context,
           path: &Path,
       ) -> Option<egui::TextureHandle> {
           let key = crate::icon_cache::file_icon_cache_key(path);
           if !self.file_icons.contains_key(&key) {
               let tex = crate::icon_cache::load_file_icon_texture(ctx, path);
               self.file_icons.insert(key.clone(), tex);
           }
           self.file_icons.get(&key).cloned().flatten()
       }
   ```
2. Delete the `let entry_icons = ...` line.
3. At the three draw sites (`app.rs:4128` Details, `:4279` List, `:4353` Icons),
   replace `entry_icons[row_idx]` / `entry_icons[idx]` with a call to
   `self.entry_icon(ctx, &entry.path)`. Note the Details site currently reads
   `} else if let Some(tex) = &entry_icons[row_idx] {` — it becomes
   `} else if let Some(tex) = self.entry_icon(ctx, &entry.path) {` (note: by
   value now, not by reference; `tex.id()` still works).
4. If the borrow checker objects inside a closure that already holds `&mut self`,
   resolve the icon just above the closure for that one row rather than
   restructuring the render — the point is *per drawn row*, not *where exactly*.
5. Remove `ensure_entry_icons` from `src/icon_cache.rs` **only if** nothing else
   calls it (`grep -rn ensure_entry_icons src/`). Keep `file_icon_cache_key`,
   `load_file_icon_texture` and their tests.

**Ceiling accepted (leave a `// ponytail:` note).** Icon extraction stays
synchronous, so scrolling fast into a run of never-before-seen extensions can
still hitch briefly. Moving extraction to a worker thread is the upgrade path if
that shows up in practice — do not build it now.

**Verify.** File icons still render in all three view modes. Opening a large
mixed-extension folder should be noticeably snappier. `cargo test`.

---

## Task 7 — Virtualize the List and Icons view modes

**Problem.** Details view uses `body.rows(...)` (`app.rs:4109`), which is
virtualized. List (`app.rs:4265`) and Icons (`app.rs:4332`) both use
`ScrollArea::…show(ui, |ui| { for (idx, entry) in entries.iter().enumerate() { … } })`,
which builds a widget for every one of N entries every frame.

**Change.** Use `ScrollArea::show_rows`, which only invokes the closure for the
visible row range:

```rust
                            let row_height = (self.font_size + 10.0).max(20.0);
                            egui::ScrollArea::vertical()
                                .id_salt(format!("file_list_pane_{pane_idx}"))
                                .scroll_bar_visibility(ScrollBarVisibility::AlwaysVisible)
                                .show_rows(ui, row_height, entries.len(), |ui, range| {
                                    for idx in range {
                                        let entry = &entries[idx];
                                        // …unchanged body…
                                    }
                                });
```
`row_height` must match what a row actually occupies or the scrollbar length will
be wrong — reuse the same `(self.font_size + 10.0).max(20.0)` the Details view
uses (`app.rs:4045`).

For the **Icons** view the rows are a wrapped grid, not one entry per row, so
`show_rows` does not map cleanly. Two acceptable options, in order of preference:

- **(a)** Compute how many 76 px tiles fit in `ui.available_width()`, treat that
  as `per_row`, call `show_rows(ui, 72.0, entries.len().div_ceil(per_row), …)`
  and inside each row lay out `entries[row*per_row .. ((row+1)*per_row).min(len)]`
  in a `ui.horizontal(...)`. Replaces `horizontal_wrapped` with an explicit grid.
- **(b)** If (a) turns out to visibly change the layout, leave the Icons view
  alone and note it in the commit message. Do **not** ship a version whose tile
  wrapping looks different from today's.

**Verify.** Switch a large folder between Details / List / Icons: all three show
the same entries in the same order, scroll to the true end of the list, and
selection, double-click-to-open and right-click still work on rows scrolled into
view. `cargo test`.

---

# P2 — cleanup

## Task 8 — Apply the egui style only when it changes

**Problem.** `src/app.rs:4709`–`4826` calls `ctx.style_mut_of(...)` four times per
frame (twice in the `for theme in [Dark, Light]` loop, then once each for the
Dark and Light palettes). Each call clones and rewrites a whole `Style`,
including its `text_styles` map.

**Change.** The block depends only on `self.font_size`. Add a field
`styles_applied_font_size: Option<f32>` to `FileManApp` (init `None`), and wrap
the whole style block in:
```rust
        if self.styles_applied_font_size != Some(self.font_size) {
            self.styles_applied_font_size = Some(self.font_size);
            // …existing style block, unchanged…
        }
```
Leave `ctx.set_theme(self.theme_pref);` (`app.rs:4708`) **outside** the guard —
it is cheap and must run every frame for theme switching to work.

Before writing this, confirm by reading the block that `self.font_size` is the
only `self` field it reads. If it reads anything else, add that to the guard key.

**Verify.** Change the font size in Settings — widgets must resize immediately.
Switch between light and dark theme — colours must change immediately.
`cargo test`.

---

## Task 9 — Sort without allocating per comparison

**Problem.** `src/fs_entry.rs:64` and `:71`:
```rust
                a.name.to_lowercase().cmp(&b.name.to_lowercase())
```
Two `String` allocations per comparison, i.e. ~2·n·log n allocations per sort.

**Change.** Compare case-insensitively without allocating:
```rust
fn name_ci(a: &str, b: &str) -> std::cmp::Ordering {
    a.chars()
        .flat_map(char::to_lowercase)
        .cmp(b.chars().flat_map(char::to_lowercase))
}
```
and use `name_ci(&a.name, &b.name)` at both sites. Apply the same to
`src/fs_entry.rs:100` (`list_subdirs`'s `sort_by_key` allocating a lowercased
String per element) only if it is a `sort_by_key` — leave it if changing it makes
the code longer than it saves.

**Verify.** The existing sort tests in `src/fs_entry.rs` must pass unchanged. Add
one test asserting mixed-case ordering matches the old behaviour, e.g.
`["Apple", "banana", "Cherry"]` sorts in that order.

---

## Task 10 — Cheaper tree nodes

**Problem.** `FileManApp::show_dir_node` (`src/app.rs:~1180`) per node per frame:
- `format!("tree_{}", dir.display())` for the `id_salt` (`app.rs:1218`)
- `self.tree_subdirs_cache.get(dir).cloned()` clones the whole child `Vec<PathBuf>`
  (`app.rs:1228`)
- two `to_string_lossy().to_lowercase()` calls for the active/ancestor test
  (`app.rs:1190`–`1193`)

**Change.**
- `id_salt` accepts any `Hash` value in egui 0.36 — pass `dir` directly
  (`.id_salt(dir)`) instead of formatting a String. Confirm against the egui
  0.36 signature first; if it requires `impl std::hash::Hash`, `&Path` satisfies it.
- For the child list, take the `Vec` out of the map, iterate, and put it back —
  same pattern as Task 5 — or hold the cache in `Rc<Vec<PathBuf>>` and clone the
  `Rc`. Pick whichever is the smaller diff.
- The lowercase comparison can use the `name_ci`-style approach from Task 9, or
  simply `dir.as_os_str().eq_ignore_ascii_case(...)`-style comparison — but only
  if it preserves today's behaviour exactly (this is Windows path matching; if in
  doubt, leave it).

This task is optional polish. **If any part of it turns out to be more than a few
lines, skip that part and say so** — the tree is not the main cost.

**Verify.** Sidebar expands/collapses, highlights the active folder, and scrolls
to it after navigation, exactly as before. `cargo test`.

---

## Task 11 — Release profile

**Change.** Append to `Cargo.toml`:
```toml
[profile.release]
lto = "thin"
codegen-units = 1
```
This lengthens release builds; if CI build time matters more than a few percent
of runtime, skip it and say so.

**Verify.** `cargo build --release` succeeds, the produced exe launches.

---

## Done criteria

- All 11 tasks either landed or explicitly reported as skipped with a reason.
- `cargo test` green (118+ tests), `cargo clippy --all-targets -- -D warnings` clean.
- Manual smoke test: open a folder with 5 000+ files; scroll, Ctrl+A, right-click,
  switch view modes, drag the window, drag the tree divider, restart the app and
  confirm the session restored.
- Version bumped to `0.1.11`, CHANGELOG entry added.
