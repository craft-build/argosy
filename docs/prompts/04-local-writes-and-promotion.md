# 04 — Local Argosy Writes, Memory Operations, and Promotion

| | |
|---|---|
| Depends on | 03 |
| Creates | `src/local.rs`; extends `src/lib.rs`, `src/error.rs` |
| Spec sections | §5.3 (`MEM-1`–`MEM-4`), §6 (`PROM-1`–`PROM-5`), §9 (`MUL-3`/`MUL-4`), §12.2 (`SEC-4`/`SEC-5` — surfaced as API design, not enforced) |

---

## 1. Context

So far the library only reads. A live argosy is a working layer: the harness writes memory notes as it learns (spec §5.3), users drop new styleguide rules into the local argosy (spec §5.4), and matured memory gets **promoted** into `document/` or `styleguide/` — the sole path by which memory-originated content becomes distributable (spec §6). This chunk builds the write surface. It operates on a single argosy designated *local*; the read-only rule for imported argosys (`MUL-3`) is enforced structurally in doc 05's `ProjectContext` — the design decision here is that **write APIs only exist on a `LocalArgosy` type**, so "writing to an imported argosy" is unrepresentable by construction (doc 05 wraps this).

`PROM-5` (what triggers promotion) and `SEC-4`/`SEC-5` (human confirmation, surfacing source content) are explicitly harness UX; the library's job is to make those policies *implementable* — promotion returns everything a confirmation dialog needs.

## 2. Requirements

### 2.1 `LocalArgosy` (`src/local.rs`)

- `pub struct LocalArgosy(Argosy)` — newtype obtained via `LocalArgosy::open(path) -> Result<LocalArgosy>`; derefs/exposes all doc 02–03 read APIs.
- `MUL-4` honored intrinsically: every namespace the library supports is writable through this type.

### 2.2 Generic write/delete

- `write_concept(&self, namespace: Namespace, id: &ConceptId, concept: &Concept) -> Result<PathBuf>`:
  - Creates parent directories as needed (supports new subdirectories — `MEM-4`, `STG-7` free organization).
  - **Refuses** reserved filenames as targets (`argosy.md`, `index.md`, `log.md` — spec §4.4) and any `Namespace::Custom` (custom namespaces are producer-owned; the library doesn't know their semantics — document this).
  - **Pre-write validation**: the concept being written must satisfy the target namespace's hard requirements, reusing doc 02/03 logic — `skill/` entries must satisfy `SKL-1`–`SKL-5` *if* the write lands at an entry-point position (file-form or directory-form entry point), `styleguide/` entries must satisfy `STG-2`/`STG-3` (`type` + `description`), all targets must satisfy OKF conformance (`type` present). Invalid writes are `Err` — the library never writes a concept it would itself flag.
  - Path safety: the final path must be verified to lie under `root/namespace_dir` after normalization (per doc 00 §5).
- `delete_concept(&self, namespace: Namespace, id: &ConceptId) -> Result<()>`: removes the file; deleting a directory-form skill entry point must error with guidance to delete the skill directory instead (partial deletion would silently violate `SKL-2`); cleaning up now-empty parent directories is allowed but stop at the namespace root.
- Convenience wrappers the MCP layer (doc 10) maps 1:1: `write_memory`, `delete_memory` (`MEM-1`–`MEM-4` read/write/delete), `write_rule`, `delete_rule`, `write_document`, `delete_document`.

### 2.3 Promotion (spec §6)

- `pub struct Promotion { pub source_id: ConceptId, pub target: PromotionTarget, pub drafted: Concept }`; `enum PromotionTarget { Document, StyleguideRule }`.
- `promote_memory(&self, source: &ConceptId, target: PromotionTarget, new_id: &ConceptId) -> Result<Promotion>`:
  - `PROM-1`: creates a **new** concept under `document/` or `styleguide/` whose content derives from the named `memory/` concept. The derivation is mechanical here — the body is copied and the frontmatter rebuilt — because rewriting for an external reader is the harness/LLM's job (`PROM-2`'s rationale; state this in docs). The returned `Promotion.drafted` concept is what was written, so a harness can present it for review (`SEC-5`).
  - `PROM-2`: the source file is **never moved or renamed** — only a new file appears in the target namespace. Assert in tests that the source is byte-identical before and after.
  - `PROM-3`: the source is left in place by default; offer `delete_memory` as the caller's discretionary follow-up, do not auto-delete.
  - `PROM-4`: the new concept's frontmatter gets a `sources` entry whose `resource` names the memory concept's bundle-relative path (e.g. `memory/gotchas.md`); preserve existing `sources` if the caller pre-seeded any. For `PromotionTarget::StyleguideRule`, the drafted concept must satisfy `STG-2`/`STG-3` — set `type: Styleguide Rule`, require/carry a `description` (if the memory note lacks one, the `description` parameter... keep the API explicit: `promote_memory` takes an optional `description_override: Option<&str>`; if neither the source nor the override provides one, error rather than write an invalid rule).
  - The `new_id` must not collide with an existing concept in the target namespace (error `ConceptExists` — no silent overwrites anywhere in this module; overwrites of *existing* concepts go through `write_concept` deliberately).
  - `MEM-3` interplay: promotion changes nothing about the memory concept's distribution status — note in the module docs that doc 08's packaging excludes `memory/` unconditionally regardless of what `sources` entries say.

### 2.4 Error variants added

`ConceptExists(ConceptId)`, `ConceptNotFound(ConceptId)`, `ReservedFilename`, `NamespaceContractViolation { requirement: &'static str, detail: String }`.

## 3. Non-Goals

- No multi-argosy coordination (doc 05 — `LocalArgosy` is handed to `ProjectContext` as-is).
- No index invalidation hooks — doc 06's reconcile discovers changes via content hashing (`IDX-11`), so writes need no notification machinery. Document this explicitly so nobody adds a callback system.
- No trust-tier/confirmation flow (`SEC-2`–`SEC-5`) — the API *returns* the source content and draft; UIs decide what to show.

## 4. Success Criteria

All tests use `tempfile` copies of the doc 02/03 valid fixture — never mutate shared fixtures.

- [ ] Write→read round trips for each writable namespace: memory note, styleguide rule, document; new nested subdirectories are created (`MEM-4`/`STG-7`).
- [ ] Write rejections, each a test: reserved filename target; `Namespace::Custom`; styleguide write without `type: Styleguide Rule` or without `description` (`STG-2`/`STG-3`, surfaced as `NamespaceContractViolation` naming the requirement); skill entry-point write missing `description` (`SKL-4`); path containing `..`.
- [ ] `promote_memory` into `document/`: new concept exists at the target id; `sources` cites `memory/<name>.md`; **source file byte-identical** afterwards (`PROM-1`/`PROM-2`/`PROM-4`); promoting again to the same id errors `ConceptExists`.
- [ ] `promote_memory` into `styleguide/`: drafted concept has `type: Styleguide Rule`; missing description with no override errors; override is used (`PROM-4`).
- [ ] After promotion, `memory/` still contains the source and `Argosy::validate` still reports zero Errors on the whole tree (`PROM-3`).
- [ ] Delete tests: delete a memory note (gone, empty parents pruned to namespace root); deleting a directory-form skill's entry point is refused with the documented guidance.
- [ ] `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` clean.
