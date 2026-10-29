---
type: Reference
description: Every user-configuration key with its default — search mode, chunk budget, cache paths, session bounds, and trust caps.
tags: [configuration]
---
# Configuration

User configuration is BarkML (`*.bml`) read from `$XDG_CONFIG_HOME/argosy`
(falling back to `~/.config/argosy`). A key set in more than one file is a
collision — an error, never a silent override. `argosy config` prints the
resolved configuration and where it was loaded from.

## index

- `index.model` — the embedding model (default `all-minilm-l6-v2`).
- `index.search_mode` — `"hybrid"` (default: vectors fused with BM25) or
  `"lexical"` (BM25-only, nothing downloaded).
- `index.default_k` — hits per CLI query (default 5).
- `index.mcp_default_k` — hits per MCP search (default 8).
- `index.chunk_max_tokens` — the per-passage chunk budget (default 220,
  validated between 1 and 256, the embedder's truncation limit).

## paths

- `paths.embed_cache_dir` — where the ~90 MB model cache lives (default
  `~/.cache/argosy-embeddings`).
- `paths.index_db_name` — the index file name inside the project slot
  (default `index.db`).

## output, pull, package, catalog, code_tools

- `output.quiet`, `output.json` — CLI verbosity defaults (both false).
- `pull.default_global` — whether pull installs to the user-wide store
  (default false).
- `package.format` — `"dir"` or `"tar.gz"` (default dir);
  `package.include_index` — ship the `.argosy/` cache (default false).
- `catalog.redact_home` — rewrite home paths as `~` (default false).
- `code_tools.allow_external_paths` — the explicit opt-out of
  workspace-root path confinement (default false).

## session

- `session.project_ttl_secs` (default 1800) and `session.project_max`
  (default 8) bound project sessions.
- `session.review_ttl_secs` (default 7200) and `session.review_max`
  (default 16) bound review sessions. All values must be >= 1; expiry is
  measured from last use.

## trust

`trust { "<manifest-name>" = "<tier>" }` caps an imported book's
self-claimed trust tier; unknown tier values or keys that cannot be
manifest names are configuration errors. An import with no entry defaults
to `unverified`.

## Environment overrides

`$ARGOSY_EMBED_CACHE_DIR` (legacy: `$FASTEMBED_CACHE_DIR`) outranks the
configured cache path. `$XDG_STATE_HOME` moves all per-project state;
`$XDG_CONFIG_HOME` moves the configuration root.
