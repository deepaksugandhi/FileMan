# FileMan memory and responsiveness audit

**Updated priorities:** the subsequent [controlled renderer reanalysis](RAM-REANALYSIS.md) found a roughly 180 MiB private-commit saving from wgpu's built-in memory hint, and a larger Glow alternative. Read that measured result before acting on the original priority list below.

Audit date: 2026-10-03. Scope: current Rust source, locally installed renderer source, and read-only measurements of the existing Windows process. No application source was changed or running window restarted.

## Follow-up: fresh release build measurement

At the user's request, `cargo build --release --locked --offline` succeeded on October 3 in 2m 11s. The new optimized executable is 19,316,224 bytes. The earlier debug instance had already exited. The release test used a copy of the saved settings database under `target/ram-check/appdata`, leaving the original session database untouched. The actual FileMan window was confirmed visible, displaying the restored network-share folder. No interaction workload or shell-context-menu test was performed.

| Time since launch | Working set | Private commit | Peak working set | Sampled CPU, one core |
|---|---:|---:|---:|---:|
| Approximately 10 seconds | 368.56 MiB | 275.28 MiB | 385.53 MiB | 0.00% |
| Approximately 60 seconds | 368.34 MiB | 275.02 MiB | 385.53 MiB | 0.00% |
| Approximately 125 seconds | 368.28 MiB | 274.90 MiB | 385.53 MiB | 0.00% |

Saved [release snapshot](memory-release-baseline.json). The release footprint is stable in this short idle test and below the earlier debug session, but **this is not a controlled build-mode comparison**: the prior debug process had eight days of usage, an older executable, and loaded third-party shell handlers. Both Direct3D and Vulkan libraries are still loaded in the fresh release process. The following original audit remains useful; its statement that no fresh release measurement was performed describes the initial audit, superseded by this follow-up. No RAM-reduction source changes were applied.

## Findings at a glance

The reported footprint is credible. The running instance has roughly **462 MiB private commit**, **610–635 MiB working set**, and a historical **914 MiB peak working set**. The strongest opportunities are renderer/driver initialization, third-party shell extensions, duplicated directory data, and unbounded caches. Icons view and recursive Find also contain CPU-heavy paths that can make the app slow without memory pressure.

