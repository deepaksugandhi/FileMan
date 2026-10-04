# FileMan RAM reanalysis: controlled renderer experiments

**Implementation update, 2026-10-04:** the memory hint and workload-growth fixes are now implemented. See [implementation and validation](RAM-IMPLEMENTATION.md). Statements below about unapplied changes describe the earlier audit snapshot.

2026-10-03. This supersedes the optimization priorities in the initial [RAM audit](RAM-AUDIT.md). The original cache/search findings remain valid, but there is now a measured, much larger opportunity in the renderer's allocation policy.

## Main result

**Keep the current renderer and set `wgpu::MemoryHints::MemoryUsage` first.** In three isolated release tests, this reduced private committed memory from about **275 MiB to 95–96 MiB**, a saving of roughly **180 MiB / 65%**. Resident working set fell from roughly **360–369 MiB to 180–189 MiB**.

**Glow/OpenGL is a second, larger option:** about **50–56 MiB private commit and 125–127 MiB resident** in three runs. That saves roughly **220–226 MiB private commit / 80–82%** compared with the default renderer on this machine. It changes the rendering backend and needs broader driver, remote-desktop, visual and interaction testing before becoming the default.

These are experimental results, not changes already shipped in the normal application. Production source and the original release executable were preserved. All diagnostic modifications live under `target/ram-reanalysis/lab-src`.

## Why the previous analysis missed the largest saving

The application leaves `NativeOptions` at its default renderer configuration. That chooses wgpu, whose default device descriptor uses `MemoryHints::Performance`. In the installed **wgpu-hal 30.0.1** implementation, the graphics allocator is configured as follows:

| Policy | Device block range | Host block range |
|---|---:|---:|
| Performance, current default | 128–256 MiB | 64–128 MiB |
| MemoryUsage | 8–64 MiB | 4–32 MiB |

