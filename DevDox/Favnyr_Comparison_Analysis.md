# FileMan vs Favnyr — why Favnyr feels faster

Date: 2026-10-27. FileMan at v0.1.16 (`8f9e518`). Favnyr at v0.3.7 (`89c2f99`, 2026-10-25).
Favnyr snapshot (source only, no `.git`): `DevDox/reference/Favnyr/`
Upstream: https://github.com/GlitchAwakened/Favnyr — **GPL-3.0-or-later**. Reference for ideas only; do not copy code into FileMan without a license decision.

> Scope caveat: this is a static read of both code bases plus FileMan's own earlier perf docs. **Nothing was profiled or benchmarked.** Items marked *(inferred)* are hypotheses to verify with a profiler before acting.

## 1. The two apps side by side

| | Favnyr | FileMan |
|---|---|---|
| UI toolkit | **Slint 1.18.1** (retained-mode, declarative `.slint`) | **egui/eframe 0.36** (immediate-mode) |
| Renderer | Slint's own (not pinned in Cargo; default) | **wgpu** (Vulkan on dev machine) |
| Structure | 2 crates: `favnyr-core` (fs, thumbnails, config; no UI) + `favnyr-gui` | 1 crate; `src/app.rs` is **9,386 of 16,787 lines** |
| Release profile | opt-level 3, thin LTO, cgu=1, **strip=true** | thin LTO, cgu=1 (no strip) |
| Exe size | single portable exe | 19.5 MB (`target/release/fileman.exe`) |
| Dir listing | `fs.rs` "streams a directory" (`list_dir_counted`) | `fs_entry::list_dir` builds a full `Vec`, sent over `mpsc` when complete |
| Change detection | `notify` watcher + debounced re-list | refresh on window focus regain; no watcher (`app.rs:5784` note) |
| Thumbnails/icons | bounded in-memory cache, decode size/pixel budgets (`MAX_DECODE_PIXELS` etc.) | `SHGetFileInfoW` per icon, texture per ext (per path for exe/lnk/ico) |
| Deps | lean, `default-features=false` throughout | `rusqlite` bundled, `windows`, `zip`, `tar`, `flate2`, `rfd`, … |

## 2. Why Favnyr is faster — ranked by likely impact

### A. Retained-mode vs immediate-mode UI (structural, biggest)
egui re-runs the whole `ui()` function on every repaint (hover, mouse move, any `request_repaint`). With a 9k-line app that does layout, sorting/filtering lookups, tree walks and widget building each frame, per-frame cost scales with everything on screen. Slint only updates the nodes whose properties changed, and list models are virtualized by the toolkit. You cannot "fix" this inside egui; you can only make each frame cheaper (section 3) or migrate (not recommended — see section 5).

### B. Renderer / memory footprint
`docs/RAM-REANALYSIS.md` already measured it: wgpu default ≈275 MiB private commit → ≈95 MiB with `MemoryHints::MemoryUsage` (shipped) → **≈50 MiB with glow/OpenGL**. wgpu device/swapchain init also dominates cold start *(inferred)*. Slint's default backends are far lighter than a wgpu Vulkan device. Switching eframe to `glow` is the cheapest lever that gets closer to Favnyr's profile; it needs driver/RDP testing.

### C. Listing is all-or-nothing
FileMan's background job returns one `Vec<FsEntry>` after the whole directory is read, so nothing appears until the last entry is stat'd. Favnyr streams. For 50k-entry or network folders the first paint is much later in FileMan *(inferred from code shape, not timed)*.

### D. Icons resolved on the UI thread
`icon_cache.rs` calls `SHGetFileInfoW` synchronously when a visible row misses the cache. Extension-keyed icons are cheap after the first hit, but `exe`/`lnk`/`ico` are cached **per full path**, so scrolling a folder of executables does shell calls on the UI thread → hitches *(inferred)*. Favnyr keeps decode work in `favnyr-core` with explicit byte/pixel budgets.

### E. Monolithic app and per-frame work
`FileManApp` + `ui()` in one file makes it hard to see what runs every frame. Remaining per-frame suspects to audit: `request_repaint*` sites (14 in `app.rs`), 50 ms repaint timers while jobs are pending (`app.rs:927`, `5393`), 3 s drive-poll wakeup (`app.rs:955`).

### F. Minor
No `strip`, no `panic = "abort"`, heavy always-linked deps (bundled SQLite etc.). Few-percent / size wins only.

## 3. What FileMan already fixed (don't redo)
Commits since v0.1.10 cover `docs/PERF_PLAN.md` tasks 1–11: drive-list cache, 500 ms session-persist debounce, no per-frame listing clone, lazy icons per drawn row, `show_rows` virtualization for List/Icons, context-menu paths on demand, in-memory favourites, style only on font-size change, allocation-free sort, cheaper tree nodes, thin LTO. Also RAM work (`063f52f`). Details view already uses `TableBuilder` `body.rows(...)`.
Note: `docs/PERF_PLAN.md` still says "Status: not started" — stale; update it.

