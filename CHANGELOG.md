# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
