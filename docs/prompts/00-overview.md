# 00 — Overview, Conventions, and Implementation Order

| | |
|---|---|
| Status | Active |
| Audience | Flow-mode implementation agent session |
| Normative source | `docs/specification.md` — Argosy Specification v0.1.0 (requirement IDs `STR-`, `DOC-`, `SKL-`, `MEM-`, `STG-`, `PROM-`, `IDX-`, `DIST-`, `MUL-`, `QRY-`, `SEC-`, `NFR-`) |
| Informative source | `docs/reference-implementation.md` — crate + MCP server architecture |

---

## 1. What Is Being Built

The reference implementation of the Argosy specification: a single Rust crate, `argosy`, shipping

1. **a library** (`src/lib.rs`) — all format, structural, indexing, and multi-argosy logic, so any Rust harness can depend on it directly; and
2. **a binary** (`src/main.rs`) — the `argosy` CLI with subcommands `validate`, `package`, `index`, `convert styleguide`, and `mcp` (an MCP server, so non-Rust harnesses can use argosys without embedding Rust).

The starting state of the repo is an empty crate skeleton (`src/lib.rs` contains only a stub). Everything is built from scratch by this document set.

The specification is the source of truth for **what**; every document in this set cites requirement IDs from it. The reference-implementation document is the source of truth for **architecture**; where it leaves a decision open, this document set resolves it (§3 below). If a conflict is ever found between this set and `docs/specification.md`, the specification wins and the conflict must be surfaced rather than silently resolved.

## 2. How to Consume These Documents

- Implement the documents **in numeric order** (`01` through `10`). Each document depends only on earlier ones; its "Depends on" line says exactly which.
- Each document is a self-contained, deliverable chunk: context, requirements, design constraints, and a **success criteria** checklist. A chunk is done only when every success criterion is verifiably met.
- Do not read ahead and implement future chunks early. Later documents assume the exact public surface earlier documents define — building ahead creates rework.
- If implementation reveals that an earlier chunk's design is unworkable, fix the earlier code **and note the deviation in the code or PR description**; do not work around it silently downstream.
- Do not commit anything unless explicitly asked.

## 3. Locked Decisions

These were open in the reference-implementation document and are now settled. Do not re-litigate them.

| Decision | Resolution |
|---|---|
| Crate count | One crate, `argosy`, lib + bin (reference doc §2.3) |
| Default vector store | **`sqlite-vec`** — one SQLite file per argosy (`.argosy/index.db`, relative to the project root that owns the context), SQL provides the `IDX-9` structured filtering |
| Default embedding provider | **`fastembed`** (ONNX, local) behind the `EmbeddingProvider` trait; model identity recorded per `IDX-5` |
| CLI surface | Full set from the start: `mcp`, `validate`, `package`, `index`, `convert styleguide` (reference doc §6) |
| Precedence rule (`MUL-6`/`MUL-7`) | Local argosy first, then imported argosys in registration order. Deterministic and inspectable: every aggregate operation reports which argosy each result came from |
| YAML rule-set conversion | One concept file per rule, named for its `rule_id`; `## Good` / `## Bad` body headings (`STG-6`); `rule_id`/`priority`/`pattern` as optional frontmatter (`STG-5`) |

## 4. Target Crate Layout

This is the layout the documents build toward. A document states which files it creates or modifies.

```text
src/
├── lib.rs               # crate root: module wiring + public re-exports only
├── error.rs             # Error + Result (doc 01)
├── concept.rs           # OKF concept parse/serialize, ConceptId (doc 01)
├── bundle.rs            # Argosy open/validate, Manifest, Namespace (doc 02)
├── skill.rs             # skill model + validation (doc 03)
├── styleguide.rs        # styleguide rule model + validation (doc 03)
├── local.rs             # write/delete + promotion against the local argosy (doc 04)
├── context.rs           # ProjectContext, QualifiedConceptId, argosy:// URIs, precedence (doc 05)
├── index/
│   ├── mod.rs           # traits, EmbeddingUnit, reconcile, Query (doc 06)
│   ├── sqlite.rs        # SqliteVecStore   (doc 07, feature "default-index")
│   └── fastembed.rs     # FastembedProvider (doc 07, feature "default-index")
├── package.rs           # distribution packaging + YAML styleguide import (doc 08)
├── main.rs              # binary entry point (doc 09)
├── cli.rs               # clap definitions + subcommand dispatch (doc 09)
└── mcp.rs               # MCP server (doc 10, feature "mcp")
tests/
├── fixtures/            # minimally-conforming and deliberately-broken argosy trees
└── *.rs                 # integration tests (CLI in doc 09, MCP in doc 10)
```

