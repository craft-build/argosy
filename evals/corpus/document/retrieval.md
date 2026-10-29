---
type: Reference
description: How retrieval works — passage chunking, the vector and BM25 legs, reciprocal-rank fusion, and the zero-model lexical mode.
tags: [retrieval, search]
---
# Retrieval

Search is hybrid: a lexical BM25 leg and a vector leg run side by side
over the same chunked passages, and their rankings are fused. A query for
an exact identifier like a rule id hits the lexical leg; a paraphrase like
"how do I keep docs honest" hits the vector leg; most real queries benefit
from both.

## Passage chunking

A short concept is embedded as one unit. A longer concept splits at ATX
headings (fenced code blocks are opaque to the splitter), and a section
that still exceeds the budget splits again at paragraph, then line,
boundaries. The per-passage token budget is `index.chunk_max_tokens`
(default 220, capped at the embedder's 256-token truncation limit), so
text past the first ~200 words of a long document stays retrievable.

Every passage is embedded behind a deterministic prefix — the concept
title plus its heading breadcrumb (`Concept:` / `Section:` lines) — the
cheap, no-LLM form of contextual retrieval. Chunks record their ordinal,
breadcrumb, and exact 1-based body line range, and search hits return the
matched passage with that range, so a caller can read just the right
section instead of the whole concept.

## The vector leg

The default embedder is all-MiniLM-L6-v2 (384 dimensions) run locally
through tract on an ONNX graph; the model identity is pinned and recorded
in the index. Query-side text goes through `embed_query`, which asymmetric
models need. The vector leg is a brute-force k-NN over every passage.

## The lexical leg

An FTS5 BM25 index (the `unit_fts` table) covers the passage text and the
concept id, kept in sync by triggers on the units table. Query text is
tokenized into quoted FTS terms joined by `OR`, so operator-shaped queries
can never be parsed as match syntax. A query with no indexable term falls
back to the vector leg alone.

## Fusion by reciprocal rank

The two legs are fused with reciprocal rank fusion (RRF): each unit earns
`1 / (60 + rank)` per leg, summed. The rank smoothing constant is 60 —
the standard default, no per-corpus tuning. Ties inside a leg stay ties in
the fused order, and no argosy's precedence can reorder results: ranking
follows retrieval relevance only. Passages of one concept are fused as
separate units, so agreement on the *same* passage is what boosts a hit.

Filters (namespace, argosy, type, tags, language, category) constrain both
legs identically *before* truncation, so a filtered query still ranks the
full corpus rather than a pre-truncated slice.

## The lexical-only mode

Setting `index.search_mode = "lexical"` in the user config builds and
queries a BM25-only index through a zero-dimension embedding provider:
nothing is ever downloaded, the vector table is never created, and the
mode records its own model identity (`lexical-only@1`), so switching to or
from a real model is an ordinary identity-mismatch full rebuild. Exact
identifiers and rule ids match strongly in this mode; paraphrases do not.
