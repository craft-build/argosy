# 09 — The `argosy` CLI: validate, package, index, convert styleguide

| | |
|---|---|
| Depends on | 08 |
| Creates | `src/main.rs`, `src/cli.rs`, `tests/cli.rs`; extends `Cargo.toml` |
| Spec sections | §11 (lifecycle), reference doc §6 (CLI surface decision); every subcommand is a thin wrapper over already-built library APIs |

---

## 1. Context

The binary exists so the library's capabilities are scriptable and so `argosy mcp` (doc 10) has a home. The reference doc (§6) locks the surface: `mcp`, `validate`, `package`, `index`, `convert styleguide` from day one. Every subcommand here is argument parsing + library call + output formatting — if any subcommand grows real logic, that logic belongs in the library and this document has failed.

Design pressure to respect: output must serve two audiences — humans in a terminal (default, one finding/line as `argosy validate` prints doc 02's `Display` rendering) and tooling (`--json` on every subcommand, serialized with `serde_json`, schema = the library's report structs).

## 2. Requirements

### 2.1 Skeleton

- `src/main.rs`: `fn main() -> ExitCode` calling `cli::run()`. Keep it that thin.
- `src/cli.rs`: `clap` v4 **derive** API. New dependencies: `clap` (derive), `serde_json` (serialize existing report structs — derive `Serialize` on `ValidationReport`/`Finding`/`PackageReport`/`ImportReport`/`IndexReport` etc. in the library where missing).
- Global flags: `--json` (machine output), `-q/--quiet` (suppress non-error human output). Exit codes: `0` success, `1` command-level failure (validation errors, packaging failure, etc.), `2` usage error (clap handles).
- Errors: library `Error` chain printed via `{:#}`-style context on stderr; no panics, no `unwrap` in command paths.

### 2.2 Subcommands

**`argosy validate <path> [--namespace document|skill|memory|styleguide]`**
- Runs `Argosy::validate` (or the per-namespace validators from doc 03 when `--namespace` is given) and prints the report.
- Exit `1` if `!report.is_conformant()`; print findings grouped by severity otherwise "OK: <name> <argosy_version>" on success.
- This is lifecycle step 2 made scriptable; it must work on directories that aren't openable argosys (broken fixtures) — that's exactly its job.

**`argosy package <source> <dest> [--format dir|tar.gz] [--include-index]`**
- Wraps doc 08 `package()`; prints the report (`files_copied`, `DIST-4` warnings — print warnings even under `--quiet`, they're the safeguard), names the package `name@argosy_version`.

**`argosy index <path> <verb>`** with `<path>` = project root (the context's local argosy root):

| Verb | Behavior |
|---|---|
| `build` | Open `ProjectContext` (local only, or `--import <path>` repeated), construct default backend (`SqliteVecStore` at `.argosy/index.db` + `FastembedProvider::new_default()`), run `reconcile`, print `IndexReport` (`rebuilt`, counts, model id). First run may download the ONNX model — say so in `--help` (doc 07 §2.3). |
| `status` | Read-only: report store `model_id`, unit counts per argosy/namespace, and a **staleness preview** — the diff reconcile *would* apply, computed without writing (embed only when content hashes differ? No: preview compares hashes only, zero embed calls). |
| `query "<text>" [-k N] [--namespace NS] [--argosy NAME] [--language L] [--category C] [--tag T] [--type T]` | Reconcile if stale (same path as `build`), then `Index::search` with a `Filter` built from flags; print one hit per line: `score  argosy://<name>/<ns>/<id>  —  description`. Flags map 1:1 onto doc 06's `Filter`; unknown argosy names must error (doc 06 already enforces). |

**`argosy convert styleguide <yaml-dir> <argosy-path>`**
- Opens `<argosy-path>` as `LocalArgosy`, runs `import_styleguide_yaml`, prints written/skipped counts plus each `Finding`; exit `1` if any findings.

### 2.3 Integration testing (`tests/cli.rs`)

- Dev-dependencies: `assert_cmd`, `predicates` (the conventional pair; adds test-only weight only).
- Drive the compiled binary, not library calls — argument parsing, exit codes, stdout/stderr split, and JSON shape are the contract under test.
- Tests run offline: `index build`/`query`-level coverage uses... the real backend needs the ONNX model → gate CLI index tests exactly like doc 07 (`#[ignore]`, run once locally), and cover flag→`Filter` mapping by `--json`-output unit tests in `cli.rs` itself (parse args, build the `Filter`, assert fields).

## 3. Non-Goals

- No `mcp` subcommand implementation here — doc 10 adds it to the same clap tree (declare the variant now, return "not yet built" if invoked... no —declare it in doc 10 only; keep this chunk's tree exactly four subcommands so tests are meaningful).
- No config files, no shell completions, no man pages.
- No interactive prompts anywhere (a CLI consumed by agents must stay scriptable; `SEC-4`-style confirmations are the harness's layer, not this binary's).

## 4. Success Criteria

- [ ] `tests/cli.rs` passes for: `validate` on the valid fixture (exit 0, "OK" line includes name+version); `validate` on each broken fixture (exit 1, finding lines include requirement IDs); `validate --json` parses as the serialized `ValidationReport`; `validate --namespace skill` runs only skill checks.
- [ ] `package` to a dir and to `tar.gz` (exit 0, warning line when `memory/` present, `--include-index` behavior asserted via dest contents); package of a broken argosy fails with the validation errors shown.
- [ ] `convert styleguide` against the doc 08 YAML fixture: exit 0 with counts; rerun → all skipped, exit 0; malformed rule file → exit 1 with findings on stderr/stdout as appropriate.
- [ ] `index status`/`build`/`query` tests exist behind `#[ignore]` and pass locally with network (model download); all argument-parsing/Filter-mapping paths covered by offline tests.
- [ ] `--json` output of every subcommand round-trips through `serde_json` into the library report types.
- [ ] `--help` for the binary and each subcommand mentions: default index location, the model-download caveat on `index build`, and the memory-exclusion guarantee on `package` (`DIST-3` — users should see the promise).
- [ ] `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` clean.
