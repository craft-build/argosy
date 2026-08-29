# Argosy Code Review Findings

- **Date:** 2026-08-29
- **Reviewed at:** `main` @ `5f23fe2` (working tree clean except untracked `.env`)
- **Scope:** full codebase — `src/` (including `codetools/`, `index/`), `tests/mcp.rs`, `tests/cli.rs` (partial), `Cargo.toml`, `.gitignore`, project styleguide rules
- **Method:** source reading only; the test suite was not compiled or run
- **Verdict:** request changes — 0 × P0, 2 × P1, 4 × P2, 5 × P3

## Overall assessment

This is an unusually well-maintained codebase. The security-critical surfaces — path construction from tool input (`ConceptId` parsing rejects `..`/`\`/`:`, `resolve_path` re-verifies containment, symlinks refused at every reader and walker, packaging canonicalizes and refuses escapes, sidecar paths validated as pure-normal components, pull checkout names charset-checked), SQL (fully bound parameters; the only interpolation is the vec0 table width, documented as internal) — are layered, each layer tested, including adversarial cases (symlink escapes, attacker-controlled sidecars, empty filter lists). Error handling is consistently `Result`-based with messages written for LLM callers; panics are confined to documented invariants (`expect`s on just-inserted map entries, serialization of plain structs). Index-vs-filesystem reconciliation is hash-diffed, transactional in SQLite, and its read-only preview is tested to mirror reconcile exactly. Test coverage — unit, wire-level MCP over an in-process duplex, CLI binary tests — is exceptional. The findings below are real but narrow: two High items (a `.gitignore` gap and a silent-truncation path in `conflicts resolve`), and a handful of Medium robustness/concurrency issues concentrated in the recently ported code tools and the MCP dispatch layer.

## P1 — High

### [P1] `.env` is not covered by `.gitignore` — secrets-exposure risk

- **Location:** `.gitignore` (entire file; no `.env` entry)
- **What:** The repo root contains an untracked `.env` file (contents deliberately not inspected). `.gitignore` lists `target/`, `.fastembed_cache`, `.argosy/`, mutants output — but no `.env` pattern.
- **Why it matters:** `.env` files conventionally hold secrets. One `git add .` (or a GUI "stage all") commits it; this is the single most common accidental-secrets mechanism. It will also keep showing as untracked noise.
- **Fix:** add `.env` (and ideally `.env.*` with `!.env.example`) to `.gitignore`.
- **Confidence:** 1.0

### [P1] `conflicts resolve` silently truncates the file when a conflict block is unterminated

- **Location:** `src/codetools/conflicts.rs:160-226` (`resolve_content`), write gate at `:243`, write at `:256`
- **What:** `resolve_content` is a state machine. On a `<<<<<<< ` opener it enters state 1 and accumulates lines into `ours` (then `theirs`); only a `>>>>>>> ` line flushes. If the block is never terminated, EOF is reached in state 1/2 and everything from the stray opener to end-of-file is discarded from `out` — and `parse_conflicts` (which only counts *complete* markers) neither counts it in `remaining` nor reports it. The file is written whenever the same file holds at least one *complete* conflict (`resolved > 0`).
- **Why it matters:** Silent data loss on a write path reachable from MCP tool input. Concrete trigger: during a messy merge a file contains one complete conflict plus a second, truncated block (a `<<<<<<< HEAD` line with no matching end — left by a manual edit, a `git rerere` interaction, or a doc/comment that quotes a conflict opener at line start). `conflicts` with `resolve: "@ours"` resolves the first conflict and silently deletes everything from the stray marker to EOF; the summary text reports only the resolved count, so the LLM operator has no signal content was dropped.
- **Fix:** at EOF, if `state != 0`, abort that file (skip the write) with an error/warning naming the unterminated marker — or at minimum flush the accumulated `ours`/`theirs` buffers verbatim and count the block as unresolved. Add a regression test with one complete + one unterminated block.
- **Confidence:** 0.9

## P2 — Medium

### [P2] Concurrent code-tool writes have a stale-read TOCTOU window (lost update)

- **Location:** `src/mcp.rs:1495-1516` (code tools dispatched without the state lock, each on `spawn_blocking`); `src/codetools/astgrep.rs:101-103`
- **What:** Argosy tool calls serialize on a global `tokio::sync::Mutex` ("the only sane order for mutating tools over a single WAL database"), but code tools deliberately run outside it. Two concurrent `astgrep apply` (or `conflicts resolve`) calls targeting the same file both pass `check_before_edit` (mtime unchanged — neither has written yet), both read, both write; last write wins, first write's replacements are lost. The rmcp client can legitimately issue parallel tool calls.
- **Why it matters:** The stale-read guard is the only protection against clobbering; it is check-then-act over shared files with no serialization. The failure is silent — both calls report success.
- **Fix:** add a dedicated write lock for code-tool mutating calls (e.g. route `astgrep apply` and `conflicts resolve` through a second shared `Mutex`, or reuse the same state lock's discipline), or document the single-flight assumption.
- **Confidence:** 0.8

### [P2] Manifest `name` charset is validated only at `init`, not at `open` — URIs that cannot round-trip

- **Location:** `src/bundle.rs:133-143` (`Manifest::parse` checks only non-empty); charset check at `src/local.rs:212-219` (`init` only) and `src/bundle.rs:214-221`; URI strictness at `src/context.rs:53-115`
- **What:** `QualifiedConceptId::from_uri` rejects anything outside `[A-Za-z0-9._-/]`, and `to_uri` (`context.rs:44-46`, documented as best-effort) emits whatever the manifest name says. A hand-authored imported bundle with `name: my bundle` (or any non-URI-charset name) opens fine, is listed in `argosy://_argosys`, and its search hits carry `uri: "argosy://my bundle/document/x"` — which `read_resource`/`read_uri` then rejects with `InvalidUri`.
- **Why it matters:** The server produces identifiers its own resolver refuses. `duplicate-name` and `UnknownArgosy` machinery all assume names are URI-safe, so this is an unenforced invariant.
- **Fix:** validate `is_safe_bundle_name(name)` inside `Manifest::parse` (or `Argosy::open`) so bad names fail at open with an actionable message, matching how bad `argosy_version` already hard-fails there.
- **Confidence:** 0.85

### [P2] Argosy tool handlers do blocking I/O directly on tokio runtime workers

- **Location:** `src/mcp.rs:1519-1536` (`dispatch!` runs synchronously inside the async `call_tool` body while holding the state lock); contrast the code-tool path at `src/mcp.rs:1341-1362`, which explicitly uses `spawn_blocking`; worst offender is the first-embed download via `LazyFastembedProvider::embed` (`src/index/fastembed.rs:173-193`) reached through `reconcile` in the session factory (`src/cli.rs:765-791`)
- **What:** The code-tool dispatch comment correctly notes sync handlers "walk directories / parse grammars, so they run on the blocking pool" — but argosy handlers do equally blocking work (reconcile re-reads every concept file; embed runs ONNX inference, potentially downloading ~90 MB on first use) inline on an async worker.
- **Why it matters:** A cold-cache `search` blocks a runtime worker (and, via the global mutex, every other argosy tool call) for the duration of a network download — potentially minutes. With a multi-thread runtime this degrades rather than deadlocks, but it violates the async contract and can stall the stdio service loop when several projects are opened concurrently (one blocking open per worker).
- **Fix:** run the argosy handlers through `spawn_blocking` too (the state `Mutex` guard must be acquired inside the closure), mirroring `dispatch_code`.
- **Confidence:** 0.8

### [P2] `FileReadTracker` uses `lock().unwrap()` — panics on mutex poisoning (violates `NO-UNWRAP-IN-PROD`)

- **Location:** `src/codetools/file_tracker.rs:41, 52, 70`; contrast `src/codetools/mod.rs:87` (`repo_maps` recovers via `unwrap_or_else(|e| e.into_inner())`)
- **Grounded against:** `.argosy/default/styleguide/rust/practices/NO-UNWRAP-IN-PROD.md` (priority: error)
- **What:** If the tracker mutex is ever poisoned (a panic between lock and unlock in a concurrent `spawn_blocking` code-tool call), every subsequent `record_read`/`check_before_edit` panics. The panic is contained — `dispatch_code!` converts the `JoinError` into a tool error (`mcp.rs:1351-1357`) — but from then on *every* code tool that touches the tracker fails for the rest of the process lifetime, and the same file uses the poisoning-recovery idiom inconsistently.
- **Why it matters:** The project's own ruleset forbids bare `unwrap()` in production; the inconsistent sibling proves the codebase already knows the fix.
- **Fix:** use `.lock().unwrap_or_else(|e| e.into_inner())` at all three sites, matching `CodeTools::repomap_for_root`.
- **Confidence:** 0.9 (mechanism certain; practical trigger rare)

## P3 — Low

### [P3] `WriteReport.bytes` reports input length, not bytes on disk

- **Location:** `src/mcp.rs:395-401` (`write_memory` passes `params.content.len()`), `:503-512`; auto-fill at `src/local.rs:41-51`
- **What:** When `write_memory` auto-fills `type: Memory`, the file on disk is larger than the submitted `content`, but the report's `bytes` field (documented "Bytes written") echoes the input length.
- **Fix:** report the length of the serialized concept actually written (`concept.to_string().len()` from the post-`with_memory_type` value).
- **Confidence:** 1.0

### [P3] `argosy index query` opens the store read-write, inconsistent with `status`

- **Location:** `src/cli.rs:689-697` — `IndexVerb::Build | IndexVerb::Query` share `SqliteVecStore::open` (which creates dirs, sets WAL, runs DDL), while `Status` deliberately uses `open_read_only` (documented at `src/index/sqlite.rs:145-165`)
- **What:** A read-only query verb takes the writable path.
- **Why it matters:** `index query` fails on a permission-locked or read-only-mounted index where `index status` succeeds, and can create `.argosy/` directories as a side effect of a query.
- **Fix:** use `open_read_only` for `Query` (embedding the query text needs no store writes — `search` takes `&self`).
- **Confidence:** 0.9

### [P3] `search` treats unknown namespace names as silently-empty, while unknown `argosy` names error

- **Location:** `src/mcp.rs:274-284` + `:846-851` (schema doc admits it) vs `src/index/mod.rs:504-511` (argosy names validated, error `UnknownArgosy`)
- **What:** `namespaces: ["documnet"]` (typo) yields `hits: []` with no signal; `argosy: "typo"` errors.
- **Why it matters:** For an LLM caller, an empty result and a typo'd filter are indistinguishable; the asymmetry with `argosy` makes it worse. The doc comment flags it, but the doc is only visible in the schema.
- **Fix:** validate namespace strings against the argosys' present namespaces (or at least the four reserved names) and error on unknown spellings, as `search` already does for argosy names.
- **Confidence:** 0.7 (possible deliberate tradeoff, but the asymmetry is trap-prone)

### [P3] MCP session cache never evicts

- **Location:** `src/mcp.rs:80-83`, `:664-674`
- **What:** Each distinct canonicalized `cwd` opens a `ProjectSession` (holding an open SQLite connection and a fully-walked context) cached for process lifetime.
- **Why it matters:** A long-lived server serving many project roots grows monotonically (fds + memory). Bounded in practice — failed opens aren't cached, so growth needs many real argosy projects — but there is no bound at all.
- **Fix:** cap the cache (LRU) or drop sessions on error/idle; at minimum document the unbounded-growth contract.
- **Confidence:** 0.6

### [P3] `conflicts` index-mode rewrite mangles preserved markers

- **Location:** `src/codetools/conflicts.rs:206-215`
- **What:** When `index` resolves only the Nth conflict, the others are re-emitted with hardcoded `<<<<<<<  ours` / `>>>>>>>  theirs` labels (note the double space: `CONFLICT_START` already ends with one), discarding the real branch names (`HEAD`, `feature`, ...).
- **Why it matters:** A subsequent `conflicts` listing reports wrong branch names for the preserved blocks — misleading exactly when the user is mid-resolution. Cosmetic relative to the P1 above, same function.
- **Fix:** capture the original opener/end lines and re-emit them verbatim.
- **Confidence:** 0.95

## Areas verified clean

Path traversal (concept ids, URIs, custom namespaces, import paths, checkout names, packaging, sidecar), symlink escapes at every reader/walker, SQL parameterization, frontmatter depth/bomb guards (`MAX_FRONTMATTER_DEPTH` with tests both directions), UTF-8 boundary handling (`floor_char_boundary` everywhere truncation happens), the index reconcile/staleness mirror, WAL checkpoint-before-package, SEP-2549 cache hints (wire-tested in `tests/mcp.rs:659-701`), and the multi-project cwd cache contract (open-once, failed-opens-not-cached, isolation tested at `tests/mcp.rs:709-826`). `zoom`'s `count() - 1` sites are all guarded by earlier emptiness checks; `repomap`'s binary search cannot underflow.

## Not covered / risks

- `src/codetools/outline.rs` (~2.2k lines of tree-sitter extraction) and `src/codetools/repomap/{tags,graph}.rs` were skimmed, with a targeted grep for slicing/underflow patterns rather than a full read — a deep audit of the query files was not done.
- `tests/cli.rs` was read only in part (first ~150 lines of a long file); the MCP wire tests were read fully.
- The suite was not compiled or run; the `#[ignore]`d fastembed network tests were not run.
- The untracked `.env` file's contents were intentionally not read.
