---
type: FAQ
description: Short answers — model download, moving bundles, private memory, per-project indexes, and searching without a model.
tags: [faq]
---
# FAQ

Short answers to recurring questions. Longer treatments live in the
reference documents.

## Why is the first index build slow?

The first run downloads the pinned embedding model — about 90 MB — into
the cache directory. Subsequent builds and queries load from the cache
offline. Set `index.search_mode = "lexical"` if the machine should never
download a model at all.

## Can I move or copy a bundle?

Yes. A bundle is a directory of markdown; the index is a derived artifact
rebuilt from it, so moving or copying the tree and running
`argosy index build` restores everything. Nothing meaningful depends on
the index existing.

## Is my memory shared with the team?

No. The `memory/` namespace is excluded from packaging and from imported
books' index walk. Only the local project's own memory notes are indexed,
for your sessions alone.

## Do two projects share one index?

No. Each project root gets its own slot under the state directory and its
own `index.db` there. Two worktrees of one repository are two projects.

## Why did everything re-embed after an upgrade?

The index records the identity of the model that built it. A model swap,
a search-mode switch, or a schema migration is an identity mismatch, and
mismatched stores are cleared and fully re-embedded — a store never holds
two models' vectors at once.

## Can I search before the model is cached?

Only in lexical mode. Hybrid mode needs the vector leg's model; the first
hybrid build announces the download on stderr so the pause never reads as
a hang.
