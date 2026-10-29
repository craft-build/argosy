---
type: Architecture Overview
description: How a bundle, the project context, the index, and the MCP server fit together.
tags: [architecture]
---
# Architecture

An argosy is a directory of plain markdown — the bundle — that a coding
harness reads and writes as knowledge. The bundle is the source of truth;
everything derived from it can be rebuilt from it at any time.

## The bundle and its namespaces

A bundle holds four reserved namespaces, each a directory: `document/` for
prose knowledge, `skill/` for reusable procedures, `styleguide/` for
enforceable rules, and `memory/` for working notes. Every concept is one
markdown file with YAML frontmatter carrying at least a `type` and a
`description`. Custom namespaces (like `roadmap/`) are allowed but are not
indexed by default.

The `memory/` namespace is private by design: it is never included when a
bundle is packaged, and imported argosys contribute no memory to the default
index walk.

## The project context

A project opens one local (writable) argosy plus any number of imported
(read-only) argosys, each a git clone under the user's state directory,
outside the project tree. Imported bundles shadow nothing: every concept
carries its origin, and an `argosy://` URI always names the argosy it came
from. Work happens through a `ProjectContext`, which resolves qualified
concept ids like `argosy://argosy-docs/document/architecture` across the
local and imported bundles.

## The index is a derived artifact

Search runs over a single SQLite file, `index.db`, rebuilt from the bundle
whenever it is missing or stale. The database holds two retrieval legs in
one file: a vector table (sqlite-vec) for embedding similarity and an FTS5
table for BM25 keyword search, fused at query time. Nothing about a
bundle's meaning depends on the index existing — at worst, an absent or
stale index degrades retrieval quality, never correctness.

## The MCP server and the CLI

The `argosy mcp` command serves the knowledge, skill, code-intelligence,
and review tools over the Model Context Protocol on stdio. Every tool call
selects its project with a `cwd` parameter, so one server process can serve
many projects; project sessions open lazily and are bounded by a TTL and
an LRU cap. The `argosy` CLI wraps the same library for humans: `init`,
`validate`, `package`, `pull`, `index build|status|query|eval`, `convert`,
`catalog`, and `agent reviewer` subcommands.

## Where state lives

Per-project state lives under the user state directory
(`$XDG_STATE_HOME/argosy`), keyed by a slug of the project root:
`<state>/projects/<slug>/` holds the local `default` bundle, imported
checkouts, and the shared `index.db`. The project tree itself stays
argosy-free apart from agent definitions the user installs.