**Do not treat all 440 MB as a Rust heap leak.** Working set includes shared resident pages; private commit is a different metric and need not all be resident. Microsoft explains these distinctions in its [working-set documentation](https://learn.microsoft.com/en-us/windows/win32/procthread/process-working-set) and [memory-counter reference](https://learn.microsoft.com/en-us/windows/win32/memory/memory-performance-information).

## What was actually measured

Saved evidence: [process snapshot](memory-baseline.json), [listing probe output](memory-listing-probe.txt).

| Metric | Observed value | Interpretation |
|---|---:|---|
| Executable | `target/debug/fileman.exe` | Debug build, not the installer release build |
| Process start | September 25 | Eight-day instance; workload history unknown |
| Binary modification | September 9 | Running binary predates current audit; not evidence that every current source path ran |
| Working set | 610.03 MiB in saved sample; 634.75 MiB earlier | Resident memory, including shared pages |
| Private commit | 462.12 MiB | Private committed memory; not an exact Rust allocation total |
| Peak working set | 914.20 MiB | Historical peak, workload unknown |
| CPU during samples | 0.16% and 1.24% of one core | Quiet sampling windows; not a measurement of slow interactions |
| Threads / handles | 35 / 1,259 | Includes runtime, OS, drivers, extensions, and application work |
| Committed MEM_PRIVATE regions | 433.03 MiB | Virtual region classification |
| Committed MEM_MAPPED regions | 118.48 MiB | Virtual region classification |
| Committed MEM_IMAGE regions | 531.61 MiB | Loaded image regions; not their resident RAM |

The three region totals must **not** be added to working set or interpreted as resident memory. Copy-on-write image/mapped pages retain their original VirtualQueryEx classification, which also explains why MEM_PRIVATE differs from private commit. See [VirtualQueryEx](https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-virtualqueryex).

Existing uncommitted edits in `src/app.rs` and `src/win_default.rs` were preserved. Current source already has several optimizations that the older process may lack. A fresh build is necessary for controlled before/after comparisons. No controlled release or renderer A/B comparison was performed, so no total saving is claimed.

## Prioritized changes

| Priority | Finding | Smallest useful change | Expected effect / confidence |
|---|---|---|---|
| P0 | Debug renderer overhead and multiple graphics backends | Fresh release baseline; compare one backend at a time | Potentially substantial fixed footprint; saving unmeasured |
| P0 | Heavy shell extensions loaded into FileMan | Query Explorer menu only when its submenu is opened | Avoids menu-handler loading for ordinary right-clicks; source and loaded modules confirmed |
| P1 | Every displayed tab owns a second listing | Cache sorted/filtered indices into the listing | About 20 MiB less owned payload per 100k-entry synthetic display cache |
| P1 | Icons view creates all item widgets | Virtualize fixed-height grid rows | Large responsiveness gain in big folders; exact RAM saving unmeasured |
| P1 | Inactive tabs retain listings and are eagerly scanned | Load active tabs only; release inactive display caches/listings | RAM scales mainly with two visible panes instead of all tabs |
| P1 | File icon and tree caches have no limit | Bounded cache, protected visible entries | Prevents retained memory growing indefinitely across navigation |
| P1 | Find walk, queues, and results are unbounded | Streaming walk, cancellation, bounded transport and result storage | Prevents runaway memory and obsolete searches consuming I/O |
| P2 | Pending jobs request immediate repaints | Worker completion wake-ups or modest polling interval | Less CPU/GPU work during slow I/O |
| P2 | Virtual drops buffer entire attachments | Copy COM stream directly to temporary file | Peak memory no longer proportional to attachment size |
| P3 | Small resource leaks, font and build overhead | Fix native handle cleanup; trim only unused features | Correctness/maintenance benefit; unlikely to explain baseline alone |

### 1. Establish a release baseline and reduce unnecessary backend initialization

Evidence: [Cargo.toml](../Cargo.toml), [main.rs](../src/main.rs#L93), local `eframe-0.36.1/src/epi.rs` and `egui-wgpu-0.36.1/src/setup.rs`.

`eframe` defaults enable wgpu. NativeOptions leaves the renderer unspecified, selecting Wgpu. The installed setup defaults to primary backends plus GL and enumerates adapters. Debug builds enable validation/debug flags by default. The process has `D3D12SDKLayers.dll`, Direct3D DLLs, `vulkan-1.dll`, `igvk64.dll`, and several large Intel graphics/compiler DLLs loaded. This supports investigating graphics initialization, but does not prove which backend renders the window or how much private heap belongs to each driver.

First compare a freshly compiled release build. Then compare release with `WGPU_BACKEND=dx12` and `WGPU_BACKEND=vulkan`, in separate fresh processes. These environment overrides already exist in the installed dependency; no custom configuration system is required. The existing release binary is older and is not a valid same-source control.

If restricting backends measurably helps, adopt an explicit supported backend policy with a fallback for unsupported hardware. A Glow comparison is also justified, but select it explicitly through NativeOptions and enable its feature. Changing to Glow is **not** a guaranteed RAM reduction: OpenGL driver behavior varies. [eframe's renderer API](https://docs.rs/eframe/0.36.1/eframe/enum.Renderer.html) supports the choice.

Keep accessibility support. The README promises Windows 8.1+, so a hard DX12-only policy would need compatibility review rather than being applied blindly. Smaller executables and fewer dependencies do not automatically imply equivalent runtime RAM savings.

### 2. Stop loading Explorer menu extensions on every ordinary context-menu open

Evidence: [show_entry_context_menu](../src/app.rs#L8535), [shell_menu query](../src/shell_menu.rs#L114).

The native menu is queried before the user opens the Windows Explorer submenu. `GetUIObjectOf` / `QueryContextMenu` can instantiate third-party shell handlers in FileMan. Filtering hidden labels happens **after** querying; hiding labels therefore does not prevent handler loading.

The process contains `drivefsext.dll` (36.39 MiB image size), `qingnse64.dll` (17.88 MiB), `FileMenuTools64.dll` (12.21 MiB), and WPS-related modules. Image sizes are not resident or private memory savings, but they confirm substantial external code is loaded.

Move the query inside the Windows Explorer submenu closure and keep the existing per-selection cache. If this still dominates, provide a setting to disable shell integration; only consider a separate helper process if profiling shows extensions must remain available but their lifetime must be isolated. A simple lazy query is the first change. Icon association lookups can also load shell code, so lazy menu queries may not avoid every extension DLL.

### 3. Replace display-entry clones with indices

Evidence: [Tab storage](../src/tab.rs#L69), [display_entries](../src/tab.rs#L142), [listing result application](../src/app.rs#L870), [render ownership transfer](../src/app.rs#L4332).

Each tab keeps `listing: Vec<FsEntry>` plus a deep-cloned `Vec<FsEntry>` for its filtered/sorted display. Each FsEntry owns both a String name and PathBuf path. Refresh clones worker results into each matching tab; cache regeneration temporarily overlaps the worker/new listing with the old display cache and replacement cache.

The runnable [ListingMemoryProbe.rs](ListingMemoryProbe.rs) uses the real FsEntry and Tab implementation with 100k synthetic entries:

| Owned payload, excluding allocator overhead | Bytes | MiB |
|---|---:|---:|
| FsEntry size | 88 per entry | — |
| Listing, including buffer capacities | 30,400,000 | 28.99 |
| Deep-cloned display cache | 21,800,000 | 20.79 |
| Equivalent usize display indices | 800,000 | 0.76 |
| Current cache filtered down to 1,000 hits | 8,930,000 | 8.52 |

Filtering retains a 100k-element Vec capacity even with only 1,000 survivors. An index cache removes about **20.03 MiB** from the unfiltered display payload in this example, about **96% of the duplicate cache payload**, not 96% of the process. Shorter paths, fewer entries, allocator behavior and multiple tabs change the result.

Keep one owner of entries, and filter/sort indices using the existing comparison semantics. Preserve listing-version invalidation, directories-first sorting, selection/range selection, drag paths, and context-menu behavior. Where only one tab receives a job, move entries rather than cloning; share immutable listings for duplicate directories only if duplication remains significant after the index change.

### 4. Virtualize Icons view

Evidence: [Icons loop](../src/app.rs#L4713), existing Details `body.rows` at line 4467 and List `show_rows` at line 4638.

Details and List already virtualize rows. Icons uses `ScrollArea::show` and `horizontal_wrapped` over the entire listing, creating widgets, labels, drag targets, folder hit-test paths, and icon lookups for all entries. Clipping alone does not skip this application loop. Shell icon extraction runs synchronously during rendering.

Compute columns from available width, and use `show_rows` for fixed-height grid rows, rendering only the entries in visible rows. Match tile sizing/wrapping to the actual fixed row height. Reuse existing selection handlers and global entry indices. Rebuild drop rectangles for visible entries only.

This bounds widget creation and icon demand to the viewport. If visible icon extraction still stalls on network paths or executables, separate Win32 extraction into a small bounded worker queue; upload textures on the UI thread. Do not extract icons for the whole folder in advance.

### 5. Make inactive tabs cheap

Evidence: [poll_listing](../src/app.rs#L897), [tab activation](../src/pane.rs#L35), [navigation](../src/tab.rs#L193).

The listing scheduler scans dirty inactive tabs after the active tab. Session restoration marks every tab dirty, so merely restoring many tabs eventually loads every directory. Previously visited tabs also keep their listing and display cache. A filtered display clone can remain stale after refresh until that tab renders again.

Tab activation already marks its listing dirty. The simplest policy is therefore to load the active tab only and retain only path/history/selection/settings for inactive tabs. Release inactive display caches first; if memory remains high, release their listings too. Mark background tabs dirty after mutations without immediately loading them. This trades faster return-to-tab for less retained RAM; loading feedback should remain visible.

Use replacement with empty Vec/None when releasing caches. `Vec::clear()` and `HashMap::clear()` retain their backing allocations. Avoid shrinking on every frame; release at tab/cache lifecycle boundaries. Releasing owned payload may not immediately lower working set because the allocator/driver can retain freed capacity.

### 6. Bound persistent icon, tree, and job state

Evidence: [cache fields](../src/app.rs#L323), [tree cache/jobs](../src/app.rs#L409), [icon cache keys](../src/icon_cache.rs#L33), [tree job creation](../src/app.rs#L1409).

`file_icons` has no eviction or clear path. Normal extensions share icons, which is already good, but exe/lnk/ico keys retain a texture for each full path visited. A 32x32 RGBA pixel payload is 4 KiB; 10k such textures represent about 39 MiB of raw pixels before GPU allocation and map overhead. Actual icon dimensions vary and conversion allows up to 256x256. Failed lookups retain keys too.

Tree subdirectory vectors survive collapsed branches and navigation. Every uncached expanded node can spawn a thread; there is no global tree-job concurrency limit. A collapsed node's completed job can remain in the jobs map, retaining its queued result until reopened.

Start with a small bounded cache policy, preserving visible entries/ancestors, and prune completed orphaned jobs. Track payload bytes as well as entry counts when tuning limits; one tree cache entry can contain thousands of paths. Share custom-action/launcher icons by executable path if this is measurable. Avoid an elaborate cache framework. Suggested initial experiments: file icons capped at 256–512 keys, tree jobs at 2–4 concurrent requests; these are tuning values, not measured optimal limits.

### 7. Bound and cancel recursive Find

Evidence: [recursive walker](../src/search.rs#L18), [search spawn](../src/app.rs#L1304), [result sorting/filtering](../src/app.rs#L6224), [result drain](../src/app.rs#L6473).

Find calls list_dir for each directory, materializing and sorting every directory even though traversal does not require sorting. Parent directory iterators retain their remaining entries while descending. The unbounded channel and results Vec can grow with the whole search tree. Closing Find drops the receiver, but send failures are ignored, so the worker continues scanning; starting another search can leave obsolete workers running. Directory links/reparse points also need an explicit traversal policy to prevent cycles.

Walk directory entries incrementally, without sorting. Check cancellation between entries/directories and stop on send failure. For prompt cancellation in trees without matches, use a cancellation flag checked during traversal. Use a bounded sync_channel for backpressure, but also bound resident results or spill them to SQLite/temp storage: bounding the channel alone does not bound the results Vec. If using a result ceiling, show truncation and let the user narrow the search; do not silently discard matches.

Sort only when results or sort options change. The current Find UI sorts on every repaint, and folder sorting lowercases/allocates both paths and names per comparison. Cache filtered result indices too. Drain a limited batch or time budget per frame rather than an unlimited loop, so large result bursts do not freeze the UI.

### 8. Reduce repaint and progress churn

Evidence: [listing repaint](../src/app.rs#L921), [tree repaint](../src/app.rs#L1428), [progress repaint](../src/app.rs#L6142), [Find repaint](../src/app.rs#L6494), [progress queues](../src/progress.rs#L107).

Pending listing, tree, Find and file operations ask for immediate repaints. Slow storage/network operations can keep the UI rendering continuously even when no new data arrives. Use worker completion notifications with `ctx.request_repaint()`, or reuse `request_repaint_after` with a modest interval while waiting. Leave repainting for active animation/input intact.

Progress is sent through unbounded queues, often more than once per file, and the UI drains all updates. Coalesce to the latest progress snapshot or rate-limit sender updates. Do not block file transfers solely because a progress queue is full. Keep completion/error delivery reliable.

These changes principally address responsiveness and power use. They do not by themselves establish a large idle RAM reduction.

### 9. Stream virtual-file drops instead of buffering whole files

Evidence: [attachment extraction](../src/native_drag.rs#L434), [read_file_contents](../src/native_drag.rs#L457), [read_stream_to_end](../src/native_drag.rs#L499).

Outlook-style virtual attachments are accumulated in a Vec and only then written to a temporary file. A large attachment causes a correspondingly large transient allocation, potentially larger due to Vec growth. HGLOBAL content is also copied into another Vec.

Copy IStream chunks directly into the temporary file using the existing 64 KiB buffer; write HGLOBAL contents without an extra whole-file copy where possible. Preserve COM apartment rules, medium release, path validation and all I/O error handling. Delete partial output on failure and never report a truncated stream as successful. This is a workload-specific peak reduction, not an explanation of steady idle RAM.

### 10. Secondary items and existing good choices

- `taskbar::colored_icon` does not delete its temporary color/mask bitmaps after CreateIconIndirect, and installed HICON handles have no tracked cleanup. Fix native ownership. It runs once per instance, so this small leak cannot credibly explain hundreds of MiB alone.
- `shell_menu::bind_children` and popup creation have early-return cleanup gaps for PIDLs. Handle cleanup belongs in the existing helpers; ordinary successful menu paths already destroy menus/free children.
- Fonts start with egui defaults plus Inter plus system regular/bold faces. Inter itself is only about 402 KiB. Font application is already gated on family change; do not remove symbol fallback or rebuild fonts per frame. Profile font atlas/layout memory before claiming savings.
- The About logo uses an image loader cache. Its PNG file size is not decoded texture size. Measure decoded dimensions if image memory is material; one logo is a lower priority than growing caches.
- Archive extraction already streams via File and io::copy/tar; normal copy uses fs::copy. Keep those choices. Archive entry metadata still scales with archive complexity, but file contents are not all loaded by application code.
- Recent items are capped at 50. There is no application-wide recursive index, full-text database, browser runtime, or file-preview buffer in the inspected source. SQLite settings/session data are not the first optimization target without heap evidence.
- Current pane rendering moves its cached Vec out and back instead of cloning it every frame; sidebar child vectors do the same. Current source also gates font/style application and debounces persistence at 500 ms. Preserve these improvements rather than implementing them again.

## Verification and rollout

1. Build current source in release and sample a fresh process, same display DPI/window size and same directories. Record startup, 30-second idle, and 5-minute idle. Separate ordinary folders from restored many-tab sessions.
2. Compare default renderer, one wgpu backend, and Glow on target low-memory machines. Record selected adapter/backend, private commit, private/resident working set, and UI timings. Retain hardware compatibility and accessibility.
3. Measure immediately before/after ordinary right-click and opening the Windows Explorer submenu. Compare DLL lists and private commit. Repeat enough times to distinguish first-load cost from growing leaks.
4. Implement index cache and virtualized Icons. Compare 1k/10k/100k-entry folders, first render, scroll, filter, sorting, selection, drag/drop and context menus. Assert sorting/filter results remain identical and rendered widgets scale with visible rows.
5. Open 20 tabs and visit many executable/link folders. Switch away, collapse trees, close tabs, wait for jobs, then sample. Track cache counts/bytes and ensure they plateau at their intended limits.
6. Search a large/deep directory; cancel and restart repeatedly. Confirm obsolete workers stop and results/queues stay within declared limits. Measure UI frame time during result bursts.
7. Test large virtual attachments, stream/write errors, and cleanup. Confirm bounded buffering and exact output bytes.

Use [VMMap](https://learn.microsoft.com/en-us/sysinternals/downloads/vmmap) to distinguish heaps, private data, images, stacks and mapped allocations. Use Windows Performance Recorder/Analyzer for allocation stacks and paging/hard-fault evidence before attributing slow machines to RAM. Dedicated/shared GPU memory should be measured separately; the process snapshot cannot account for all GPU allocations.

Choose a numeric RAM budget **after** the fresh release baseline. The measurements do not justify promising a universal 100 MB process or adding up speculative savings from different subsystems. The practical success criteria are lower fresh-process private commit, bounded growth under navigation/search, and improved interaction timings on affected machines.

## Reproduce the audit checks

The sampler is read-only and needs 64-bit PowerShell. It does not start, stop, or trim the target process. Supply the intended PID when several FileMan instances are open. The ExecutionPolicy override below applies only to this child PowerShell invocation.

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File ./docs/Measure-Memory.ps1 -ProcessId 37004 -SampleSeconds 10
rustc --edition 2024 -O -Awarnings docs/ListingMemoryProbe.rs -o target/memory-audit-probe.exe
./target/memory-audit-probe.exe
```

Completed checks: sampler against FileMan and against its own PowerShell process; optimized listing probe compiled and assertions passed. Owned-byte figures exclude allocator metadata and are not process working-set measurements.

Project-wide build/tests were not run for this audit-only change. `cargo tree --offline -e features -i eframe` could not resolve dependencies because cached tempfile 3.27.0 needed unpacking into the read-only Cargo registry. No permission escalation or dependency installation was attempted; source-level renderer conclusions use the already-installed dependency source. Full project verification remains necessary when application fixes are implemented.
