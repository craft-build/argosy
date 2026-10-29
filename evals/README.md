# Retrieval eval

A labeled, corpus-based search-quality benchmark (roadmap item 8 in
`docs/next.md`): queries mapped to expected `argosy://` URIs, scored with
recall@k and MRR. Every later retrieval change — chunking budgets, fusion
constants, embedder swaps, a reranker — becomes measurable against it.

## Layout

- `corpus/` — the eval book, an argosy named `argosy-docs` whose documents
  describe the argosy system itself: real multi-section prose (exercises
  heading chunking), identifier-heavy references (exercises the BM25 leg),
  styleguide rules, a skill, and a memory note. It is checked in, so label
  drift is visible in review.
- `retrieval.json` — the labels: `k` cutoffs, the `absent_window` for
  drift-challenge queries, aggregate floors, and the labeled queries.

## Dataset format

```json
{
  "k": [1, 5, 10],
  "absent_window": 5,
  "min": {"recall@5": 0.95, "mrr": 0.9},
  "queries": [
    {"query": "what is reciprocal rank fusion",
     "expected": ["argosy://argosy-docs/document/retrieval"]},
    {"query": "how do I share a book with my team",
     "expected": ["argosy://argosy-docs/document/sharing"],
     "absent": ["argosy://argosy-docs/document/error-codes"]}
  ]
}
```

- `expected` — URIs that must rank. A query may expect several (multi-relevant
  labels); recall@k counts each one, MRR uses the first of them to rank.
- `absent` — URIs that must **not** surface within `absent_window` ranks: the
  stale/deleted-content challenge entries, pointed at confusable corpus
  content a drifted or poisoned index would surface.
- Label URIs have no `.md` extension (`…/document/retrieval`, not
  `retrieval.md`) and must exist in the built index — a typo'd label fails
  the run instead of silently scoring zero.
- The `min` floors are the CI gate for the default (hybrid) mode; the CI
  lexical leg passes its own, lower floors via `--min-recall`/`--min-mrr`.

## Running

```sh
# from a project whose default bundle is the corpus (see CI for the scaffold)
argosy index build
argosy index eval ../evals/retrieval.json          # human report, exits 1 below floors
argosy index eval ../evals/retrieval.json --json   # machine-readable
argosy index eval ../evals/retrieval.json --report-only   # calibration: always exits 0
```

The eval measures whatever the configuration builds: `index.search_mode`
selects hybrid or lexical, `index.chunk_max_tokens` sets the chunk budget.
Compare configurations by re-running the eval under each config.

## Maintaining the labels

Bootstrapped LLM-style then pruned by hand; keep them pruned:

1. When the corpus changes, re-run with `--report-only` and inspect the
   per-query ranks in the JSON output.
2. A query whose answer genuinely moved to another document gets its label
   moved; a query whose answer vanished gets deleted, not loosened.
3. Floors sit slightly below the measured values (inference is deterministic
   at the pinned model revision). Raising a floor is a reviewed change that
   says "quality improved"; lowering one needs a better excuse than noise.
4. Query mix to preserve: exact identifiers, paraphrases, multi-relevant
   expectations, passage-depth answers (past the first ~200 words), and the
   `absent` challenges.
