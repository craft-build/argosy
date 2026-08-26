# 03 — Namespace Models: Skills and Styleguide Rules

| | |
|---|---|
| Depends on | 02 |
| Creates | `src/skill.rs`, `src/styleguide.rs`; extends `src/lib.rs`, fixtures |
| Spec sections | §5.1 (`DOC-1`–`DOC-3`), §5.2 (`SKL-1`–`SKL-7`), §5.3 (`MEM-1`–`MEM-4`), §5.4 (`STG-1`–`STG-8`), §10.2 (`QRY-5`) |

---

## 1. Context

Doc 02 validated generic structure. The `skill` and `styleguide` namespaces carry **additional contracts** (`SKL-1`–`SKL-5`, `STG-1`–`STG-3` are hard requirements), and both namespaces need typed models because later chunks lean on them: skill listing powers `QRY-5` (doc 05 aggregation, doc 10 MCP), styleguide rules power retrieval filters (`QRY-3`, doc 06/07) and dynamic user-authored rules (doc 04 writes, doc 08 YAML import). The `document` and `memory` namespaces are deliberately unstructured (`DOC-1`/`MEM-1`: OKF concept conformance only) — doc 02 already enforces that; nothing more is built for them here.

## 2. Requirements

### 2.1 Skill model (`src/skill.rs`)

Per `SKL-1`, a skill is either **file-form** (`skill/deploy.md`) or **directory-form** (`skill/deploy/deploy.md` plus supporting materials).

- `pub struct Skill { pub namespace_dir: PathBuf /* skill/ root */, pub name: String, pub entry_point: ConceptId, pub description: String, pub form: SkillForm }`; `enum SkillForm { SingleFile, Directory { root: PathBuf } }`.
  - `name` is the concept's base name (file stem of the entry point) — this is the key used for lookup and collisions in doc 05/precedence.
- Discovery: `Skill::list(argosy: &Argosy) -> Result<Vec<Skill>>` walks `skill/` (top level only for entry points): each `.md` file directly under `skill/` is a file-form candidate; each directory directly under `skill/` is a directory-form candidate whose entry point per `SKL-2` is `<dir>/<dir-basename>.md`.
- Validation: `Skill::validate_*` checks integrated into doc 02's report style — add `Argosy::validate_skills() -> Vec<Finding>` (reuse `Finding`/`Severity` from `bundle.rs`); each check names its requirement ID:

| Req | Check | Severity |
|---|---|---|
| `SKL-1` | Every entry under `skill/` is either a `.md` file or a directory | Error otherwise (e.g. a stray non-markdown file at top level is a Warning, not Error) |
| `SKL-2` | Directory-form skill contains `<basename>.md` entry point | Error if absent |
| `SKL-3` | Entry point `type: Skill` | Error |
| `SKL-4` | Entry point has non-empty `description` | Error |
| `SKL-5` | Entry point body is non-empty (instructions live there) | Error |
| `SKL-6` | Supporting materials under `references/` within the skill dir | Info only if materials exist elsewhere — do not over-engineer |
| `SKL-7` | Attested Computation option | no check — informational in docs |

- `Skill::list` is tolerant: it returns every skill that satisfies `SKL-1`–`SKL-5`; use `validate_skills()` for the full report. (Listing broken skills would poison every consumer; validation is where breakage is surfaced.)
- Materials inside a directory-form skill (other `.md` files, `references/`) must **not** appear as separate skills in `Skill::list`, and `Argosy::concepts(Namespace::Skill)` already returns them as plain concepts — document this distinction in `skill.rs` docs.

### 2.2 Styleguide rule model (`src/styleguide.rs`)

- `pub struct StyleguideRule<'a>` (or owning struct — your choice, justify briefly) wrapping a `Concept`/`ConceptId` with typed accessors:
  - `language() -> Option<&str>`, `category() -> Option<&str>` (`STG-4` — frontmatter `language`/`category`)
  - `rule_id() -> Option<&str>`, `priority() -> Option<&str>`, `pattern() -> Option<&str>` (`STG-5` optional metadata; `priority` values are conventionally `error`/`warn`/`info` but are not validated)
  - `good_examples()` / `bad_examples() -> Option<&str>` — body sections under `## Good` and `## Bad` headings (`STG-6`), parsed with simple heading scanning (no markdown AST dependency).
- Listing: `StyleguideRule::list(argosy) -> Result<Vec<...>>` over `styleguide/`, including subdirectory organization (`STG-7` — recurse; `styleguide/rust/naming/foo.md` is one rule).
- Filtering for later query layers: `StyleguideRule::filter(rules, language: Option<&str>, category: Option<&str>)` — exact-match on the `STG-4` fields; doc 07 combines this with vector search.
- Validation: `Argosy::validate_styleguide() -> Vec<Finding>`:

| Req | Check | Severity |
|---|---|---|
| `STG-1` | Every concept under `styleguide/` is OKF-conformant | Error (already produced by doc 02's generic pass — do not duplicate; call it out in docs) |
| `STG-2` | `type: Styleguide Rule` | Error |
| `STG-3` | non-empty, self-contained `description` | Error |
| `STG-4` | `language`, `category` set | Warning if absent |

- `STG-8` (cross-argosy rule combination with local precedence) is **doc 05** scope; note it, don't implement it here.

### 2.3 Aggregate validation

`Argosy::validate(path)` from doc 02 must now include the skill and styleguide findings (compose the three reports). Keep the standalone per-namespace validators public so callers (doc 09 CLI) can ask for one namespace only.

### 2.4 Fixtures

Extend the valid `acme-billing` fixture to include its spec §16 skills (`reconcile-ledger.md` file-form; `rotate-api-keys/` directory-form with `references/checklist.md`) and the `styleguide/rust/naming/snake-case-vars.md` rule with `## Good`/`## Bad` sections — if doc 02 didn't already create them fully, complete them now. Add broken fixtures: skill dir missing its entry point; skill without `description`; rule missing `type`; rule missing `description`.

## 3. Non-Goals

- No writes (doc 04), no cross-argosy behavior (doc 05), no embeddings (doc 06/07), no YAML import (doc 08).
- No markdown parsing beyond `## Good`/`## Bad` heading scanning.

## 4. Success Criteria

- [ ] `Skill::list` on the valid fixture returns exactly 2 skills with correct names, descriptions, and forms (file vs directory); the directory-form skill's `references/checklist.md` is not listed as a skill.
- [ ] Broken fixtures produce findings with the right IDs: missing entry point (`SKL-2`), missing `description` (`SKL-4`), wrong/missing `type` (`SKL-3`), empty body (`SKL-5`); each a test named e.g. `skl2_directory_skill_requires_entry_point`.
- [ ] `StyleguideRule::list` returns the fixture rule with `language() == Some("rust")`, `category() == Some("naming")`, and non-empty `good_examples()`/`bad_examples()`.
- [ ] `StyleguideRule::filter` narrows by language/category exactly; a rule without those fields survives listing but is flagged Warning by validation (`STG-4`).
- [ ] `Argosy::validate` on the valid fixture reports zero Errors; on each broken fixture reports the expected Error; per-namespace validators are independently callable.
- [ ] `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` clean.
