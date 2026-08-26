# Argosy Reference Implementation: Crate and MCP Server Architecture

| | |
|---|---|
| Status | Proposal |
| Version | 0.1.0 |
| Date | 2026-08-24 |
| Implements | Argosy Specification v0.1.0 (`specification.md`) |
| Relationship to the specification | Informative. Nothing here changes any `MUST`/`SHOULD`/`MAY` requirement in the specification — it describes one way to build a conformant implementation, not the only way. |

---

## 1. Overview and Rationale

The Argosy Specification defines a format and a set of required capabilities without naming a database, embedding model, or programming language (`NFR-1`). This document proposes a concrete Rust-based reference implementation: a single crate, `argosy`, that ships both a library and a binary:

1. **A library (`lib.rs`)** — the actual logic, published so any Rust project can depend on it directly.
2. **A binary (`main.rs`)** — the `argosy` CLI, which includes the MCP server as a subcommand, so any MCP-compatible tool can use argosys without embedding Rust at all.

A Rust-native harness (Craft is the motivating example) depends on the library directly. Everything else — a non-Rust harness, or a Rust harness that would rather not embed the logic — launches the MCP server via the binary. Both paths exercise the same underlying code; only the boundary differs.

## 2. Crate Architecture

A single crate, `argosy`, with the shared logic in `lib.rs` and the MCP server and other CLI commands in `main.rs`.

### 2.1 The library (`lib.rs`)

Owns everything in the specification that is format, structural, and indexing logic:

- OKF/argosy bundle parsing and validation (§4: `STR-1`–`STR-11`)
- Namespace and manifest handling (§5)
- Promotion semantics (§6: `PROM-1`–`PROM-5`)
- Multi-argosy identity and precedence (§9: `MUL-1`–`MUL-7`)
- Distribution/packaging helpers, including the `memory` exclusion (§8: `DIST-1`–`DIST-6`)
- The trait definitions an index implementation must satisfy — the Rust expression of the `IDX-` requirements in §7 — plus a default, concrete implementation:
  - An embedded vector store satisfying `IDX-7`–`IDX-13` (similarity search, namespace/metadata-filtered search, incremental upsert/delete)
  - A local embedding provider satisfying `IDX-5`/`IDX-6` (model identity recorded; no network dependency required to be useful out of the box)

The index implementation is exposed behind a trait and is meant to be replaceable: a consumer with its own embedding pipeline implements the trait directly and ignores the default backend entirely.

### 2.2 The binary (`main.rs`)

The `argosy` executable provides the MCP server and other CLI commands (validation, packaging, index inspection), built on the library. It depends on `rmcp`, the official Rust MCP SDK, and is described in §3.

### 2.3 Why One Crate

Keeping everything in one crate avoids version skew between the format logic and its default index backend, and keeps the dependency story simple for consumers: one crate, one version. The replaceability the split used to provide is preserved by the trait boundary — the default vector store and embedding provider remain optional at the feature level, so a consumer with its own embedding infrastructure (Craft, which already runs local ONNX embeddings via fastembed for its context-compaction feature) implements the trait around what it already has instead of carrying two embedding stacks side by side.

## 3. The MCP Server (`argosy mcp`)

### 3.1 Role

The MCP server is a subcommand of the `argosy` binary, built on the library with the default backend enabled. From the specification's point of view (§11), it *is* a harness: it performs discovery, validation, index building, and activation internally, then exposes the result through MCP instead of a native API.

