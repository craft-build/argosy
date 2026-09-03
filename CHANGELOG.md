# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
