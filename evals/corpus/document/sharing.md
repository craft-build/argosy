---
type: How-to
description: Sharing argosys — pull imports, the global catalog, and packaging with integrity sidecars.
tags: [sharing, packaging]
---
# Sharing

Books move between people as git repositories or packaged archives, and
the project tree never carries them.

## Pulling an import

`argosy pull <url> <name>` clones an external argosy into the project's
argosy store under the state directory (`--global` installs into the
user-wide store instead). Option-shaped URLs are refused, and pull never
overwrites an existing checkout — delete and re-pull to replace one. Every
accepted clone that ships an integrity sidecar is verified before it can
serve, and the verification is stamped beside the checkout in
importer-controlled state.

Imports are read-only by construction: writes go to the local bundle
only. Imported `memory/` never joins the default index walk, so a team
book shares documents, skills, and rules — not your private notes.

## The catalog

With many repositories and worktrees the state directory is hard to
reason about. `argosy catalog` regenerates a read-only markdown index of
every project slot: each project's canonical root, its local bundle and
imports, per-namespace counts, and index status. A slot whose recorded
project directory has disappeared is flagged stale. `--write`
materializes it as `<state>/README.md`; `--redact-home` rewrites home
paths as `~`. Coding agents can read the same document through MCP at
`argosy://catalog`.

## Packaging

`argosy package <src> <dest>` produces a distributable copy — a plain
directory tree or a `--format tar.gz` archive — validated first, with an
integrity sidecar recipients verify on pull. The `memory/` namespace is
**never** included: packaging shares the curated book, never private
notes. The `.argosy/` index cache can ride along (`--include-index`) but
recipients always rebuild on first use if it is absent or stale, because
the index is a derived artifact.
