---
type: Reference
description: The index lifecycle — reconcile diffs, staleness reports, model identity, and rebuilds.
tags: [index, lifecycle]
---
# Index lifecycle

The index is brought in line with the bundles by reconcile: it never
silently diverges, and it never holds two models' vectors at once.

## Reconcile

`argosy index build` walks every active argosy's default namespaces,
hashes each concept's would-be chunks, and diffs that against what the
store holds. New or content-changed concepts are embedded and upserted in
batches; concepts deleted from disk are removed; unchanged concepts are
left alone. A concept's hash covers all its chunk texts, so re-chunking a
document (say, after editing a heading) counts as a content change and
re-embeds it.

## Model identity and full rebuilds

The store records the identity of the model that embedded its vectors.
When the expected identity differs from the recorded one — a model swap, a
search-mode switch, or data with no recorded identity — reconcile clears
the store and re-embeds everything. Querying a stale index does not return
quietly wrong results: a dimension mismatch fails the search with a
rebuild hint telling the user to run `argosy index build`.

## Staleness

`argosy index status` prints a read-only preview: concepts on disk the
store lacks (added), concepts whose hash changed (changed), concepts the
store holds that no longer exist (removed), and whether a model mismatch
forces a full rebuild. The status command never writes.

## Where the index lives

The database is one SQLite file, `index.db`, under the project's slot in
the state directory: `<state>/projects/<slug>/index.db`. Each project gets
its own index; there is no shared cross-project index. The first build on
a machine downloads the pinned embedding model (about 90 MB) into the
cache configured by `paths.embed_cache_dir` (or `$ARGOSY_EMBED_CACHE_DIR`);
later builds load from the cache offline.