Cargo features: `default = ["default-index", "mcp"]`; `default-index` gates the sqlite-vec/fastembed backend; `mcp` gates `rmcp` so library consumers can stay lean.

## 5. Global Conventions

These apply to every chunk. Individual documents add chunk-specific rules.

- **Toolchain.** Rust edition 2024 (already set in `Cargo.toml`). Every chunk ends with `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test` all clean.
- **Errors.** One `thiserror`-based `argosy::Error` enum plus `pub type Result<T>`, defined in doc 01. No panics, `unwrap`, or `expect` on paths reachable by untrusted input (an argosy's contents are untrusted — spec §12.1). Panics are acceptable only in tests.
- **Dependencies.** Keep the tree minimal; every dependency must be justified in the document that introduces it. Already prescribed: `thiserror`, `serde` + `serde_yaml` (frontmatter), `semver` (manifest), `tempfile` (dev). Docs 07/09/10 add `rusqlite`/`sqlite-vec`/`fastembed`, `clap`, and `rmcp` respectively.
- **Testing.** Unit tests colocated with the code they test; fixtures under `tests/fixtures/` constructed as real directory trees (write small helper builders as needed). `cargo test` with default features must require **no network access** — the fastembed model download is exercised only in a gated test (doc 07).
- **Filesystem access.** All reads/writes of bundle content go through the library's own APIs so structural rules (namespace placement, reserved filenames, imported-argosy read-only) are enforced in exactly one place. Path-traversal safety: any path derived from user or bundle input must be normalized and verified to stay inside the bundle root.
- **Spec citations.** When a requirement ID (e.g. `SKL-4`) is implemented or enforced in code, name it in a doc comment or test name, e.g. `#[test] fn skl4_skill_requires_description()`. This is the traceability mechanism for conformance claims.
- **No speculative scope.** UX decisions the spec leaves open (`PROM-5`, `SEC-3`, `SEC-4`) stay open here — the library exposes the mechanism; who triggers it is the harness's business.

## 6. Document Index

| # | Document | Delivers | Depends on |
|---|---|---|---|
| 01 | `01-core-concepts.md` | OKF concept parse/serialize, `ConceptId`, `Error` | — |
| 02 | `02-bundle-and-validation.md` | Open an argosy on disk, parse `argosy.md`, validate `STR-1`–`STR-11` | 01 |
| 03 | `03-namespaces.md` | Skill and styleguide rule models + per-namespace validation, skill listing | 02 |
| 04 | `04-local-writes-and-promotion.md` | Write/delete concepts in the local argosy; memory→document/styleguide promotion | 03 |
| 05 | `05-multi-argosy-context.md` | `ProjectContext` (one local + N imported), read-only enforcement, qualified identity, `argosy://` URIs, precedence | 04 |
| 06 | `06-index-traits-and-reconciliation.md` | `EmbeddingProvider`/`VectorStore` traits, `EmbeddingUnit`, staleness + reconcile, `Query` | 05 |
| 07 | `07-sqlite-vec-and-fastembed-backend.md` | Default index backend: `SqliteVecStore` + `FastembedProvider` | 06 |
| 08 | `08-distribution-and-import.md` | Packaging with `memory/` exclusion, integrity hashes, Craft YAML styleguide import | 07 |
| 09 | `09-cli.md` | `argosy` binary: `validate`, `package`, `index`, `convert styleguide` | 08 |
| 10 | `10-mcp-server.md` | `argosy mcp` server per the reference doc §3.2 tool/resource mapping | 09 |

## 7. Global Definition of Done

A document's chunk is complete when all of the following hold:

1. Every item in that document's success-criteria checklist passes.
2. `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test` are clean on the whole crate.
3. Public API added in the chunk has doc comments; requirement IDs are traceable per §5.
4. No undocumented deviations from this document set.