These are allocation block settings, not a cap on actual graphics resources or a guarantee that every block is allocated. On this integrated GPU, reducing the starting sizes is consistent with the approximately 180 MiB reduction measured in process private commit. The [upstream allocator implementation](https://docs.rs/wgpu-hal/30.0.1/src/wgpu_hal/lib.rs.html#437-455) documents the actual values.

A diagnostic global allocator counted successful Rust allocation/reallocation requests minus frees, while continuing to delegate to the normal System allocator. The default renderer's settled live Rust allocation payload was only **2.79 MiB**, with a peak around **4.07 MiB**. Egui reported **four textures totaling 1.02 MiB of logical texture payload**. This rules out hundreds of MiB of live Rust Strings, Vecs, or font textures as the explanation for this particular idle session.

The allocator counter excludes allocator metadata/retained heap pages, native C allocations, DLL mappings, OS allocations, and GPU/driver allocations. Texture metadata is not total GPU memory and should not be added to Rust live bytes as an independent total. The comparison proves a large graphics-allocation effect; it does not attribute every remaining byte.

## Controlled results

All cases used a release build of the current source, a fresh process, a separate copy of the **same saved database**, the same restored session, and the same **1938 × 1060** outer window size. The original user database was not modified. Tests ran sequentially; test processes were closed after sampling. Normal cases settled for 25 seconds and were sampled over another five seconds. The final memory-saving repeat settled for 55 seconds before sampling.

The diagnostic renderer reported **Intel UHD Graphics 770**, driver **101.5592**, with **Vulkan** selected by the automatic/default backend policy. The diagnostic build adds allocator counters and an environment-controlled renderer selector; it does not modify directory listing, search, caching, fonts, or file operations. Its default-mode control reproduced the normal release footprint within about 1 MiB of private commit.

| Configuration | Private commit, MiB | Resident working set, MiB | Interpretation |
|---|---:|---:|---|
| Original release, default, run 1 | 275.70 | 369.00 | Baseline reproduced |
| Original release, default, run 2 | 274.98 | 359.65 | Private commit stable; residency varies |
| Original release, DX12 only | 309.03 | 341.64 | Worse private commit; do not select blindly |
| Original release, Vulkan only | 252.70 | 334.78 | Modest saving from restricting initialization |
| Diagnostic default renderer control | 275.94 | 369.20 | Instrumentation did not materially change commit |
| Same memory-hint test executable, policy off | 275.49 | 359.91 | Direct control for memory-hint cases |
| MemoryUsage, automatic backends, run 1 | 95.62 | 189.30 | Large saving without forcing a backend |
| MemoryUsage, automatic backends, run 2 | 96.31 | 181.08 | Reproduced |
| MemoryUsage, automatic backends, run 3 | 94.70 | 179.53 | One-minute sample; actual app window answered WM_NULL |
| MemoryUsage, Vulkan only | 72.35 | 154.57 | Additional saving, narrower backend policy |
| Glow/OpenGL, run 1 | 49.51 | 124.79 | Lowest tested baseline |
| Glow/OpenGL, run 2 | 55.50 | 126.98 | Reproduced |
| Glow/OpenGL, run 3 | 54.96 | 126.54 | Reproduced; actual app window answered WM_NULL |

Raw results, module lists, and allocator telemetry are saved in [ram-reanalysis-results.json](ram-reanalysis-results.json). Resident memory varies with OS residency decisions and other system work; private commit was the primary comparison metric. Some first-pass cases ran while the diagnostic build compiled, which may affect residency. The direct control and memory-hint repeats ran after compilation finished.

No long interaction workload, transfer, search, repeated navigation, or hardware matrix was benchmarked. Zero CPU in short idle samples means below the sampler's displayed precision, not literally no computation. A Win32 PrintWindow capture returned the window frame with a blank GPU client surface, so it was not used as evidence of visual correctness. Window creation, visibility, UI-updated title and explicit responsiveness were checked; full visual QA remains required.

## Recommended implementation

### 1. Change the memory hint; preserve automatic backend selection

The [proposed patch](ram-memory-hint.patch) changes only `src/main.rs`. It wraps the existing device descriptor callback and sets:

```rust
descriptor.memory_hints = eframe::wgpu::MemoryHints::MemoryUsage;
```

Preserving that callback keeps eframe's required graphics limits. Mutating `NativeOptions::default().wgpu_options` also preserves eframe's existing low-latency surface configuration. There is no additional dependency, cache architecture, or user setting needed for this change.

The diagnostic implementation of this setting compiled and was tested with the same backend and equivalent surface configuration as the control. The production patch remains unapplied for this analysis request. Before shipping, exercise scrolling, resizing, both panes, Icons view, settings/fonts and drag/drop, plus a large folder and a long session. Smaller graphics pools can require additional allocations as demand grows; the measured idle saving is not a claim of identical frame-time performance under every workload.

### 2. Consider Glow if a lower baseline is still needed

The experiment enabled the existing eframe `glow` feature and explicitly selected `eframe::Renderer::Glow`. It retained wgpu in the build for a controlled renderer switch; the reduction came from using Glow at runtime, not from stripping dependencies. Eframe exposes both options in its [renderer API](https://docs.rs/eframe/0.36.1/eframe/enum.Renderer.html).

A minimal adoption would enable that feature in Cargo.toml and set NativeOptions.renderer. Preserve accessibility. Initially retain a wgpu fallback for machines where OpenGL initialization/rendering fails; do not assume one driver's result generalizes to every low-end PC or remote desktop. Removing unused renderer compile features is a later packaging improvement and has not been measured here.

### 3. Treat Vulkan-only as optional additional tuning

`WGPU_BACKEND=vulkan` is already supported by the installed dependency. Combined with MemoryUsage it reached 72 MiB private commit on this machine, versus 96 MiB with automatic backends. That extra saving is smaller than the allocator-policy saving and sacrifices fallback choices. DX12-only increased private commit in the tested configuration. Do not hardcode DX12 or Vulkan as a universal RAM fix based only on lower working set.

### 4. Then fix workload-dependent growth

These source findings were rechecked against the unchanged current code and remain relevant after the renderer baseline is reduced:

| Priority | Source | Change | What it addresses |
|---|---|---|---|
| Next | `src/tab.rs:142` | Cache filtered/sorted indices rather than deep-cloned FsEntry values | Large directories and many tabs; prior probe measured about 20 MiB removable duplicate payload per 100k synthetic entries |
| Next | `src/app.rs:4713` | Virtualize Icons grid rows using visible ranges | All-entry widget creation and shell icon lookup in large directories |
| Next | `src/app.rs:897` | Load active tabs on demand; release inactive data at lifecycle boundaries | Eager scanning and retained listings for restored/background tabs |
| Next | `src/app.rs:350`, `:414` | Bound per-file icon and tree caches; prune orphaned jobs | Memory growth over extended navigation |
| Next | `src/app.rs:8535` | Defer native shell query until the Explorer submenu opens | Third-party shell-handler loading from ordinary context-menu opens |
| Next | `src/search.rs:18`, `src/app.rs:6224` | Cancel obsolete searches, stream enumeration, bound queues/results, sort on change | Broad searches and repeated Find operations |
| Later | `src/native_drag.rs:499` | Stream virtual attachments to disk | Large drop-related memory peaks |

The old long-lived debug process contained third-party shell extensions; these experiments did not open their menus. Their incremental memory remains unmeasured. Similarly, the 100k-entry probe is a scaling example, not the current saved session's allocation total.

Font trimming, removing SQLite, changing allocators, reducing thread stack reservations, and repeated `shrink_to_fit()` calls are poor first moves for the measured idle footprint. Current live Rust allocations are far smaller than the graphics-pool saving. Forcing working-set trimming can change Task Manager's number without removing retained allocations and was not used.

## Reproduction and limitations

The original executable is archived at `target/ram-reanalysis/original/fileman.exe`; its SHA-256 is `C4B2D95843136B8C464253D5A4446D749DA197C3D65AF5155662BDEE59861000`. `target/release/fileman.exe` was restored byte-for-byte after diagnostic builds. Existing user edits in app.rs and win_default.rs were retained.

The experiment harness is `target/ram-reanalysis/Run-Case.ps1`; the instrumented source and allocator are under `target/ram-reanalysis/lab-src`. The sampler remains [Measure-Memory.ps1](Measure-Memory.ps1). Example direct comparison using the already-built diagnostic executable:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File target/ram-reanalysis/Run-Case.ps1 -Name control-repeat -FrameLatency 1 -Executable target/ram-reanalysis/memory-hint/fileman.exe
powershell -NoProfile -ExecutionPolicy Bypass -File target/ram-reanalysis/Run-Case.ps1 -Name memory-repeat -FrameLatency 1 -MemoryHint memory -Executable target/ram-reanalysis/memory-hint/fileman.exe
```

Both use frame latency 1, matching eframe NativeOptions' default. The explicit setting matters because constructing a standalone WgpuConfiguration otherwise defaults to its high-throughput surface profile. Reducing frame latency below the app's current default is therefore not an additional proposed saving.

The results establish a substantial reduction on this PC. They do not establish a universal minimum RAM requirement, prove the original slowdowns were caused by paging, or replace regression testing on affected machines.
