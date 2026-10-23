---
type: shared
created: 2026-10-23T00:00:00.000Z
relevance: When changing FileMan architecture, directory listings, caching, search, icons, tabs, or other runtime behavior
tags: [performance, memory, fileman]
---

# Lessons

- 2026-10-23: Always keep FileMan's memory footprint low. Prefer bounded caches, shared or borrowed data, lazy loading, pagination or virtualization, and prompt cleanup of background work; avoid cloning large directory listings or retaining unbounded search, icon, and tab state.
- 2026-10-23: Size fixed bottom panels from `ui.spacing().interact_size` plus room for separators and margins; a hard-coded 22-point status panel clips the lower half of text at FileMan's default Windows font size.
- 2026-10-23: Keep folder-tree reveal state alive until the lazily loaded target node is actually rendered. A one-frame force-expand flag expires before background subdirectory scans finish and leaves deeper parents collapsed.
- 2026-10-23: Settings and other modal-style windows should close on Escape, except when a nested capture mode owns Escape (for example, cancelling keyboard-shortcut capture without closing Settings).
- 2026-10-23: Avoid private-use icon-font glyphs for FileMan's essential navigation controls. Windows fonts can claim those codepoints and display misleading characters; clear text labels are more robust and understandable.
