# 02 — Bundle Opening, Manifest, and Structural Validation

| | |
|---|---|
| Depends on | 01 |
| Creates | `src/bundle.rs`; extends `src/error.rs`, `src/lib.rs`; `tests/fixtures/` argosy trees |
| Spec sections | §4.1 (`STR-1`–`STR-3`), §4.2 (`STR-4`–`STR-6`), §4.3 (`STR-7`–`STR-11`), §4.4, §11 step 2, `NFR-5` |

---

## 1. Context

An argosy is a directory: an OKF bundle root containing exactly one `argosy.md` manifest and optional namespace directories (`document/`, `skill/`, `memory/`, `styleguide/`, plus tolerated custom namespaces). This chunk delivers the type that **opens** such a directory and the validator that answers "is this a conformant argosy?" — the Validate step of the lifecycle (spec §11). Per OKF's permissive conformance, validation reports findings; it does not hard-reject over tolerable issues (unknown fields, unknown namespaces, broken bodies).

The spec example argosy (§16, Appendix A, `acme-billing`) is the reference shape for the valid fixture.

## 2. Requirements

### 2.1 Namespace enumeration (`src/bundle.rs`)

- `pub enum Namespace { Document, Skill, Memory, Styleguide, Custom(String) }` with `as_dir_name()`, and classification `from_dir_name(&str)`. The four reserved names map to their variants per `STR-7`; any other top-level directory maps to `Custom` (`STR-10`) unless it collides with a reserved **filename** (`index.md`, `log.md`, `argosy.md` are files, but a *directory* named e.g. `argosy.md` is malformed — report it).
- `Namespace::RESERVED` list (the four names) and `RESERVED_FILENAMES` (`argosy.md`, `index.md`, `log.md`) as public constants — doc 04 (writes) and doc 08 (packaging) both need them.

### 2.2 Manifest (`src/bundle.rs`)

