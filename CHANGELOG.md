# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
