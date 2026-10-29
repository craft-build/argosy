---
type: Reference
description: Security — import integrity, provenance-capped trust tiers, untrusted-content delimiters, and bounded sessions.
tags: [security, trust]
---
# Security

Shared books are untrusted content. Argosy verifies what it can, caps what
it cannot, and labels everything that reaches the model.

## Import integrity

`argosy pull` verifies a packaged bundle's integrity sidecar after
cloning; a mismatch is refused and leaves no checkout. Every time a
context opens, each import is re-verified against the stamp recorded at
pull time — content tampered with after the pull, a deleted sidecar, or a
rewritten self-consistent one all fail closed. An import that fails is
excluded from the session: it is never served, and it is reported as
`INTEGRITY FAILED` in the catalog and under `excluded` in the argosy
listing.

## Trust tiers come from provenance

A trust tier is decided by the *importer's* configuration, never by the
book's own frontmatter. The user config's `trust { "<manifest-name>" =
"<tier>" }` section caps each import's self-claimed `verified` tier;
anything uncapped defaults to `unverified`. Local concepts relay their
claims verbatim. Skills and hits surface the effective tier so the caller
can weigh them.

## Untrusted-content delimiters

Imported free text — read bodies, skill content, resource bodies, and
rule excerpts — arrives wrapped in paired
`<untrusted-content argosy="…" uri="…" trust="…">` delimiters. Local
content is never wrapped. Every search hit and read result also carries
`kind` (`local` or `imported`), the effective `trust` tier, and, for
imports with git metadata, the checkout's `source_commit`.

## Invisible-Unicode stripping

At index time, invisible format characters — zero-width spaces and
joiners, bidi marks and overrides, the BOM, interlinear annotations, tag
characters — are stripped from the embedded text only. On-disk bytes stay
verbatim, so the integrity sidecar still verifies and planted payloads
remain inspectable by `read`. Each strip is reported as a warning by
`index build`.

## Bounded sessions and confined tools

Project and review sessions are bounded by TTL and LRU caps
(`session.project_ttl_secs` 1800 and `session.project_max` 8 for
projects; `session.review_ttl_secs` 7200 and `session.review_max` 16 for
reviews). Expiry is measured from last use, and expiry is also the refresh
path: the next call reopens and re-reconciles, so a book pulled after the
session opened becomes visible without a restart. Code-tool paths are
confined to the workspace root by default (`code_tools.allow_external_paths`
is the explicit opt-out), and the write guard fails closed on files with
no recorded read.