- `pub struct Manifest` parsed from the root `argosy.md` concept: `name: String` (required), `argosy_version: semver::Version` (required), `okf_version: Option<String>`, `description: Option<String>`, and retention of any other frontmatter keys (unknown fields tolerated, `STR-6`).
- Missing/empty `name` or missing/malformed `argosy_version` is a validation **error** (`STR-4`/`STR-5` and §16's manifest shape); missing `okf_version`/`description` is a **warning** (they are `SHOULD`).
- Declared version tolerance (`NFR-5`): the validator never rejects an argosy for an old or newer `okf_version`/`argosy_version` — at most an informational finding.

### 2.3 The `Argosy` handle

- `pub struct Argosy { root: PathBuf, manifest: Manifest, ... }`, opened via `Argosy::open(path) -> Result<Argosy>`. Opening performs validation and **errors only on hard failures** (no `argosy.md` at root, or a root `argosy.md` that isn't a parseable `Argosy Manifest` concept); soft findings come back through `validate()` below even on a successfully opened argosy.
- Accessors: `root()`, `manifest()`, `namespace_dir(Namespace) -> Option<PathBuf>` (per `STR-8`, absent namespaces are fine), `namespaces_present() -> Vec<Namespace>`, and `concepts(Namespace) -> Result<Vec<(ConceptId, Concept)>>` — a recursive walk of that namespace directory collecting `.md` files as concepts (skip `index.md`/`log.md` listing/history files from concept lists; they are OKF reserved files, §4.4). Directory walking must be deterministic (sorted order).
- `Argosy::validate(path) -> ValidationReport` — the standalone entry point the CLI's `validate` command (doc 09) uses; works on any directory, whether or not `Argosy::open` would accept it.

### 2.4 `ValidationReport`

- `pub struct ValidationReport { findings: Vec<Finding> }`; `Finding { severity: Severity, id: Option<&'static str>, path: Option<PathBuf>, message: String }`; `Severity::{Error, Warning, Info}`.
- Helpers: `is_conformant() -> bool` (no `Error` findings), `errors()`, `warnings()`, `Display` rendering one finding per line as `[ERROR STR-4] path: message`.
- Every finding that corresponds to a spec requirement carries that requirement's ID (`id: Some("STR-4")`).

### 2.5 Requirements checked (each needs a fixture + test)

| Req | Check | Severity |
|---|---|---|
| `STR-1` | Root looks like an OKF bundle (has at least `argosy.md` as a concept; deep OKF conformance beyond concept-level checks is out of scope — note this boundary in the report docs) | Error if root is not a directory / unreadable |
| `STR-2` | Exactly one `argosy.md`, at root | Error if missing; Error if a second `argosy.md` exists anywhere below root (`STR-3`) |
| `STR-4` | Root `argosy.md` is a parseable concept | Error if unparseable / missing frontmatter |
| `STR-5` | Root `argosy.md` has `type: Argosy Manifest` | Error otherwise |
| `STR-5`/`§4.2` | Manifest has non-empty `name` and valid semver `argosy_version` | Error |
| `§4.2` | Manifest has `okf_version`, `description` | Warning if absent |
| `STR-6` | Unknown manifest keys | tolerated — no finding (contrast test: they parse fine and are retained) |
| `STR-7` | Reserved namespace names, when present, are top-level dirs | Error if e.g. `document/document/`-style shadowing attempts place a *reserved-named* directory somewhere weird is fine to ignore; the real check: a top-level **file** named `document`/`skill`/`memory`/`styleguide` |
| `STR-9` | `memory/` present | Info (allowed for a local argosy; the imported-read-only rule is `MUL-3`, enforced in doc 05 — note that here) |
| `STR-10` | Custom top-level dirs | tolerated; enumerated as `Namespace::Custom` |
| `STR-11` | Custom dir/file colliding with a reserved filename (`argosy.md` used as ordinary concept elsewhere, `index.md`/`log.md` as dir names) | Error |
| `STR-3` | Nested `argosy.md` anywhere below root | Error |

Concept-level conformance (`type` present) for every `.md` under the four reserved namespaces is checked here too (generic half of `DOC-1`/`MEM-1`/`STG-1`; namespace-specific contracts like `SKL-3` are doc 03).

### 2.6 Dependencies added

`semver`. (Walking: implement recursion by hand with `std::fs` — no `walkdir` needed at this scale.)

## 3. Non-Goals

- Namespace-specific contracts (`SKL-*`, `STG-*`) — doc 03.
- Any mutation of the bundle — docs 04/08.
- Index/database concerns — docs 06/07. The `.argosy/` index directory, if encountered in fixtures, must be **ignored** by validation and concept walking (it is a derivative, not bundle content — spec §3.1).

## 4. Success Criteria

- [ ] `tests/fixtures/` contains: a fully conformant argosy (mirroring the spec §16 `acme-billing` tree, with real file contents), and broken variants covering every Error row in §2.5 (missing manifest, wrong manifest `type`, nested `argosy.md`, bad semver, top-level file named `document`, custom dir named `index.md`, `argosy.md` used as a regular concept under `document/`).
- [ ] `Argosy::open` on the valid fixture returns manifest fields matching the fixture (`name`, `argosy_version`, `okf_version`); `namespaces_present()` lists all four reserved namespaces.
- [ ] `validate` on each broken fixture produces the expected `Error` finding with the correct requirement ID; on the valid fixture `is_conformant()` is true with no `Error` findings.
- [ ] Unknown manifest keys and a `Custom` namespace produce no findings and are retained/enumerated (`STR-6`, `STR-10` tests).
- [ ] `concepts(Skill)` on the valid fixture returns all skill concepts in sorted order, excluding any `index.md`/`log.md`.
- [ ] An `.argosy/` directory placed in the valid fixture is ignored by validation and walking.
- [ ] `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` clean.
