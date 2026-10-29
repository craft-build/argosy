---
type: Skill
description: Build the project's semantic index and query it from the CLI.
---
# Build and Query the Index

1. Run `argosy index build` in the project root. The first run downloads
   the pinned embedding model (~90 MB); later runs are offline and
   incremental.
2. Check `argosy index status` for staleness — added, changed, and removed
   concepts since the last build — without writing anything.
3. Query with `argosy index query "<text>"`, optionally `-k <n>` for more
   hits and `--namespace`/`--language`/`--tag` filters to scope the legs.
4. On a machine that must not download a model, set
   `index.search_mode = "lexical"` in the user config first; BM25-only
   search works with zero downloads.
