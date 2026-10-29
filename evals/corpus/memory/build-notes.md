---
type: Note
description: Build-time gotchas — the v3 schema rebuild and the model-download pause.
tags: [build]
---
# Build Notes

The index schema move to v3 (adding the FTS5 table) forced a one-time
full rebuild on first open after the upgrade — expected, not a bug: the
index is a derived artifact. Model identity changes do the same.

Benchmarks on Apple silicon: grouped inference batches of 8 cut a
663-concept cold build from ~22 s to ~13 s and peak RSS from ~960 MB to
~500 MB. Keep FP32 weights; the quantized graph measured slower on CPU.
