# Tract embedding sweep — Apple M5 Max

Tested with Rust 1.97.1, tract-onnx 0.23.8, the pinned
`sentence-transformers/all-MiniLM-L6-v2` revision
`1110a243fdf4706b3f48f1d95db1a4f5529b4d41`, and a cached model.
Runs were single-process and sequential; the benchmark uses five warm
repetitions on 40 concepts. See [README.md](README.md) to reproduce them.
These figures are measurements on **one machine**, not portable defaults
for every processor or corpus.

## Results

| Corpus / configuration | Warm embed time per 40 concepts | Notes |
| --- | ---: | --- |
| Mixed lengths, original 32 | 1,235 ms | Tokenization ~2 ms, tract inference ~1,225 ms |
| Mixed lengths, ungrouped 8 | 1,167 ms | Smaller batch without length grouping |
| Mixed lengths, grouped 16 | 800 ms | Includes ~3 ms grouping cost |
| Mixed lengths, grouped 8 | **671 ms** | Includes ~3 ms grouping cost |
| Short-only, original 32 | **74 ms** | Grouped 8 takes ~81 ms |
| Long-only, original 32 | 1,327 ms | Grouped 8 takes ~1,259 ms |
| Repository documentation, original 32 | 1,255 ms | 40 passages sampled from project docs |
| Repository documentation, grouped 8 | **957 ms** | Includes ~3 ms grouping cost |

Model load takes about 65–88 ms from the warm local cache in the
microbenchmark. Grouped inference sorts by unpadded tokenizer length
*within each 32-concept index batch*, then restores input order after
eight-text inference runs. It retains the same weights, 256-token
truncation, pooling, index model identity, and vector-to-concept mapping.
The benchmark's minimum vector cosine against original batching was
at least 0.9999989; minor floating-point differences are possible.

The fixed 40-concept mixed-length **CLI cold index build** measured
1.39 s and 604 MB peak RSS before grouping, and 0.78 s with 348 MB
peak RSS after grouping; three final runs took 0.78 s each and peaked
at 348–351 MB. A full CLI build after replacing its concepts with the
40 repository-document passages took 1.02 s and peaked at 342 MB.
The CLI figures include model load, hashing, SQLite writes, and process
startup. Unlike the CLI measurement, microbenchmark peak RSS includes
the original-batch reference run, so it is **not** an isolated measure
of the alternative configuration's memory.

## Model-change probes (not adopted)

- At the same pinned Hugging Face revision, `onnx/model_qint8_arm64.onnx`
  (SHA-256 `4278337fd0ff3c68bfb6291042cad8ab363e1d9fbc43dcb499fe91c871902474`)
  loaded and ran in tract. On 40 natural passages it took **1,865 ms**
  with grouped batches, versus ~957 ms for FP32, and had minimum
  same-text vector cosine **0.976** and self-query top-5 overlap **90.5%**
  against FP32. It is slower and changes rankings; keep FP32.
- Reducing the FP32 tokenizer limit from 256 to 128 tokens cut that
  corpus's grouped inference from ~957 ms to **471 ms**, but minimum
  vector cosine was **0.759** and top-5 overlap only **65%**. Keep 256.

The top-5 overlap test compares model outputs on the same documents as
queries, excluding the query document itself. It is a drift detector,
**not** a relevance-labeled search-quality evaluation. Neither model
change should be adopted from these results alone. Different ONNX
weights or truncation would require a new model identity and index
rebuild.

**Chosen setting:** keep index reconciliation batches of 32, use
length-grouped inference batches of 8 inside the tract provider, and
retain the existing FP32 model and 256-token limit. On short-only
corpora, 32 is slightly faster; the production path favors the
substantial mixed/long-text savings and lower peak memory instead of
adding another corpus-specific tuning rule.

## Larger imported argosy: stylish

As a separate validation,
`argosy pull https://github.com/jmt-lab/stylish.git stylish`
installed the public `stylish` checkout at commit
`0205c988aa111a418641b9afa444577e2a50843a` into isolated
benchmark state. It contributed **623 styleguide rules**; the project
also had **40 local fixture documents**, for **663 indexed concepts**
in each cold build. The network pull, model download, and compilation
were excluded from index-build timings.

| Release binary | First cold build | Second cold build | Peak RSS, respectively |
| --- | ---: | ---: | ---: |
| Original ungrouped 32 (`f589e30`) | 24.94 s | 21.87 s | 960 / 827 MB |
| Current length-grouped 8 (`eb5babd`) | **12.48 s** | **13.85 s** | 499 / 534 MB |

The four runs alternated original/grouped/grouped/original. Before
each one, only `index.db` (and its WAL/SHM companions) was removed,
leaving both argosy checkouts, the 40 local documents, and the model
cache unchanged. Both binaries reported 663 upserts; SQLite recorded
623 units from `stylish` and 40 from the local bundle. After an
original-binary build, a grouped-binary rerun reported **0 upserts,
663 unchanged** in 0.10 s, confirming the existing index remains
reusable. The original binary was built from a `git archive` of
`f589e30` with a separate Cargo target directory; the two binary
SHA-256 hashes differed.

On this larger real rule set, grouping used approximately **44% less
wall time by the two-run average**, with peak resident memory roughly **42%
lower**. There are only two timed runs per variant, so treat these
as a strong directional comparison rather than a cross-machine
performance guarantee.
