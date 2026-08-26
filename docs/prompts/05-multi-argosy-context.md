# 05 — Multi-Argosy Project Context: Identity, Read-Only Enforcement, Precedence

| | |
|---|---|
| Depends on | 04 |
| Creates | `src/context.rs`; extends `src/lib.rs`, `src/error.rs` |
| Spec sections | §3.3, §9 (`MUL-1`–`MUL-7`), §5.4 (`STG-8`), §10.2–10.3 (`QRY-5`, `QRY-6` groundwork), reference doc §3.2 (URI scheme) |

---

## 1. Context

A **project context** (spec §1.4) activates one local argosy plus any number of imported ones (e.g. an argosy shipped with a dependency). This chunk builds the `ProjectContext` type everything above the library's single-bundle layer operates on — docs 06–07 (index spans all active argosys), 09 (CLI), and 10 (MCP server is "launched with a project root so it knows which argosy is local"). It is where the spec's composition rules become enforced behavior rather than prose: one local at a time (`MUL-1`), imported argosys are read-only (`MUL-3`), identity is qualified by argosy (`MUL-5`), and aggregate operations apply a deterministic precedence (`MUL-6`/`MUL-7`).

Locked decision (doc 00 §3): precedence = local first, then imported in registration order, and every aggregate result reports its origin argosy (inspectable, `MUL-6`).

## 2. Requirements

### 2.1 Qualified identity (`src/context.rs`)

- `pub struct QualifiedConceptId { pub argosy: String /* manifest name */, pub namespace: Namespace, pub id: ConceptId }` (`MUL-5`). Two argosys both defining `document/architecture.md` are two distinct `QualifiedConceptId`s — add the spec's own example as a test.
- **`argosy://` URIs** (needed by doc 10's Resources; reference doc §3.2): `argosy://<argosy-name>/<namespace>/<concept-id>`, e.g. `argosy://acme-billing/document/decisions/2026-05-caching`.
  - `QualifiedConceptId::to_uri() -> String` and `from_uri(&str) -> Result<QualifiedConceptId>` with strict parsing: scheme must be `argosy`, at least namespace + one path segment, namespace must be one of the four reserved names (custom namespaces are not addressable by URI — document why: identity across custom namespaces is producer-defined).
  - Round-trip tests including concept ids with `/` separators and percent-reserved characters rejected rather than silently mangled (keep v1: no percent-encoding; reject ids containing characters outside `[A-Za-z0-9._-/]` — record the limitation in a doc comment).

### 2.2 `ProjectContext`

- `pub struct ProjectContext { local: LocalArgosy, imported: Vec<Argosy> }`, built via `ProjectContext::open(local_path, imported_paths: impl IntoIterator<Item = PathBuf>) -> Result<ProjectContext>`.
- `MUL-1`: exactly one local argosy by construction (the signature takes one local path); opening validates every argosy (reuse doc 02 `Argosy::open`); a failure to open any activated argosy fails the context with that path in the error.
- `MUL-2`: any number of imported, including zero.
- Lookups:
  - `argosy_named(&self, name: &str) -> Option<ArgosyRef>` where `ArgosyRef` is `Local(&LocalArgosy)` or `Imported(&Argosy)` — imported names colliding with each other or the local name: **error at `open`** (identity by name, `MUL-5`, must be unambiguous).
  - `resolve(&self, qid: &QualifiedConceptId) -> Result<Concept>` — direct lookup, `QRY-4`: read the concept at that namespace/id from that argosy; `ConceptNotFound` otherwise.
  - `read_uri(&self, uri: &str) -> Result<Concept>` — `from_uri` + `resolve` (doc 10's Resource handler calls exactly this).
- **Read-only enforcement** (`MUL-3`): `ProjectContext` exposes no write methods for imported argosys at all (the `LocalArgosy` type from doc 04 is the only write-bearing handle, reached via `context.local()`). Additionally, an imported argosy that *contains* a `memory/` directory is tolerated but exposed read-only like everything else (`STR-9`) — add a fixture proving a `write` attempt cannot even be expressed: the test asserts `context.local()` is the only mutable accessor (compile-time) plus a runtime test that `resolve` into an imported `memory/` works for reads.
- `MUL-4`: `context.local()` returns `&LocalArgosy` with the full doc 04 write surface — one test exercises write→read through the context.

### 2.3 Aggregate operations + precedence (`MUL-6`/`MUL-7`, `STG-8`, `QRY-5`)

- `list_skills(&self) -> Vec<SkillListing>` where `SkillListing { argosy: String, skill: Skill }`: concatenates local + imported skills in precedence order and **annotates collisions**: if two argosys provide a skill with the same `name`, mark each entry `shadowed: bool` (all but the first-in-precedence are shadowed). Never silently drop the losers — the listing stays inspectable (`MUL-6`).
- `resolve_skill(&self, name) -> Option<&SkillListing>` — returns the highest-precedence skill of that name (local over imported, `MUL-7`; earlier-registered import over later). Test both collision directions.
- `list_rules(&self, language: Option<&str>, category: Option<&str>) -> Vec<RuleListing>` — combines rules across all active argosys (`STG-8` says combine, not replace), each annotated with its origin argosy, reusing doc 03's `StyleguideRule::filter`.
- No search here — semantic ranking across argosys (`QRY-6`/`QRY-7`) is doc 06/07; these listings are the filesystem-level ground truth those documents must match.

## 3. Non-Goals

- No vector index, no semantic ranking (`QRY-1` etc.) — docs 06/07.
- No packaging/distribution — doc 08.
- `SEC-1`–`SEC-3` trust surfacing is doc 10 presentation; this chunk's contribution is that every aggregate result carries its origin argosy so a consumer *can* distinguish imported content.

## 4. Success Criteria

- [ ] URI round-trip tests: parse/format agree for nested concept ids; wrong schemes, missing segments, custom namespaces, and out-of-charset ids are all `Err`.
- [ ] Identity test: two fixture argosys both containing `document/architecture.md` yield two distinct `QualifiedConceptId`s and `resolve` returns each argosy's own content (`MUL-5`).
- [ ] `ProjectContext::open` rejects duplicate argosy names (local vs imported, and imported vs imported) with the colliding name in the error.
- [ ] Collision tests: a skill `deploy` in both local and an imported argosy — `list_skills` returns both, exactly the imported one flagged `shadowed`; `resolve_skill("deploy")` returns the local one; with two imports, registration order decides between them (`MUL-6`/`MUL-7`).
- [ ] `STG-8` test: rules for `rust`/`naming` in local + imported argosys both appear in `list_rules`, each tagged with its origin.
- [ ] Imported argosy containing `memory/` (fixture): readable via `resolve`/`read_uri`, and validation of the context does not fail (`STR-9` tolerance).
- [ ] Write test: a memory write via `context.local()` then `resolve` returns the new concept.
- [ ] `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` clean.