It is launched with a project root (matching the specification's project context, §1.4) so it knows which argosy is local and which are imported.

### 3.2 Tool and Resource Mapping

MCP distinguishes **Resources** (addressable, read-oriented data) from **Tools** (invocable actions). Argosy's own requirements map onto that split fairly directly:

| Argosy capability | Specification reference | MCP primitive | Note |
|---|---|---|---|
| Semantic search | `QRY-1`, `QRY-2`, `QRY-3` | Tool | Namespace/argosy scope and `tags`/`type` filters as parameters |
| Direct concept lookup | `QRY-4` | Resource | Addressed as `argosy://<argosy-name>/<namespace>/<concept-id>` |
| List available skills | `QRY-5` | Tool | Returns each skill's name and `description` |
| Read a memory concept | `MEM-1`–`MEM-4` (read) | Resource | Same URI scheme as document/skill concepts |
| Write or delete a memory concept | `MEM-1`–`MEM-4` (write/delete) | Tool | Mutating, so a Tool rather than a Resource |
| Promote memory into a document or styleguide rule | `PROM-1`–`PROM-4` | Tool | The MCP-side trigger for §6's promotion pathway (`PROM-5` leaves the trigger unspecified; this is one valid choice) |
| List active argosys | §9 | Resource | Distinguishes the local argosy from imported ones (`MUL-5`) |
| Browse an argosy's contents | OKF §8 | Resource | Exposes a bundle's `index.md`, where present |
| Search styleguide rules | §5.4, `QRY-1`–`QRY-3` | Tool | Semantic match against code or a change under review, filtered by `language`/`category` |
| Read a styleguide rule | `QRY-4` | Resource | Same URI scheme as other concepts |
| Write or delete a styleguide rule | `MUL-4` | Tool | Local argosy only; enables dynamic user expansion of rule sets |

### 3.3 Transport

`rmcp` supports both stdio and HTTP transports. Recommendation: stdio as the default (matching how most coding-tool MCP servers are launched today, as local subprocesses), with HTTP available as a secondary mode for shared or remote deployments — for instance, a team-hosted `argosy mcp` instance serving an argosy multiple people's harnesses read from.

## 4. Craft's Direct Integration

Craft depends on the `argosy` library (using either the default index backend or its own trait implementation) directly through Cargo — no MCP, no subprocess boundary. Consistent with the specification's decision that the skill namespace is additive, not a replacement (§2, decision 2 of the specification), Craft's existing `skill` and `memory` Lua-plugin tools stay as they are; the integration point is wiring those tools to also consult an active argosy via the library when one is present, rather than introducing a parallel set of argosy-specific commands. Beyond skills and memory, Craft's review flow is a second direct consumer. Craft already ships embedding-searched styleguide rules as YAML files (per language and category, with rule ids, priorities, patterns, and good/bad examples); mapping that mechanism onto the `styleguide` namespace (§5.4 of the specification) moves the same content into argosy concepts — one concept per rule, `type: Styleguide Rule`, with `language`, `category`, `rule_id`, `priority`, and `pattern` as optional frontmatter and the guidance plus good/bad examples in the body. The existing YAML rule sets seed an argosy's `styleguide/` namespace via a one-time conversion; from then on, Craft's `styleguide` review tool queries the active argosy (embedding similarity over rule descriptions, filtered by `language` and `category`) instead of reading YAML from disk, and users extend the rule set by adding or editing concepts in the local argosy's `styleguide/` namespace — no harness-specific format involved.

How deep that wiring goes — read-only consultation versus full read/write — is a Craft product decision outside the scope of this document.

## 5. Relationship to the Specification

Everything in this document is informative. The specification's requirements (`STR-`, `DOC-`, `SKL-`, `STG-`, `MEM-`, `PROM-`, `IDX-`, `DIST-`, `MUL-`, `QRY-`, `SEC-`, `NFR-`) remain the source of truth for what a conformant argosy and a conformant implementation must do. This document proposes one way to satisfy them; another implementation — in a different language, with a different crate boundary, without an MCP server at all — can be equally conformant.

## 6. Implementation Decisions

- **Default vector store for the index backend.** Shortlist, all satisfying the in-process constraint (an argosy is "a directory," not a service — specification §3.4): `usearch` (smallest footprint, ANN-only), `sqlite-vec` (SQLite-backed, giving durable storage and metadata filtering in one file), and `LanceDB` (embedded columnar store, richest filtering, heaviest dependency). Evaluation criteria: incremental upsert/delete performance (`IDX-10`), metadata-filtered search (`IDX-9`), and dependency weight for consumers who enable the default backend. Current leaning: `sqlite-vec` — one file per argosy fits the "rebuildable derivative next to the markdown" model, and SQL covers the structured-filter half of `IDX-9` without a second dependency.
- **CLI surface.** The `argosy` binary ships the full set from the start: `mcp` (the MCP server), `validate` (bundle conformance checking), `package` (distribution packaging with the `memory/` exclusion), `index` (build/inspect the index for debugging), and `convert styleguide` (one-time YAML rule-set conversion, §4).
- **How far Craft's integration goes at first.** Reading imported argosys only, versus full read/write against the local argosy from day one, changes how much of the library's surface Craft needs immediately.
- **Styleguide YAML conversion.** Settled by the specification: good/bad examples map into the rule body under `## Good` and `## Bad` headings (`STG-6`), and `rule_id` stays optional frontmatter (`STG-5`) — the concept's path is its stable identity. The `convert styleguide` command maps each YAML rule to one concept file named for its rule id (for example, `styleguide/rust/naming/SNAKE-CASE-VARS.md`).

---

*End of document.*
