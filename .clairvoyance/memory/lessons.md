---
type: shared
created: 2026-10-23T00:00:00.000Z
relevance: When changing FileMan architecture, directory listings, caching, search, icons, tabs, or other runtime behavior
tags: [performance, memory, fileman]
---

# Lessons

- 2026-10-23: Always keep FileMan's memory footprint low. Prefer bounded caches, shared or borrowed data, lazy loading, pagination or virtualization, and prompt cleanup of background work; avoid cloning large directory listings or retaining unbounded search, icon, and tab state.
