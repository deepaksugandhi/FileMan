# RAM reductions implemented

2026-10-04. Follow-up to [RAM reanalysis](RAM-REANALYSIS.md).

## Release measurements

Same cloned saved session, automatic graphics backend selection and 1938 × 1060 window on this machine. Each sample spans five seconds after settling; all three application windows answered the responsiveness probe. No forced working-set trimming was used.

| Build/run | Settle time | Private commit | Resident working set |
|---|---:|---:|---:|
| Original release, fresh control | 25 s | 274.54 MiB | 359.00 MiB |
| Updated release | 25 s | 94.59 MiB | 179.41 MiB |
| Updated release, repeat | 45 s | 93.51 MiB | 178.29 MiB |

**Private commit fell approximately 66%; resident memory fell approximately 50%.** These are idle release measurements on this PC, not a guarantee for every driver or workload, and are separate from the original long-running process reported around 440 MB.

Raw samples: [ram-implementation-results.json](ram-implementation-results.json). Tested executable: `target/release/fileman.exe`, SHA-256 `BEFDC2B0B75B959E08D024E68303FABA1DE60B615C820EA0DCD68F73B0DBDC75`. Test instances were closed; the user's saved session was not modified.

## Changes

- **Renderer:** use `wgpu::MemoryHints::MemoryUsage` while preserving eframe's device limits, backend selection and surface configuration. No new dependency or renderer switch.
- **Directory views:** keep one owned listing and cache filtered/sorted indices. Rendering borrows the listing through `Arc`; navigation releases stale rows. Only active tabs are loaded; inactive tabs retain navigation/selection state and reload when activated.
- **Icons view:** render visible grid rows with `ScrollArea::show_rows`. Row height accommodates the configured font; truncated filenames have full-name tooltips. Offscreen entries no longer create widgets or request shell icons.
- **Caches:** LRU eviction for file icons (256 entries, approximately 8 MiB of pixel/key payload) and tree listings (128 directories, approximately 8 MiB of path/vector payload). A single oversized tree listing is retained so it remains usable without repeated scans. Bounds exclude allocator/map overhead and renderer allocation pools. Completed tree jobs are drained even after their nodes collapse; normal scheduling admits at most four tracked tree jobs.
- **Find:** stream directory enumeration, skip traversal through symlinks/reparse points, cancel replaced/closed searches, bound the result channel to 256 entries and consume at most 256 per frame. Stop at 10,000 results with a visible instruction to narrow the search. Sort only when results or sort settings change. Cancellation is cooperative; an already blocked filesystem call must return before its worker can exit.
- **Context menus:** query native shell handlers only when the Windows Explorer submenu opens.
- **Virtual-file drops:** write `IStream` attachments through a 64 KiB buffer and write HGLOBAL contents without an extra full-file copy. Reject failed/partial transfers and invalid attachment paths, and release COM media on completion. Memory supplied by the source application itself remains outside this optimization.

## Validation

- `cargo test --offline`: 121 passed. Includes indexed filtering/sorting, cache eviction/accounting, search cancellation/limits, a headless 10,000-folder Icons view with fewer than 100 rendered entries, inactive-tab release/reactivation, and a multi-chunk Windows COM stream with write-failure handling.
- The Icons-view check passed again after adapting row height for large fonts.
- `cargo build --release --offline`: optimized application build.
- `git diff --check`: no whitespace errors. Pre-existing edits in `app.rs` were retained; `win_default.rs` matches the pre-implementation backup byte for byte.

The [100,000-entry probe](ListingMemoryProbe.rs) measures owned vector/string/path capacities, not whole-process RAM:

| Display cache | Before | After |
|---|---:|---:|
| All 100,000 entries | 21,800,000 bytes | 1,048,576 bytes |
| Filtered to 1,000 entries | 8,930,000 bytes | 8,192 bytes |

The underlying listing remains 30,400,000 bytes in this deliberately long-path synthetic case. See [before](memory-listing-probe.txt) and [after](memory-listing-probe-after.txt) output.

## Remaining compatibility testing

Glow/OpenGL and Vulkan-only selection remain optional experiments, as recommended in the reanalysis. Automatic backend fallback is retained. Tests here do not replace visual/interaction checks on affected low-memory machines, remote desktops, third-party shell extensions or actual Outlook attachment drops.
