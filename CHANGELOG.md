# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- *(index)* `argosy index eval <dataset>`: a labeled retrieval eval — a JSON
  dataset of queries mapped to expected `argosy://` URIs, scored with
  macro-averaged recall@k and MRR over the project's real search path, with
  passage hits collapsed to unique concepts by best rank. Multi-relevant
  expectations and `absent` drift-challenge labels (URIs that must stay out
  of a configurable rank window) are supported; labels that do not exist in
  the built index fail the run instead of silently scoring zero. Exits 1
  below the dataset's floors (`--min-recall`/`--min-mrr` override,
  `--report-only` is the calibration escape)
- *(eval)* a checked-in corpus and dataset under `evals/` (13 concepts,
  51 passages, 32 hand-pruned labels) plus a `retrieval-eval` CI job gating
  two legs: hybrid (embedding model cached by its pinned revision) and
  lexical (zero network). Measured at hybrid recall@5 1.000 / MRR 0.932,
  lexical 0.938 / 0.795; floors set at 0.95/0.90 and 0.90/0.75
- *(index)* hybrid retrieval: an FTS5 BM25 index over the chunked passages
  (and concept ids) is fused with vector similarity by reciprocal rank
  fusion — exact identifiers, error codes, and rule IDs now rank next to
  paraphrase matches in every `search`/`search_rules`/`ask` and
  `argosy index query`. Query text is tokenized to quoted FTS terms, so
  operator-shaped queries are never parsed as match syntax, and content
  ties in a leg stay exact ties in the fused order (QRY-7 unchanged)
- *(index)* `index.search_mode = "lexical"` in the user config: BM25-only
  retrieval with no embedding model at all — nothing is downloaded, the
  vec table is never created, and the mode records its own model identity
  so switching to or from a real model is an ordinary full rebuild
- *(index)* heading-based passage chunking: long concepts split at ATX
  headings (then paragraphs) into units embedded behind a
  `Concept:`/`Section:` breadcrumb prefix, so text past the first ~200 words
  is retrievable; short concepts stay a single passage. Configurable with
  `index.chunk_max_tokens` (default 220, capped at the embedder's 256-token
  truncation)
- *(mcp)* search hits carry the matched `passage`, its `heading_path`, and a
  1-based body line range (`start_line`/`end_line`); imported passages are
  wrapped in the untrusted-content delimiters like every imported free text
- *(mcp)* `read` accepts `start_line`/`end_line` for small-to-big expansion of
  a passage hit, reporting the sliced `lines` range

### Other

- the index schema is now v3 (adds `unit_fts`, the FTS5 BM25 index kept in
  sync by `units`-row triggers): an existing index is cleared and rebuilt on
  first open after the upgrade (it is a derived artifact), so the first
  search after upgrading re-embeds the book. Search hit `score` is now the
  reciprocal-rank-fusion score — a ranking signal, not a cosine similarity;
  consumers that only sort or compare hits within one result set are
  unaffected
- `argosy index status` JSON reports `concepts` (and per-namespace
  `concepts`) where it previously reported one unit per concept; `index
  build` prints concepts and passages separately

## [0.7.0](https://github.com/craft-build/argosy/compare/v0.6.0...v0.7.0) - 2026-10-10

### Other

- *(deps)* move to tree-sitter 0.27 and drop perl support

## [0.6.0](https://github.com/craft-build/argosy/compare/v0.5.0...v0.6.0) - 2026-10-01

### Added

- *(decision)* fit decision requests to a configurable input-token budget
  (`decision.max_input_tokens`, default 512): over-budget requests are trimmed
  before sending — longest fields first, each cut marked in place — so a large
  question, rule set, or diff hunk can no longer overflow the endpoint's
  context window

## [0.5.0](https://github.com/craft-build/argosy/compare/v0.4.0...v0.5.0) - 2026-09-29

### Added

- *(decision)* add generic Jev decision endpoint with ask, rule rerank, and finding scoring

### Other

- *(deps)* update dependencies
- *(mcp)* direct agents to the argosy tools and resources, not the state dir

## [0.4.0](https://github.com/craft-build/argosy/compare/v0.3.1...v0.4.0) - 2026-09-26

### Added

- *(catalog)* generate a global project catalog
- *(index)* add bge-small-en-v1.5 to the embedding model registry
- *(config)* add user-level BarkML configuration and model registry

### Fixed

- *(error)* box large barkml Config source to shrink Error
- *(index)* apply BGE query-side instruction via embed_query
- *(index)* refuse search when store width is unrecorded but vectors exist
- *(index)* fail search with a rebuild hint on model width mismatch

### Other

- *(deps)* update dependencies
- *(sqlite+tract)* enable multithreaded embedding and synchronous db

### Added

- *(catalog)* `argosy catalog` regenerates a global catalog of every project
  slot (`--json`, `--write` to `<state>/README.md`, `--redact-home`); the
  same document is served over MCP as `argosy://catalog`, which honors the
  `catalog.redact_home` config (redacting when the config cannot be read).
