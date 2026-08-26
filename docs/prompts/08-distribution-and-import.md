# 08 — Distribution Packaging, Integrity, and Styleguide YAML Import

| | |
|---|---|
| Depends on | 07 |
| Creates | `src/package.rs`; extends `src/error.rs`, `src/lib.rs`, fixtures (Craft YAML rule-set samples) |
| Spec sections | §6 (`PROM-4` downstream), §8 (`DIST-1`–`DIST-6`), §7.6 (`IDX-14`–`IDX-16`), `MEM-3`, `DIST-4`, reference doc §4 + §6 (YAML conversion) |

---

## 1. Context

Two directions of content movement, one module. **Out**: packaging an argosy for distribution — the markdown bundle is the artifact (`DIST-1`), `memory/` never leaves (`DIST-3`/`MEM-3`), the index may ride along as an optional precomputed cache (`IDX-14`). **In**: seeding an argosy's `styleguide/` namespace from Craft's existing YAML rule sets — the one-time conversion the reference doc (§4, §6) prescribes so Craft's review flow can move onto argosy concepts.

## 2. Requirements

### 2.1 Packaging — `package.rs`

- `pub struct PackageOptions { pub include_index: bool /* IDX-14, default false */, pub format: PackageFormat }`; `enum PackageFormat { Directory, TarGz }` — the two mechanisms the CLI exposes (git-distribution is just "commit the bundle"; `DIST-2` needs no code).
- `pub fn package(source: &Argosy, dest: &Path, options: &PackageOptions) -> Result<PackageReport>`.
- Copy rules:
  - Walk the bundle root; copy `argosy.md`, all namespace directories, `index.md`/`log.md` files, and custom namespaces VERBATIM.
  - **`memory/` is never copied** (`DIST-3`/`MEM-3`) — unconditional, no override flag; the exclusion is structural (the walker filters by top-level component, not by glob, so a nested `document/memory/` directory is *not* excluded — only the root namespace).
  - `.argosy/` (index dir) excluded unless `include_index` (`IDX-14`); when included it ships as the precomputed cache and consumers still treat it as derivative (`IDX-16`) — doc 07's reconcile handles model mismatch.
  - Symlinks: don't follow out-of-bundle links; copy file contents, error on a symlink that would escape the bundle root.
- `PackageReport { files_copied: usize, memory_excluded: bool, warnings: Vec<String> }`:
  - `DIST-4`: if `memory/` existed at the source at packaging time, push an explicit warning ("memory/ present and excluded from package") — the safeguard against a packaging path that failed to exclude (the report makes the exclusion *visible* even when it worked).
- Integrity (`DIST-6`): after copying, emit `SHA256SUMS`-style sidecar `<dest>/argosy-integrity.txt` listing `sha256  <relative-path>` for every copied file (manifest first, then sorted paths). Also expose `pub fn bundle_content_hash(argosy_root: &Path) -> Result<String>` — a single SHA-256 over the ordered list of (path, file-hash) pairs; this is the spec's recommended change detector independent of `argosy_version` bumps. Both reuse via `sha2` crate (new dependency — justify: `DIST-6` needs a standard hash; `sha2` is the conventional choice).
- `TarGz`: stream via `tar` + `flate2` (new dependencies; keep them feature-ungated — `package` is core CLI function). The integrity file is written *inside* the archive root.
- Versioning helper (`DIST-5`): `Manifest` already carries `argosy_version`; add nothing — but `PackageReport` echoes `name` + `argosy_version` so the CLI can print "packaged acme-billing 0.3.1".

### 2.2 Styleguide YAML import — `import_styleguide_yaml`

Craft's rule sets (reference doc §4): YAML files keyed per language/category, each rule carrying `id`, `description`, `language`, `category`, optional `priority` (`error`/`warn`/`info`), optional `pattern`, and good/bad examples. Locked mapping (reference doc §6):

- Input: a directory of `*.yaml`/`*.yml` files, each decoding to a sequence of rule objects (this is the observed Craft shape; be liberal: also accept a mapping with a top-level `rules:` key). Rule objects: required `id`, `description`; optional `language`, `category`, `priority`, `pattern`, `good` (string or list of strings), `bad` (same), plus free-form extra keys tolerated.
- Output per rule: one concept under the **local** argosy at `styleguide/<language or "general">/<category or "misc">/<RULE-ID>.md` (rule id as filename, per reference doc §6), frontmatter: `type: Styleguide Rule`, `description`, and optional `language`, `category`, `rule_id`, `priority`, `pattern` (`STG-4`/`STG-5`); body = guidance text followed by `## Good` / `## Bad` sections when examples exist (`STG-6`), list-form examples rendered as `- ` bullets.
- API: `pub fn import_styleguide_yaml(local: &LocalArgosy, yaml_dir: &Path) -> Result<ImportReport>` where `ImportReport { written: usize, skipped_existing: Vec<String>, findings: Vec<Finding> }`.
  - Write through doc 04's `write_concept` so the `STG-2`/`STG-3` contract is enforced by the same code path; a rule that would fail validation is collected into `findings`, not written, and does not abort the batch (report-level Partial-Failure tolerance; the command exits non-zero in doc 09 if any findings exist).
  - Existing concept at the target path → `skipped_existing` (imports are additive and re-runnable; overwriting user edits silently is how you lose trust — `STG-8`'s spirit of user-extensible rule sets).
- After import, `Argosy::validate_styleguide` on the target must report zero Errors — an end-to-end test asserts this on the converted output.

### 2.3 Dependencies added

`sha2`, `tar`, `flate2`. (`serde_yaml` already present from doc 01 for the YAML decode.)

## 3. Non-Goals

- No signing/encryption/access control (spec §12.3, §15).
- No registry/upload protocol (`DIST-2` stays mechanism-agnostic; packaging stops at directory/tarball).
- No migration of YAML-in-use consumers — this produces the concepts; Craft wiring is its own project (reference doc §4).

## 4. Success Criteria

Each test builds fixtures in `tempfile` dirs:

- [ ] Package a fixture argosy with `memory/gotchas.md`, `document/`, `styleguide/`, a custom namespace, `.argosy/index.db`, and nested `document/memory-notes/` (note: *not* excluded — naming guard test): output contains everything except root `memory/` and `.argosy/`; `memory_excluded == true`; the `DIST-4` warning is present; nested lookalike dirs survive.
- [ ] `package` with `include_index: true` ships `.argosy/index.db` (`IDX-14`).
- [ ] `argosy-integrity.txt` exists, hashes match recomputation for spot-checked files; `bundle_content_hash` changes when any concept body changes and is stable across a copy (`DIST-6`).
- [ ] `TarGz` round trip: package to tarball → extract → `Argosy::open` succeeds and `is_conformant()`.
- [ ] Symlink escaping the bundle errors; in-bundle symlink is materialized as contents.
- [ ] A packaged argosy (directory form) passes `Argosy::open` + `validate` with zero Errors — the package is itself a conformant argosy (`DIST-1`).
- [ ] YAML import: sample Craft-style rule file (2+ languages, one rule with pattern+priority, one minimal rule) → concepts at the locked paths with correct frontmatter and `## Good`/`## Bad` bodies; `validate_styleguide` clean; second run reports all rules in `skipped_existing`; a rule missing `description` appears in `findings` and is not written.
- [ ] `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` clean.
