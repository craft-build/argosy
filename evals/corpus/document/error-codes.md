---
type: Reference
description: Exit codes, error kinds, URI rules, and the failure messages argosy emits.
tags: [errors, reference]
---
# Errors and exit codes

The CLI exits `0` on success, `1` on command failure, and `2` on usage
errors. MCP tools report business failures as `isError` results with
actionable messages, never as silently empty output.

## Error kinds

- `UnknownArgosy` — a query or filter named an argosy that is not active
  in the context. A configuration error, not an empty result: scoping a
  search to a name that was never pulled surfaces this immediately.
- `InvalidUri` — an `argosy://` URI failed to parse. URIs carry the
  scheme, the argosy name, and the namespace-prefixed concept id, and the
  body may only contain `A-Za-z0-9._-/`: v1 has no percent-encoding, so a
  space or other character makes the URI unrepresentable.
- `ConceptExists` — a write targeted a concept id that already exists.
  The library never overwrites silently except through the deliberate
  edit path.
- `Validation` — structural or contract failures with a human-readable
  reason (bad configuration values, malformed datasets, unmet frontmatter
  requirements).
- `Config` — the BarkML user configuration could not be read, parsed, or
  resolved (duplicate keys are collisions, never overrides).

## Index failure messages

- "no index at `<path>` — run `argosy index build`" — commands that need
  a built index say so instead of failing obscurely.
- "index was built with a different embedding model: store holds N-dim
  vectors, provider emits M; run `argosy index build` to rebuild" — the
  dimension guard: a stale store fails the search with a rebuild hint
  rather than returning quietly wrong results.

## Reviewer guards

`start_review` refuses option-shaped revisions (a leading `-`) and
resolves every `base`/`commit` through `git rev-parse --verify` before
any diff runs. The code-tool write guard answers a file with no recorded
read with "no read on record: … read it through these tools first" — it
fails closed so an agent cannot clobber a file it never looked at.