- *(catalog)* project slots record their canonical root in a `.project-root`
  sidecar (`argosy init` / project-scoped `argosy pull`), so slots whose
  project directory is gone are flagged stale and duplicate concept ids
  across a slot's active bundles are reported.

## [0.3.1](https://github.com/craft-build/argosy/compare/v0.3.0...v0.3.1) - 2026-09-24

### Fixed

- *(tract)* simplify with to_vec

### Other

- *(tract)* use default tract interface with auto-select runtime

## [0.3.0](https://github.com/craft-build/argosy/compare/v0.2.4...v0.3.0) - 2026-09-24

### Added

- *(index)* group tract inference by token length
- *(index)* show embedding progress during CLI builds

### Other

- *(deps)* update dependencies
- *(review)* remove browser server and status tool
- *(index)* replace candle with tract-onnx embedding backend
- *(deps)* update dependencies

### Changed

- *(index)* replace the candle embedding backend with pure-Rust `tract-onnx` running the ONNX export of `all-MiniLM-L6-v2`; `model_id()` is now `tract/...@tract-1`, which triggers one full index rebuild

## [0.2.4](https://github.com/craft-build/argosy/compare/v0.2.3...v0.2.4) - 2026-09-08

### Fixed

- *(review)* gate rmcp schema derives behind the mcp feature

### Other

- *(deps)* upgrade hf-hub to 1.0
- bump dependencies

### Fixed

- *(review)* gate rmcp schema derives behind the mcp feature so `code-tools` compiles without mcp
- *(cli)* gate index imports behind the default-index feature

## [0.2.3](https://github.com/craft-build/argosy/compare/v0.2.2...v0.2.3) - 2026-09-03

### Added

- *(review)* add structured MCP review workflow
- *(mcp)* add one-time browser code reviews

### Fixed

- *(mcp)* avoid review HTTP close race

### Other

- *(mcp)* make review HTTP test deterministic

## [0.2.2](https://github.com/craft-build/argosy/compare/v0.2.1...v0.2.2) - 2026-09-02

### Added

- *(mcp)* more thorough scan prompt: two-pass investigation, fact ownership, audit pass

### Other

- ignore .lab/ and run.log for research sessions
- *(index)* replace fastembed with pure-Rust candle backend

## [0.2.1](https://github.com/craft-build/argosy/compare/v0.2.0...v0.2.1) - 2026-08-31

### Added

- add MCP read tool, fix tool/conflict edge cases, add CI
- *(mcp)* add `scan` project-documentation prompt

### Fixed

- assorted small robustness cleanups
- *(local)* error on non-list promote sources; name the directory in skill delete
- refuse option-like clone urls; fall back to USERPROFILE for home
- *(zoom,inspect)* clearer ambiguity candidates; scoped, honest git status
- *(callgraph)* mark recursive edges and cap the rendered tree
- *(conflicts)* surface conflicted files that are not valid UTF-8
- *(repomap)* parse tsx and jsx files with the tsx grammar
- *(outline)* call a Rust function a method only inside impl or trait
- *(outline)* size-check before reading, track reads, cap single files
- *(outline)* markdown headings span their sections; html headings are h1-h6 only
- *(concept)* unique staging files and clean them up on failed renames
- *(convert)* reconcile the index after a styleguide import
- *(repomap)* saturate the token budget instead of overflowing
- *(astgrep)* roll back only rewrites that introduce new syntax errors
- *(astgrep)* skip stale or unwritable files per-file in apply
- reject .argosy path segments in concept ids
- *(conflicts)* handle diff3/zdiff3 base sections in resolve
- *(outline)* floor the 30KB truncation cut to a char boundary
- never read bundle concepts through symlinks

### Other

- update stale-read e2e to per-file skip contract; fmt
- *(mcp)* collapse the triplicated write and delete handlers
- restructure crate into per-module directories

## [0.2.0](https://github.com/craft-build/argosy/compare/v0.1.2...v0.2.0) - 2026-08-29

### Added

- [**breaking**] move argosy data out of project tree into XDG state dir

## [0.1.2](https://github.com/craft-build/argosy/compare/v0.1.1...v0.1.2) - 2026-08-29

### Added

- *(memory)* auto-fill `type: Memory` on writes instead of rejecting
- *(mcp)* multi-project server — tools select their project via cwd

### Fixed

- *(harness)* require cwd on every argosy MCP tool call in prompt
- *(mcp)* send SEP-2549 cache hints on list/read results

### Other

- *(cli)* trim help text to concise one-liners

## [0.1.1](https://github.com/craft-build/argosy/compare/v0.1.0...v0.1.1) - 2026-08-29

### Fixed

- *(index)* cache the fastembed model under XDG, not the CWD
- *(tests)* untrack fixture .argosy placeholder that dirtied release-plz

### Other

- ignore test argosy