## 4. Recommended next steps (in order)
1. **Measure first.** Add a frame-time overlay (egui `ctx.input(|i| i.unstable_dt)` + a `puffin`/`tracing` span around `ui()`), then record: cold start to first paint, idle CPU, frame time scrolling a 50k-file folder, frame time while hovering the tree. Without numbers we are guessing at A–E.
2. **Try `glow` renderer** behind a setting/feature flag; compare against `docs/ram-implementation-results.json` baselines (start time, private commit).
3. **Stream the listing**: send chunks (e.g. 500 entries) over the channel, render partial, sort at end. Keep UI responsive for network folders.
4. **Move non-extension icon extraction off the UI thread** (request queue + background worker; show generic icon until ready).
5. **Idle-repaint audit**: confirm CPU ≈ 0 when nothing changes; replace 50 ms timers with `request_repaint` from the worker threads.
6. **Add a real watcher** (`notify` + debounce) — freshness more than speed, but removes full re-lists on focus regain.
7. **Split `app.rs`** into modules (panes, tree, toolbar, dialogs) and extract a UI-free core crate like Favnyr's — makes per-frame cost visible and testable.
8. Release profile: `strip = true`, optionally `panic = "abort"`.

## 5. Toolkit migration?
Rewriting in Slint would give Favnyr-like behavior but means rewriting ~9k lines of UI plus drag/drop, shell context menu and taskbar integration. Not justified until steps 1–5 are measured; egui + glow + streaming + async icons should close most of the gap.

## 6. Where to look in Favnyr
- `crates/favnyr-core/src/fs.rs` — streaming listing, metadata rules (`:496`–`:620`), natural sort via `sort_by_cached_key`
- `crates/favnyr-core/src/thumbnail/` — decode budgets, bounded caching
- `crates/favnyr-gui/src/bridge.rs` (21k lines) — UI↔core glue, `notify` watcher (`:18450`), debounced re-list
- `crates/favnyr-gui/src/ui/*.slint` — declarative UI
- `Cargo.toml` — `default-features = false` discipline, `cargo-deny` (`deny.toml`)

## 7. Implementation log (2026-10-27) — uncommitted working tree

| Step | Done | Where |
|---|---|---|
| 1 Measure | `FILEMAN_PERF=1` / Ctrl+Shift+F12: overlay + `%APPDATA%\FileMan\perf.log` (cold start, avg/p95/max `ui()` ms, frames/s) | `src/perf.rs` |
| 2 glow | Selectable renderer (Settings > Appearance, or `FILEMAN_RENDERER=glow`); default stays wgpu | `main.rs`, `Cargo.toml` |
| 3 Streaming | `list_dir_streaming` batches (256 first, then 100 ms / 4096); progressive when nothing on screen, whole-swap on refresh; bad entries skipped | `fs_entry.rs`, `app.rs` |
| 4 Async icons | Worker thread + COM init, 64 in-flight cap, UI gets `None` (text fallback) until ready | `icon_cache.rs` |
| 5 Repaints | Listing worker calls `request_repaint` itself (fallback poll 50→250 ms); op-progress capped 33 ms | `app.rs` |
| 6 Watcher | `notify` non-recursive on each pane's folder, 250 ms debounce | `src/dirwatch.rs` |
| 7 Split | help, menus, nav icons, tests moved out (9.4k→8.6k lines). `ui_body` (~2.6k lines) still monolithic | `src/app/` |
| 8 Profile | `strip = true` | `Cargo.toml` |

Tests: 123 pass (added streaming + watcher tests). `cargo clippy -D warnings` still fails on ~50 *pre-existing* lints (collapsible_if etc.); none from new code.

### Measurements (release, same DB copy, 3 runs each, noisy, dev machine)
| | first UI done (ms) | working set (MiB) | private (MiB) |
|---|---|---|---|
| wgpu | 489 / 2306 / 498 | 271–276 | 95–99 |
| glow | 290 / 685 / 358 | 196–259 | 63–127 |

Reading: glow starts faster and usually lighter, but the spread is large and the earlier RAM-REANALYSIS "50 MiB" figure was **not** reproduced at 12 s of uptime. Idle CPU ≈ 0 for both after settling (one glow outlier at 9–12% right after start). Default left on wgpu until visual/RDP checks. Exe: 20.2 MB (glow feature adds size).

### Not verified
Streaming, async icons and the watcher were verified by unit tests, compile and clean app startup — **not** by interactive use on a 50k-file or network folder. Do that before release. Next candidate: split `ui_body` itself, and a 50k-file benchmark using the perf overlay.
