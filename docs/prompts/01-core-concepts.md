# 01 — Core Concepts: OKF Parsing, ConceptId, Error Types

| | |
|---|---|
| Depends on | — (first chunk) |
| Creates | `src/error.rs`, `src/concept.rs`, rewires `src/lib.rs` |
| Spec sections | §1.4 (Concept), §1.5 (relationship to OKF), §4.2 (`STR-5`, `STR-6`), §5 (all the `type`/`description` contracts this layer must represent) |

---

## 1. Context

Everything in argosy is built on **concepts**: markdown files with YAML frontmatter, as defined by OKF §4. Every later module — validation, namespaces, indexing, packaging — parses, inspects, and serializes concepts. This chunk delivers that foundation plus the crate-wide error type. There is no existing OKF crate dependency; this layer implements the subset of OKF the specification relies on (the spec references OKF v0.2 concepts but only the frontmatter convention, `type`, `description`, `tags`, `generated.by`, `sources`, `verified`, `status` are load-bearing for argosy).

This chunk is pure data modeling and parsing — no filesystem walking beyond reading a single file, no argosy-specific structure (that's doc 02).

## 2. Requirements

### 2.1 Error type (`src/error.rs`)

- Define `pub enum Error` with `thiserror` and `pub type Result<T> = std::result::Result<T, Error>`.
- Variants must cover, at minimum: IO errors (with the offending path), YAML/frontmatter parse errors (with path), missing or malformed frontmatter, missing required field (name the field), and a catch-all validation message variant. Later documents extend this enum; design variants so additions don't break existing `match` ergonomics (mark the enum `#[non_exhaustive]`) — it is acceptable for variants to be added in later docs.
- `lib.rs` re-exports `Error` and `Result`; remove the skeleton `add`/`it_works` stub.

### 2.2 Concept parsing (`src/concept.rs`)

A **Concept** is one markdown file: optional YAML frontmatter delimited by `---` lines, then a markdown body.

- `pub struct Concept` holds: the parsed frontmatter as an ordered map (use `serde_yaml::Mapping` or an equivalent that **preserves unknown keys** — `STR-6` requires tolerating and round-tripping unrecognized fields without dropping them), plus the body as a `String`.
- Typed accessors for the fields argosy cares about; all return borrowed data or clones, never panic on absence:
  - `concept_type() -> Option<&str>` — the frontmatter `type`
  - `description() -> Option<&str>`
  - `tags() -> Vec<&str>` (accept both a YAML sequence and a single string)
  - plus generic `get(key)` / `get_str(key)` for anything else (`generated.by`, `sources`, `verified`, `status`, and the styleguide fields added in doc 03 are all read through these).
- Parsing functions:
  - `Concept::from_str(&str) -> Result<Concept>`
  - `Concept::from_file(path) -> Result<Concept>` (attaches the path to any error)
- Serialization: `Concept::to_string()` / `to_file()` must round-trip — a concept parsed and re-serialized preserves unknown frontmatter keys and body content byte-for-byte where reasonable (formatting normalization of the YAML block itself is acceptable; losing keys or reordering the body is not).
- **Conformance predicate**: `is_okf_conformant() -> bool` — true iff frontmatter exists and `type` is present and non-empty. Per spec §1.5 and `STR-5`, a non-empty `type` is the only hard OKF concept requirement argosy depends on; this predicate is the single place that rule lives.

### 2.3 ConceptId (`src/concept.rs`)

- `pub struct ConceptId` — a concept's identity within one bundle: its path relative to the bundle root, without the `.md` extension, using forward slashes (e.g. `document/decisions/2026-05-caching`).
- Construct from a relative path (validating: no `..`, no absolute components, `.md` extension handled/stripped consistently) and render back to a relative path.
- `Display` produces the slash-separated id; `FromStr` / `TryFrom<&Path>` both provided. `SKL-2` (doc 03) and the `argosy://` URI scheme (doc 05) build on this type.

### 2.4 Dependencies added in this chunk

`thiserror`, `serde` (derive), `serde_yaml`. Dev: `tempfile` (for `to_file`/round-trip tests).

## 3. Non-Goals for This Chunk

- No directory walking, no namespace awareness, no manifest handling (doc 02).
- No namespace-specific typed fields (language/category/rule_id — doc 03 reads those through `get_str`).
- No validation beyond OKF conformance (`type` present) — argosy-level structure is doc 02's `ValidationReport`.

## 4. Success Criteria

- [ ] `cargo test` passes with unit tests demonstrating each of:
  - [ ] Parsing a concept with full frontmatter yields correct `concept_type`, `description`, `tags` (sequence form *and* single-string form).
  - [ ] A file without frontmatter parses to an empty frontmatter map with the whole file as body, and `is_okf_conformant()` is `false`.
  - [ ] A frontmatter block with an empty or missing `type` makes `is_okf_conformant()` `false`; any non-empty `type` (including one argosy has never heard of) makes it `true` (OKF tolerance, spec §1.5).
  - [ ] Unknown frontmatter keys survive a parse → serialize → parse round-trip (basis for `STR-6`).
  - [ ] Malformed YAML frontmatter produces an `Err` naming the file, not a panic.
  - [ ] `ConceptId` round-trips: `document/decisions/2026-05-caching` ↔ path; `..` and absolute paths are rejected.
- [ ] `Error` carries file paths in its IO and parse variants (assert via `Display` output in a test).
- [ ] `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` clean.
- [ ] `src/lib.rs` exposes `error`, `concept` modules and re-exports `Error`, `Result`, `Concept`, `ConceptId`; skeleton stub removed.
