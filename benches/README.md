# CPU embedding benchmark

The ignored `index::tract::tests::embedding_batch_benchmark` test in
[`src/index/tract.rs`](../src/index/tract.rs) measures model load, grouping,
tokenization, input preparation, tract inference, and pooling separately.
It checks vector ordering against the current ungrouped 32-text reference.
The model is downloaded on first use; subsequent runs reuse the pinned cache.

On macOS, from the repository root:

```sh
python3 benches/embedding_fixture.py target/embedding-bench/mixed --kind mixed
ARGOSY_BENCH_DOCS=target/embedding-bench/mixed \
  ARGOSY_BENCH_BATCH=8 ARGOSY_BENCH_GROUP=1 ARGOSY_BENCH_REPS=5 \
  cargo test --release --lib index::tract::tests::embedding_batch_benchmark \
    -- --ignored --nocapture
```

Sweep `ARGOSY_BENCH_BATCH=8,16,32` and `ARGOSY_BENCH_GROUP=0,1`. Measure peak
resident memory by running the already built test executable under
`/usr/bin/time -l`; do not include Cargo compilation in the timing. Use the
`short` and `long` fixture kinds as well as `mixed`, and compare on real
concept texts before generalizing these results. `--kind repository` samples
40 longer passages from the repository's README, changelog, and docs.
`embed_ms` is the per-repeat
wall time, while `group_ms` is additional and includes the extra unpadded
tokenization. The `load_ms` measure is independent; the CLI also does parsing
and SQLite work. The benchmark itself first executes the reference and the
current provider path to validate embeddings, so process peak RSS includes
those allocations as well as the measured alternative.

For alternate pinned ONNX graphs, download them into a separate local cache
and run `index::tract::tests::candidate_onnx_smoke_test` with
`ARGOSY_BENCH_ONNX=<path>` and `ARGOSY_BENCH_DOCS=<fixture directory>`. It
reports the lowest vector cosine and self-query top-5 overlap against FP32;
this is a smoke test, not a validated retrieval-quality evaluation. Failure
to load or run is not a performance result. Alternatively, set
`ARGOSY_BENCH_TRUNCATE=128` instead of `ARGOSY_BENCH_ONNX` to compare a
shortened tokenizer limit against the same FP32 graph. Do not deploy a
different graph or lower the 256-token truncation
limit on throughput alone: compare retrieval rankings on representative
queries and change the model identity before rebuilding the index.
