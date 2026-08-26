# Argosy: A Portable Knowledge, Skill, and Memory Layer for Coding Harnesses

**Design Document & Requirements Specification**

| | |
|---|---|
| Status | Draft |
| Version | 0.1.0 |
| Date | 2026-08-24 |
| Depends on | Open Knowledge Format (OKF) v0.2 |
| Reference implementation target | Craft (github.com/craft-build/craft) — informative only; this specification is harness-agnostic |
| Breaking change | Renamed from Folio v0.1.0. This is not a pure rename: it changes the artifact contract — the manifest filename (`folio.md` → `argosy.md`), the manifest `type` value (`Folio Manifest` → `Argosy Manifest`), the manifest version key (`folio_version` → `argosy_version`), and the concept URI scheme (`folio://` → `argosy://`). Bundles, manifests, and URIs conforming to Folio v0.1.0 are not conformant with this version without migration. |

---

## Table of Contents

1. [Introduction](#1-introduction)
2. [Key Design Decisions](#2-key-design-decisions)
3. [Conceptual Model](#3-conceptual-model)
4. [Argosy Structure Requirements](#4-argosy-structure-requirements)
5. [Namespace Specifications](#5-namespace-specifications)
6. [The Memory-to-Document Promotion Pathway](#6-the-memory-to-document-promotion-pathway)
7. [The Embedding and Vector Index Layer](#7-the-embedding-and-vector-index-layer)
8. [Distribution and Packaging](#8-distribution-and-packaging)
9. [Multi-Argosy Composition](#9-multi-argosy-composition)
10. [Retrieval and Query Requirements](#10-retrieval-and-query-requirements)
11. [Argosy Lifecycle](#11-argosy-lifecycle)
12. [Security and Trust Considerations](#12-security-and-trust-considerations)
13. [Non-Functional Requirements](#13-non-functional-requirements)
14. [Conformance Summary](#14-conformance-summary)
15. [Open Questions and Future Work](#15-open-questions-and-future-work)
16. [Appendix A: Illustrative Example](#16-appendix-a-illustrative-example)
17. [Appendix B: Requirement Index](#17-appendix-b-requirement-index)

---

## 1. Introduction

### 1.1 Motivation

A coding harness — an AI agent that reads, writes, and reasons about a codebase — starts every session with limited knowledge of the system it's working in. Today that knowledge is rebuilt from scratch each session, or accumulated in ad hoc, harness-specific mechanisms that don't travel with the code and don't compose across tools.

Argosy defines a single, rebuildable, distributable data layer that closes this gap. An argosy packages what a harness has learned about a project — its documentation, its reusable skills, and its working memory — as plain, version-controllable markdown, indexed for semantic retrieval. Because the markdown is the source of truth and the index is always a rebuildable derivative, an argosy survives being moved between machines, harnesses, and embedding models. Because it's built on the Open Knowledge Format, it doesn't require a proprietary reader: any OKF-aware tool can browse an argosy's documentation even without understanding argosy's own conventions.

### 1.2 Goals

- Define a markdown-based, OKF-conformant format for packaging project documentation, reusable skills, and working memory together.
- Make the format fully rebuildable from source: an index built from an argosy is always reproducible from the argosy's markdown content alone.
- Make argosys distributable independent of the harness, embedding model, or vector database that produced or will consume them.
- Support a project drawing on more than one argosy at a time — its own, plus argosys bundled with its dependencies.
- Specify the format and the required capabilities of its storage/retrieval layer, without prescribing a specific database, embedding model, or harness implementation.

### 1.3 Non-Goals

- Argosy does not define a runtime or execution engine. It formats and locates skills; it does not run them.
- Argosy does not mandate a specific vector database, embedding model, or programming API. §7 specifies required *capabilities*; the technology that provides them is an implementation choice.
- Argosy does not define a harness's user interface, command names, or tool schemas. Where a requirement would touch UX (for example, how a person triggers promotion, §6), this document says so explicitly and stops there.
- Argosy does not replace a harness's native configuration or skill-discovery mechanism. It is designed to coexist as an additional source (§2, decision 2), though a harness may choose to adopt it as its primary mechanism.
- Argosy does not (yet) define cross-argosy linking, access control, or a package registry. These are noted as future work in §15.

### 1.4 Terminology

| Term | Definition |
|---|---|
| **Argosy** | This specification. |
| **An argosy** | A directory tree conformant with this specification: an OKF knowledge bundle (§1.5) organized into namespaces (§3), optionally accompanied by a vector index (§7). The unit of distribution. |
| **Namespace** | One of the top-level subdivisions of an argosy — `document`, `skill`, or `memory` — each with its own semantics (§5). |
| **Concept** | A single markdown-plus-frontmatter document, as defined by OKF §4. The unit of knowledge within a namespace. |
| **Project context** | The scope within which a harness activates one local argosy and any number of imported argosys — typically a single project, but see §3.3. |
| **Local argosy** | The argosy a harness reads from and writes to for its current project context (§3.3). |
| **Imported argosy** | Any other argosy active in the same project context — bundled with a dependency, pulled from a shared location, or otherwise brought in from outside the project. Read-only by default (§9). |
| **Index** | The vector store and its embeddings, derived from an argosy's concepts. Never the source of truth; always rebuildable (§7). |
| **Harness** | The consuming AI coding agent or tool. Used generically throughout; no specific implementation is assumed. |
| **Promotion** | The act of creating a new `document`-namespace concept derived from a `memory`-namespace concept, as the only path by which memory-originated content is distributed (§6). |

### 1.5 Relationship to OKF

An argosy **MUST** be a conformant OKF v0.2 knowledge bundle, as defined by the OKF specification's §11. Every requirement OKF places on a bundle and its concepts applies to an argosy without exception. This document defines additional, argosy-specific requirements on top of that baseline: a reserved manifest file (§4.2), a fixed set of namespace directories (§4.3), and semantics for each namespace (§5).

A consequence of this relationship: **an argosy is a valid OKF bundle, and generic OKF tooling that has never heard of argosy can still open one and browse it correctly.** A tool that only understands OKF will see a normal bundle with a few extra concepts (the manifest) and some namespaced subdirectories it doesn't attach special meaning to — nothing about an argosy's structure violates OKF conformance. The reverse is not true: not every OKF bundle is an argosy, since an argosy adds requirements OKF itself leaves open.

Where this document is silent, OKF's own rules govern — cross-linking (OKF §6), the actor convention (OKF §7), index and log files (OKF §8–9), and so on all apply to an argosy exactly as OKF defines them.

### 1.6 Conformance Language

The keywords **MUST**, **MUST NOT**, **SHOULD**, **SHOULD NOT**, and **MAY** are used as defined by common specification convention (matching OKF's own usage): **MUST**/**MUST NOT** mark hard requirements; **SHOULD**/**SHOULD NOT** mark strong recommendations that may be deviated from with reason; **MAY** marks a genuine option.

Every requirement in this document is additionally tagged with an identifier (for example, `STR-1`) so it can be referenced unambiguously. §17 (Appendix B) indexes all of them.

---

## 2. Key Design Decisions

The rest of this document works out the consequences of a small number of foundational choices. They're collected here so the reasoning is visible in one place rather than scattered across sections.

| # | Decision | Resolution | Rationale |
|---|---|---|---|
| 1 | Scope | Portable and harness-agnostic. | Mirrors OKF's own vendor-neutral posture. Craft is this spec's motivating and reference implementation, not a design constraint — nothing here requires any Craft-specific mechanism. |
| 2 | Skill namespace vs. native skill discovery | Additive, not a replacement. | A harness's existing skill mechanism (however it discovers and loads skills today) and an argosy's `skill` namespace coexist as independent sources. Neither this document nor a harness adopting it is required to retire what already works. |
| 3 | Memory distribution | Never distributed directly. Reachable only through promotion into `document` (§6). | Keeps memory's privacy boundary unconditional — no per-document flag to misconfigure, no partial-export surface to audit. If it's in `memory`, it stays local, full stop. |
| 4 | Source of truth | The markdown bundle, not the index. | Embeddings are coupled to whatever model produced them and are not portable across models. Markdown is diffable, human-readable, and git-native — exactly the properties OKF was designed around. |
| 5 | Storage and embedding technology | Specified as required capabilities, not a named database or model. | Keeps the format specification decoupled from any particular implementation. Also matches the pluggable-provider pattern already common in this space. |
| 6 | Namespace set | Four reserved namespaces (`document`, `skill`, `memory`, `styleguide`); extensible. | OKF itself refuses to fix a taxonomy of concept types. An argosy spec that hard-codes a fixed namespace set forever would be a real limitation; reserving four with defined semantics (the first three for knowledge, instructions, and working notes; `styleguide` for retrievable coding rules) while leaving room for more follows OKF's own precedent. |
| 7 | Multi-argosy support | In scope from the start: a project may have one local argosy and any number of imported argosys active at once. | Follows from decision 3's own premise — if a harness reuses the argosy mechanism for its own memory, every project context already has at least one live, local argosy, distinct from anything it might separately import. |

---

## 3. Conceptual Model

### 3.1 Two Layers

An argosy has exactly two layers, with a strict dependency in one direction:

1. **The bundle** — an OKF-conformant directory tree of markdown concepts, organized into namespaces. This is the source of truth: version-controlled, diffable, human-readable, portable across every embedding model and vector database that will ever exist.
2. **The index** — a vector store holding an embedding for some unit of each concept, built by running an embedding model over the bundle. This is a derived artifact: disposable, rebuildable, and never authoritative for anything the bundle itself says.

A harness **MUST** be able to reconstruct a complete, correct index from the bundle alone (`IDX-1`). Nothing about an argosy's meaning depends on the index existing, matching a particular embedding model, or being up to date — at worst, an out-of-date or absent index degrades retrieval quality, not correctness.

### 3.2 Namespaces at a Glance

| Namespace | Holds | Distributed by default | Primary retrieval mode | Detail |
|---|---|---|---|---|
| `document` | Project documentation — architecture, decisions, references, guides. AI- or human-authored. | Yes | Semantic search | §5.1 |
| `skill` | Reusable, project-specific instructions for the harness to load on demand. | Yes | Direct lookup by name; discoverable by listing or search | §5.2 |
| `memory` | Working notes, learnings, and decisions accumulated across sessions. | No — see §6 | Direct read/write; search is a recommendation, not a requirement | §5.3 |
| `styleguide` | Coding-style rules — per language or language-agnostic — that a harness's review or linting flow consults, and that users can extend by adding concepts. | Yes | Semantic search combined with structured filters (`language`, `category`) | §5.4 |

### 3.3 Local and Imported Argosys

Because a harness reuses the argosy mechanism for its own memory (decision 3, §2), every project context (§1.4) has at least one **local argosy**: the live, read-write argosy its own memory and any documentation or skills authored in that context belong to. A project context may additionally have zero or more **imported argosys** active — for instance, an argosy published alongside a library dependency, giving the harness pre-built documentation and skills for that dependency without needing to derive them itself.

A project context is typically a single project, but this document doesn't require that. It could just as well be a person's broader, cross-project workspace. Nothing in the argosy format itself distinguishes these cases — which scope an argosy is local *to* is a harness activation decision (§11), not a property the format encodes.

This distinction drives the mutability and precedence rules in §9. It is a relationship between an argosy and the project context it's active in, not a property baked into the argosy's own files — the same argosy could be the local argosy for the project that produces it and an imported argosy for every project that depends on it.

### 3.4 What an Argosy Is Not

An argosy is not a running process, a network service, or a database with its own query language. It's a directory. A harness can build one, read one, and rebuild its index for one entirely offline, using nothing more than a markdown parser, an embedding model, and somewhere to put vectors.

---

## 4. Argosy Structure Requirements

### 4.1 Bundle Conformance

- **STR-1**: An argosy **MUST** satisfy OKF v0.2 conformance (OKF §11) at its root.
- **STR-2**: An argosy **MUST** contain exactly one argosy manifest (§4.2) at its bundle root.
- **STR-3**: An argosy **MUST NOT** nest another argosy manifest anywhere below its root. (An argosy does not contain other argosys; see §15 for composite argosys as future work.)

### 4.2 The Argosy Manifest

An argosy's identity — its name, version, and the OKF version it targets — lives in a reserved concept at the bundle root.

- **STR-4**: An argosy **MUST** contain a file named `argosy.md` at its root, and this filename is reserved: it **MUST NOT** be used for an ordinary concept anywhere in the bundle.
- **STR-5**: `argosy.md` **MUST** be a valid OKF concept (frontmatter with a non-empty `type`) whose `type` is `Argosy Manifest`.

The manifest's frontmatter carries the following argosy-specific fields, in addition to any ordinary OKF fields (`description`, `generated`, and so on) a producer chooses to include:

| Field | Requirement | Meaning |
|---|---|---|
| `name` | **MUST** | The argosy's identifying name. Stable across versions; used to reference this argosy from elsewhere (dependency declarations, cross-references in tooling). |
| `argosy_version` | **MUST** | The version of this specific argosy (this bundle's own content), using semantic versioning. Distinct from the Argosy *specification* version this document defines. |
| `okf_version` | **SHOULD** | The OKF specification version this bundle targets (for example, `"0.2"`). |
| `description` | **SHOULD** | A one-line summary of what this argosy covers. |

- **STR-6**: A harness encountering an unrecognized field in `argosy.md` **MUST NOT** reject the argosy, consistent with OKF's own tolerance for unknown frontmatter keys.

### 4.3 Namespace Directories

- **STR-7**: The namespace names `document`, `skill`, `memory`, and `styleguide` **MUST**, when present, appear as top-level directories directly under the argosy root.
- **STR-8**: An argosy **MAY** omit any namespace directory it has no content for. An imported argosy with no working memory of its own, for instance, simply has no `memory/` directory.
- **STR-9**: An imported argosy **SHOULD NOT** contain a `memory/` directory — memory is meaningful only for the argosy designated local to a given project context (§3.3, §9). A harness encountering one anyway **MUST** still treat it as read-only, consistent with all other content in an imported argosy (`MUL-3`).
- **STR-10**: An argosy **MAY** define additional, custom-named top-level directories beyond the four reserved namespaces, for content that doesn't fit `document`, `skill`, `memory`, or `styleguide`. A harness that doesn't recognize a custom namespace **MUST** ignore it rather than reject the argosy, mirroring OKF's tolerance of unknown `type` values.
- **STR-11**: A custom namespace name **MUST NOT** collide with a reserved namespace name (`document`, `skill`, `memory`, `styleguide`) or a reserved filename (`index.md`, `log.md`, `argosy.md`).

### 4.4 Reserved Filenames Summary

| Path | Presence | Meaning |
|---|---|---|
| `argosy.md` | Required, root only | Argosy manifest (§4.2) |
| `index.md` | Optional, any directory | OKF progressive-disclosure listing (OKF §8) |
| `log.md` | Optional, any directory | OKF change history (OKF §9) |
| `document/` | Optional | Document namespace (§5.1) |
| `skill/` | Optional | Skill namespace (§5.2) |
| `memory/` | Optional; local argosys only by convention (`STR-9`) | Memory namespace (§5.3) |
| `styleguide/` | Optional | Styleguide namespace (§5.4) |
| `<custom>/` | Optional | Producer-defined extension namespace (`STR-10`) |

---

## 5. Namespace Specifications

### 5.1 Document Namespace

The `document` namespace is the most direct application of OKF: it holds project documentation as ordinary OKF concepts, with no additional argosy-specific requirements beyond OKF conformance itself.

- **DOC-1**: Every concept under `document/` **MUST** satisfy OKF concept conformance (a `type` field is the only hard requirement).
- **DOC-2**: An argosy producer **SHOULD** use OKF's `description` and `tags` fields on document concepts, since these drive both human browsing and retrieval quality (§10.1).
- **DOC-3**: Whether a document concept originated from a person or an AI agent **SHOULD** be recorded using OKF's existing `generated.by` actor convention — `human:<id>` or `<producer>/<version>` as appropriate. No argosy-specific field is needed for this distinction; OKF already provides it.

`document` places no constraint on `type` values beyond what OKF itself imposes — architecture overviews, runbooks, API references, and design records are all equally valid, exactly as they would be in a standalone OKF bundle.

### 5.2 Skill Namespace

A `skill`-namespace concept is a set of instructions a harness can load into context on demand. Because skills are meant to be portable across harnesses with their own, independent native skill mechanisms (decision 2, §2), this namespace carries more required structure than `document`, so that any harness's loader has a reliable, minimal contract to map onto.

- **SKL-1**: A skill **MUST** be represented as either (a) a single concept file directly under `skill/`, or (b) a subdirectory under `skill/` containing an entry-point concept plus any supporting materials the skill needs.
- **SKL-2**: For a directory-form skill (`SKL-1`b), the entry-point concept file **MUST** share its base name with the containing directory (for example, `skill/deploy/deploy.md` for the skill directory `skill/deploy/`). This lets a harness locate the entry point without a second reserved filename, reusing OKF's own Concept ID convention.
- **SKL-3**: A skill's entry-point concept **MUST** have `type: Skill`.
- **SKL-4**: A skill's entry-point concept **MUST** set `description` (upgraded here from OKF's normal recommendation to a hard requirement) — a natural-language statement of what the skill does and when a harness should reach for it. This is the field a harness's routing or listing logic depends on most.
- **SKL-5**: A skill's entry-point concept body **MUST** contain the instructions the harness follows when the skill is loaded, in ordinary markdown.
- **SKL-6**: Supporting materials for a directory-form skill (reference scripts, templates, examples) **SHOULD** live under a `references/` subdirectory of the skill's own directory, following OKF's `references/` convention.
- **SKL-7**: A skill that needs to perform a specific, sanctioned computation **MAY** express it as an OKF Attested Computation concept (OKF §10) alongside the skill, linked from the skill's body, rather than argosy defining its own execution-safety mechanism. This is an option, not a requirement — most skills are plain instructions with nothing to attest.

A harness **MAY** treat argosy-sourced skills identically to skills from its own native discovery mechanism once loaded (decision 2, §2) — argosy doesn't require they be presented differently, only that they be structured predictably enough to load at all.

### 5.3 Memory Namespace

The `memory` namespace is deliberately the least constrained. It's a working scratchpad, often written informally and quickly, by a harness on its own initiative as much as by a person.

- **MEM-1**: Every concept under `memory/` **MUST** satisfy OKF concept conformance (a `type` field is the only hard requirement).
- **MEM-2**: Beyond `MEM-1`, this document imposes no required frontmatter fields or body structure on memory concepts.
- **MEM-3**: A memory concept **MUST NOT** be included when an argosy is packaged for distribution (§8.3), regardless of any frontmatter it carries, except by way of promotion (§6).
- **MEM-4**: A harness **MAY** organize `memory/` into subdirectories at its own discretion (for example, by topic or by session) — this document defines no required internal structure.

### 5.4 Styleguide Namespace

The `styleguide` namespace holds coding-style rules as first-class, retrievable concepts. Its motivating consumer is a harness's code-review or linting flow, which retrieves rules by embedding similarity against the code or change under review, filtered by language and category — and, because rules are ordinary concepts in the (writable) local argosy, users can extend or override a rule set dynamically by adding or editing concepts, with no harness-specific rule format or config file involved.

- **STG-1**: Every concept under `styleguide/` **MUST** satisfy OKF concept conformance (a `type` field is the only hard requirement).
- **STG-2**: A styleguide rule concept **MUST** have `type: Styleguide Rule`.
- **STG-3**: A styleguide rule concept **MUST** set `description` — a natural-language statement of the rule. This is the primary text a harness embeds and matches against code under review, so it must be self-contained.
- **STG-4**: A styleguide rule concept **SHOULD** set `language` (an identifier such as `rust`, `python`, or `general` for language-agnostic rules) and `category` (for example, `naming`, `error-handling`, `organization`), since these drive structured filtering alongside semantic search (`QRY-3`).
- **STG-5**: A styleguide rule concept **MAY** carry additional structured fields — for instance `rule_id` (a producer's stable identifier), `priority` (for example, `error`/`warn`/`info`), or `pattern` (a regular expression) — which consumers **MUST** treat as optional metadata, not as a required linting contract.
- **STG-6**: A rule concept's body **SHOULD** contain the rule's guidance in ordinary markdown, and **MAY** illustrate it with contrasting good and bad examples, conventionally under `## Good` and `## Bad` headings so converters have a deterministic mapping. Machine-checkable enforcement is not this namespace's job; it conveys guidance to a reviewer, whether human or AI.
- **STG-7**: A rule set **MAY** be organized into subdirectories by language and category at the producer's discretion (for example, `styleguide/rust/naming/`) — this document defines no required internal structure.
- **STG-8**: Where multiple active argosys supply rules for the same language and category, the harness's multi-argosy precedence rules (§9.4) apply: imported rule sets combine with, rather than replace, rules in the local argosy, and the local argosy takes precedence on direct conflict.

### 5.5 Namespace Summary Table

| Requirement class | `document` | `skill` | `memory` | `styleguide` |
|---|---|---|---|---|
| OKF conformance | Required | Required | Required | Required |
| Required `type` value | None (open) | `Skill` | None (open) | `Styleguide Rule` |
| Required `description` | Recommended | **Required** | Not required | **Required** |
| Structural freedom | High | Low (entry-point contract) | Highest | Medium (optional structured fields) |
| Distributed by default | Yes | Yes | No | Yes |

---

## 6. The Memory-to-Document Promotion Pathway

### 6.1 Rationale

Decision 3 (§2) makes memory's non-distribution unconditional at the namespace level. That would be a dead end if there were no way to move something a harness or person learned into memory out into something worth sharing — so promotion is the one, explicit, always-available path for that. Promotion targets the distributed namespaces: `document/` by default, and `styleguide/` where what matured is a coding rule rather than prose documentation (for instance, a note about a recurring naming mistake promoted into a `Styleguide Rule` concept).

### 6.2 Semantics

- **PROM-1**: A harness **MAY** create a new concept in the local argosy's `document/` or `styleguide/` namespace whose content is derived from an existing `memory/` namespace concept. This action is promotion.
- **PROM-2**: Promotion **MUST** result in a new, independent concept under the target namespace. It **MUST NOT** be implemented by relocating the memory concept's file, since content moving from an informal working note to a shared concept is expected to be reviewed and rewritten for an external reader, not mechanically copied.
- **PROM-3**: The originating memory concept **MAY** be retained, deleted, or annotated after promotion, at the harness's or user's discretion. Its continued existence in `memory/` **MUST NOT** be treated as a distribution opt-in for that concept — `MEM-3` still applies to it unchanged.
- **PROM-4**: A promoted concept **SHOULD** record its origin using OKF's existing `sources` field — an entry whose `resource` names the originating memory concept's path within the same argosy. This is provenance, not a distribution guarantee: OKF explicitly permits a `sources` entry to name material a consumer of the promoted concept cannot necessarily follow, so citing a `memory/` path this way is consistent with the format even though the memory concept itself never travels. A promoted styleguide rule **SHOULD** additionally satisfy the namespace contract of §5.4 (`STG-1`–`STG-3`) like any other rule concept.

### 6.3 What This Document Doesn't Specify

- **PROM-5**: The trigger for promotion — an explicit user command, an agent-suggested action requiring confirmation, or something else — is a harness UX decision and is out of scope for this specification, consistent with §1.3.
- Recommendations on *when* promotion should require human review appear in §12.2, since promotion is also where a privacy boundary gets crossed.

---

## 7. The Embedding and Vector Index Layer

### 7.1 Relationship to the Bundle

As established in §3.1, the index is strictly derived. This section specifies what any implementation of the index — regardless of database or embedding model — is required to do.

- **IDX-1**: An index **MUST** be fully reconstructible from an argosy's bundle content alone. No information required to rebuild a complete, correct index may exist only outside the bundle.
- **IDX-2**: An argosy distributed as markdown-only (no packaged embeddings, §7.6) **MUST** still be fully usable — a harness with no prior index for it builds one before first use.

### 7.2 Embedding Unit and Traceability

This document does not mandate a specific chunking strategy — whether a harness embeds one vector per concept or splits long concepts into multiple passages is an implementation choice.

- **IDX-3**: Whatever unit is embedded, the index **MUST** record enough to trace any retrieved unit back to its source concept (its OKF Concept ID) unambiguously.
- **IDX-4**: If a concept is split into multiple embedded units, the index **SHOULD** record each unit's position or offset within the source concept, so a harness can present retrieved content with its place in the original document.

### 7.3 Embedding Provider Requirements

- **IDX-5**: The index **MUST** record the identity of the embedding model or provider (name and version, at minimum) used to produce its current embeddings.
- **IDX-6**: This document does not require a specific embedding model, whether local or remote. It requires only that whatever model is used, its identity is recorded per `IDX-5`, so mismatches can be detected (`IDX-12`).

### 7.4 Vector Store Capability Requirements

Independent of the specific database, an index **MUST** support:

- **IDX-7**: Similarity search over embedded content, returning ranked results.
- **IDX-8**: Scoping a search to one or more namespaces (for example, "search only `document`," or "search `document` and `skill` together but not `memory`").
- **IDX-9**: Filtering or combining search with the structured metadata OKF frontmatter already provides — `tags`, `type`, `status`, and so on — so semantic and structured filtering can be used together.
- **IDX-10**: Incremental maintenance: adding, updating, or removing the embedding(s) for a single concept without requiring a full-bundle rebuild.

### 7.5 Rebuild and Staleness

- **IDX-11**: A harness **SHOULD** detect when a concept's content has changed since it was last embedded (for example, via a content hash) and treat its existing embedding as stale.
- **IDX-12**: A harness **SHOULD** detect when the recorded embedding-model identity (`IDX-5`) no longer matches the model it would use to embed new content, and treat the entire index as stale in that case — vectors from different models are not comparable, so a partial rebuild across two models is not a safe substitute for a full one.
- **IDX-13**: A harness **MUST NOT** silently mix vectors produced by different embedding models within results returned for a single query.

### 7.6 Precomputed Embeddings in Distribution

- **IDX-14**: An argosy **MAY** be distributed with a precomputed index or embedding cache alongside its markdown, as a performance optimization.
- **IDX-15**: A harness **MUST NOT** use a distributed, precomputed embedding cache unless the embedding-model identity it records (`IDX-5`) matches the harness's own embedding model. On any mismatch, `IDX-12` applies and the harness rebuilds.
- **IDX-16**: A precomputed embedding cache, where present, is itself a derived artifact (§3.1) — its presence or absence **MUST NOT** change what the bundle means, only how quickly a harness can start using it.

---

## 8. Distribution and Packaging

### 8.1 Distribution Artifact

- **DIST-1**: The markdown bundle (argosy manifest, namespace directories, and their concepts) is the distributable artifact. A precomputed index (§7.6) is an optional addition to it, never a substitute for it.

### 8.2 Distribution Mechanisms

- **DIST-2**: An argosy **MAY** be distributed by any of the mechanisms OKF itself defines for a bundle: a git repository, a tarball or zip archive, or a subdirectory within a larger repository. This document does not add or restrict distribution mechanisms beyond what OKF already allows.

### 8.3 Exclusions

- **DIST-3**: A packaged or exported argosy **MUST NOT** include the contents of `memory/`, per `MEM-3`, regardless of the distribution mechanism used.
- **DIST-4**: A tool that packages an argosy for distribution **SHOULD** warn if a `memory/` directory is present at the point of packaging, as a safeguard against a packaging tool that doesn't itself enforce `DIST-3`.

### 8.4 Identity and Versioning

Three independent version numbers can describe a given argosy at rest, and conflating them is a common source of confusion, so this document keeps them explicit:

| Version | Declared in | Meaning |
|---|---|---|
| Argosy specification version | This document's own header | Which version of *this specification* an argosy's structural requirements were written against. |
| OKF version | `argosy.md`'s `okf_version` field | Which version of OKF the bundle's concepts were written against. |
| Argosy package version | `argosy.md`'s `argosy_version` field | This specific argosy's own content version (semantic versioning), incremented as its documentation, skills, or structure change. |

- **DIST-5**: An argosy consumer (for instance, a dependency-management tool bringing in an imported argosy) **SHOULD** treat `argosy_version` as the version to reason about for compatibility and update purposes — it is the one that changes as the argosy's actual content changes.

### 8.5 Integrity

- **DIST-6**: A harness **SHOULD** be able to detect whether an argosy's content has changed since it was last read (for example, by comparing a content hash of the bundle, or of individual concepts) independent of whether `argosy_version` was bumped, since a version bump is a producer's responsibility and cannot be relied on alone to signal every change.

---

## 9. Multi-Argosy Composition

### 9.1 Activation

- **MUL-1**: A project context **MUST** have at most one argosy designated local (§3.3) at a time.
- **MUL-2**: A project context **MAY** have any number of imported argosys active alongside the local one.

### 9.2 Mutability

- **MUL-3**: A harness **MUST NOT** write to any namespace of an imported argosy. All writes — new memory entries, promoted documents, newly authored skills — go to the local argosy.
- **MUL-4**: The local argosy **MUST** be writable by the harness across all namespaces it uses.

### 9.3 Identity Across Argosys

- **MUL-5**: A concept's full identity across an active multi-argosy context **MUST** be qualified by which argosy it came from (`argosy.md`'s `name`, §4.2) in addition to its namespace and OKF Concept ID. Two different argosys placing a concept at the same path (for example, both defining `document/architecture.md`) are not a conflict at the identity level — they are two distinct concepts that happen to share a path within their own, separate bundles.

### 9.4 Precedence

Namespacing by argosy (`MUL-5`) resolves identity, but a harness still needs a rule for aggregate operations that merge across argosys — listing all available skills, for instance, where two argosys might offer a skill with the same name.

- **MUL-6**: A harness performing an aggregate operation across multiple active argosys **MUST** apply a deterministic, inspectable precedence rule when a naming collision occurs. This document does not mandate the specific rule.
- **MUL-7**: Where a harness does define a default precedence rule, the local argosy **SHOULD** take precedence over any imported argosy.

---

## 10. Retrieval and Query Requirements

This section specifies what a harness must be able to ask of an active argosy (or set of argosys). It deliberately stops short of specifying how — no function signatures, tool schemas, or query languages appear here, per §1.3.

### 10.1 General Requirements

- **QRY-1**: A harness **MUST** be able to perform a semantic similarity search against one or more active argosys, given a natural-language query, and receive ranked results.
- **QRY-2**: A harness **MUST** be able to scope a query to a specific namespace, a specific argosy, or a combination of both.
- **QRY-3**: A harness **MUST** be able to combine semantic search with structured filtering on OKF frontmatter fields (`tags`, `type`, `status`), per `IDX-9`.
- **QRY-4**: A harness **MUST** be able to retrieve a specific, known concept directly by its qualified identity (argosy, namespace, Concept ID) without going through semantic search — a direct lookup, not just a fuzzy one.

### 10.2 Per-Namespace Retrieval Behavior

| Namespace | Required retrieval mode | Recommended retrieval mode |
|---|---|---|
| `document` | Semantic search (`QRY-1`), direct lookup (`QRY-4`) | — |
| `skill` | Direct lookup by name (`QRY-4`) | Listing all available skills with their descriptions; semantic search over skill descriptions, for a harness to find a relevant skill without knowing its exact name |
| `memory` | Direct read, write, and delete access to individual concepts | Semantic search over memory content, for recall of relevant past learnings |

- **QRY-5**: A harness **SHOULD** support listing every skill available across all active argosys, with each skill's `description`, as a lower-cost alternative to semantic search when a harness (or a person) is browsing what's available.

### 10.3 Cross-Argosy Query Behavior

- **QRY-6**: A query not explicitly scoped to a single argosy (`QRY-2`) **MUST** search across all active argosys and return results identifiable by which argosy they came from (`MUL-5`).
- **QRY-7**: Ranking results across multiple argosys **SHOULD** be based on retrieval relevance (similarity score) alone, not argosy precedence — precedence (`MUL-6`, `MUL-7`) governs collisions in aggregate, name-keyed operations, not the ordering of ranked search results.

---

## 11. Argosy Lifecycle

This section is informative: it walks through the states an argosy moves through in a harness's use of it, tying together requirements defined elsewhere. It does not introduce new requirements of its own.

1. **Discover.** A harness locates an argosy — the local one for its current project context, or an imported one brought in some other way (a dependency, a shared location).
2. **Validate.** The harness confirms OKF conformance (`STR-1`) and the presence of a valid manifest (`STR-4`, `STR-5`). An argosy that fails validation is not activated; per OKF's own permissive conformance rules, a harness tolerates unknown fields and broken links rather than rejecting the bundle over them.
3. **Build or reconcile the index.** If no index exists, or an existing one is stale (`IDX-11`, `IDX-12`), the harness builds or rebuilds it from the bundle (`IDX-1`).
4. **Activate.** The argosy becomes queryable (§10) and, if it is the local argosy, writable (`MUL-4`).
5. **Use.** Over a session, the harness reads via retrieval, writes new memory concepts, and may promote memory into documentation (§6) — all against the local argosy.
6. **Update.** As the bundle changes (new concepts, edits), the index is incrementally reconciled (`IDX-10`) rather than fully rebuilt, except where a full rebuild is required (`IDX-12`).
7. **Deactivate.** An argosy drops out of a project context (a session ends, a dependency is removed) without requiring any action on the bundle itself — deactivation is purely a change in what's currently queryable.

---

## 12. Security and Trust Considerations

### 12.1 Imported Content Is Untrusted Input

An argosy's content can shape a harness's behavior directly — most sharply in the `skill` namespace, where a concept's body is a set of instructions the harness may follow. An imported argosy (§3.3) did not necessarily come from the project's own author, and its `skill` and `document` content should be treated with the same caution a harness applies to any other externally-sourced content it reads.

- **SEC-1**: A harness **SHOULD** treat concepts from an imported argosy as untrusted input, not as ground truth, in the same way it treats output from a web fetch or an external tool call.
- **SEC-2**: A harness **SHOULD** surface an imported skill's OKF trust tier (unverified, machine-confirmed, or human-reviewed, derived from the `verified` field) where it presents that skill, so a person can judge whether to let the harness load and follow it.
- **SEC-3**: A harness **MAY** require explicit confirmation before loading a skill from an imported argosy that carries no `verified` entry at all, since an unreviewed set of instructions from outside the project carries more risk than an unreviewed document meant only to be read.

### 12.2 Promotion Is a Trust-Boundary Crossing

Promotion (§6) is the one path by which content moves from a namespace that is never distributed (`memory`) to one that is (`document`). That makes it the natural point to apply scrutiny.

- **SEC-4**: A harness **SHOULD** require explicit human confirmation before a promoted document concept is included in anything actually distributed externally (packaged, published, or pushed to a shared location) — not necessarily before the promotion itself, but before that promoted content leaves the project.
- **SEC-5**: A harness performing promotion **SHOULD** surface the memory concept's content to the person confirming it, rather than promoting silently, since memory is written with the assumption that it stays local (`MEM-3`) and may contain informal or sensitive detail not intended for an external reader.

### 12.3 What This Document Does Not Solve

Access control for argosys shared within a team or organization (as opposed to fully public distribution), and any form of content encryption or signing, are out of scope for this version — see §15.

---

## 13. Non-Functional Requirements

- **NFR-1** (Portability): An argosy **MUST** remain fully valid and usable after being copied between machines, operating systems, and harness implementations, with no dependency on any single embedding model or vector database (`IDX-1`, `IDX-6`).
- **NFR-2** (Diffability): Because an argosy's source of truth is markdown (`DIST-1`), a meaningful content change **SHOULD** be visible as a meaningful diff under ordinary version control, with no opaque or binary intermediate representation required to inspect a change.
- **NFR-3** (Human-readability): A person **MUST** be able to read any concept in an argosy, in any namespace, without specialized tooling — a text editor and knowledge of OKF's frontmatter convention is sufficient, matching OKF's own design intent.
- **NFR-4** (Incremental performance): Rebuilding or reconciling an index (§7.5) **SHOULD** scale with the amount of content that actually changed, not with the total size of the argosy, except in the full-rebuild case required by `IDX-12`.
- **NFR-5** (Backward tolerance): A harness **MUST** tolerate argosys that declare an older `okf_version` or `argosy_version` than the one it was built against, on a best-effort basis, consistent with OKF's own guidance not to refuse a bundle solely because of its declared version.

---

## 14. Conformance Summary

An argosy is conformant with this specification (Argosy v0.1) if:

1. It satisfies OKF v0.2 conformance in full.
2. It contains exactly one `argosy.md` manifest at its root, with `type: Argosy Manifest` and the required fields listed in §4.2.
3. Its namespace directories, where present, are named and located as required by §4.3.
4. Every concept under `skill/` satisfies the entry-point contract in §5.2 (`SKL-1` through `SKL-5`).
5. Every concept under `styleguide/` satisfies the rule contract in §5.4 (`STG-1` through `STG-3`).
6. No `memory/` content appears in anything distributed from it, except where it has been promoted into `document/` per §6.

A harness's *implementation* is conformant with this specification if it satisfies the capability requirements of §7 (index), §9 (multi-argosy), and §10 (retrieval) — independent of which specific database, embedding model, or programming language it uses to do so.

---

## 15. Open Questions and Future Work

The following are deliberately out of scope for this version, in the same spirit OKF itself defers work rather than rushing an under-specified answer:

- **Cross-argosy linking.** OKF links resolve within a single bundle. This document defines no addressing scheme for a concept in one argosy to link directly to a concept in another; for now, such references are prose, not machine-followable links.
- **Access control.** Argosys shared within a team but not fully public — readable by some harness users and not others — are not addressed here.
- **A package registry or dependency-resolution protocol.** §8.2 covers distribution mechanisms OKF already allows; a discovery/versioning protocol analogous to a package manager is a natural extension but a separate piece of work.
- **A canonical chunking algorithm.** §7.2 deliberately leaves embedding granularity to implementations. A future revision could recommend (not require) a default, purely for cross-implementation consistency.
- **Nested or composite argosys.** `STR-3` currently forbids an argosy containing another. Whether that should change (an argosy that bundles others as a convenience) is left open.

---

## 16. Appendix A: Illustrative Example

This walks through a hypothetical argosy for a project called `acme-billing`, describing its shape in prose rather than as literal file contents.

Its manifest, `argosy.md`, declares `name: acme-billing`, a `argosy_version`, and `okf_version: "0.2"`.

Its `document/` namespace includes a top-level `architecture.md` (`type: Architecture Overview`) describing the payment flow, and a `document/decisions/` subdirectory holding individual decision records, each a small concept of `type: Decision Record`.

Its `skill/` namespace includes a single-file skill, `skill/reconcile-ledger.md` (`type: Skill`, with a `description` explaining it reconciles the internal ledger against the payment processor's records), and a directory-form skill, `skill/rotate-api-keys/`, whose entry point is `skill/rotate-api-keys/rotate-api-keys.md` alongside a `references/` subdirectory holding a supporting checklist concept.

Its `styleguide/` namespace carries the project's coding rules as retrievable concepts — for instance `styleguide/rust/naming/snake-case-vars.md` (`type: Styleguide Rule`, `language: rust`, `category: naming`, with a `description` stating the rule and a body contrasting good and bad examples) — which the harness's review flow matches against code under review, and which a user can extend by dropping in another concept alongside it.

As the project's local argosy, it also has a `memory/` namespace — informal notes like `memory/gotchas.md`, accumulated over sessions, that stay local. When one of those notes matures into something worth sharing (say, a subtle rate-limiting behavior worth documenting properly), it's promoted: a new concept appears under `document/`, citing the original memory note as a source, and the note itself is left in place in `memory/`, still excluded from anything distributed.

| Path | Namespace | Type | Notes |
|---|---|---|---|
| `argosy.md` | — | Argosy Manifest | Root manifest |
| `document/architecture.md` | document | Architecture Overview | |
| `document/decisions/2026-05-caching.md` | document | Decision Record | |
| `document/rate-limit-behavior.md` | document | Reference | Promoted from `memory/gotchas.md` |
| `skill/reconcile-ledger.md` | skill | Skill | Single-file skill |
| `skill/rotate-api-keys/rotate-api-keys.md` | skill | Skill | Entry point of a directory-form skill |
| `skill/rotate-api-keys/references/checklist.md` | skill | Reference | Supporting material |
| `styleguide/rust/naming/snake-case-vars.md` | styleguide | Styleguide Rule | `language: rust`, `category: naming` |
| `memory/gotchas.md` | memory | (open) | Never distributed |

---

## 17. Appendix B: Requirement Index

| ID | Section | Summary |
|---|---|---|
| STR-1 | 4.1 | Argosy must satisfy OKF v0.2 conformance |
| STR-2 | 4.1 | Exactly one manifest at root |
| STR-3 | 4.1 | No nested argosy manifests |
| STR-4 | 4.2 | `argosy.md` required, reserved filename |
| STR-5 | 4.2 | Manifest is `type: Argosy Manifest` |
| STR-6 | 4.2 | Unknown manifest fields tolerated |
| STR-7 | 4.3 | Namespace dirs are top-level |
| STR-8 | 4.3 | Namespace dirs may be omitted |
| STR-9 | 4.3 | Imported argosys shouldn't carry `memory/` |
| STR-10 | 4.3 | Custom namespaces allowed |
| STR-11 | 4.3 | Custom namespace names can't collide with reserved names |
| DOC-1 | 5.1 | Document concepts satisfy OKF conformance |
| DOC-2 | 5.1 | `description`/`tags` recommended |
| DOC-3 | 5.1 | AI vs. human authorship via `generated.by` |
| SKL-1 | 5.2 | Skill is single file or entry-point directory |
| SKL-2 | 5.2 | Entry point shares directory's base name |
| SKL-3 | 5.2 | Entry point is `type: Skill` |
| SKL-4 | 5.2 | `description` required on skills |
| SKL-5 | 5.2 | Body holds the skill's instructions |
| SKL-6 | 5.2 | Supporting material under `references/` |
| SKL-7 | 5.2 | Attested Computation optional for skills that need it |
| MEM-1 | 5.3 | Memory concepts satisfy OKF conformance |
| MEM-2 | 5.3 | No further required structure |
| MEM-3 | 5.3 | Never included in distribution except via promotion |
| MEM-4 | 5.3 | Free internal organization |
| STG-1 | 5.4 | Styleguide concepts satisfy OKF conformance |
| STG-2 | 5.4 | Rule concepts are `type: Styleguide Rule` |
| STG-3 | 5.4 | `description` required on rules |
| STG-4 | 5.4 | `language`/`category` recommended for filtering |
| STG-5 | 5.4 | Optional structured fields (`rule_id`, `priority`, `pattern`) |
| STG-6 | 5.4 | Body holds guidance, optionally with `## Good`/`## Bad` examples |
| STG-7 | 5.4 | Free internal organization |
| STG-8 | 5.4 | Imported rule sets combine with local precedence |
| PROM-1 | 6.2 | Promotion creates a new `document/` or `styleguide/` concept |
| PROM-2 | 6.2 | Promotion copies, never relocates |
| PROM-3 | 6.2 | Source memory concept's fate is discretionary |
| PROM-4 | 6.2 | Provenance recorded via `sources`; promoted rules satisfy §5.4 |
| PROM-5 | 6.3 | Promotion's trigger is a harness UX decision |
| IDX-1 | 7.1 | Index fully reconstructible from bundle |
| IDX-2 | 7.1 | Markdown-only argosys are fully usable |
| IDX-3 | 7.2 | Retrieved units trace back to source concept |
| IDX-4 | 7.2 | Chunk position recorded if applicable |
| IDX-5 | 7.3 | Embedding model identity recorded |
| IDX-6 | 7.3 | No specific model mandated |
| IDX-7 | 7.4 | Similarity search required |
| IDX-8 | 7.4 | Namespace-scoped search required |
| IDX-9 | 7.4 | Metadata filtering combinable with search |
| IDX-10 | 7.4 | Incremental upsert/delete required |
| IDX-11 | 7.5 | Content staleness detection |
| IDX-12 | 7.5 | Model-mismatch triggers full rebuild |
| IDX-13 | 7.5 | No mixing vectors from different models |
| IDX-14 | 7.6 | Precomputed embeddings may be distributed |
| IDX-15 | 7.6 | Precomputed cache used only on model match |
| IDX-16 | 7.6 | Cache is optional, never authoritative |
| DIST-1 | 8.1 | Markdown bundle is the distributable artifact |
| DIST-2 | 8.2 | Distribution mechanisms inherited from OKF |
| DIST-3 | 8.3 | Memory excluded from packaging |
| DIST-4 | 8.3 | Packaging tools should warn on stray `memory/` |
| DIST-5 | 8.4 | `argosy_version` is the compatibility-relevant version |
| DIST-6 | 8.5 | Content-hash change detection recommended |
| MUL-1 | 9.1 | At most one local argosy per project context |
| MUL-2 | 9.1 | Any number of imported argosys |
| MUL-3 | 9.2 | No writes to imported argosys |
| MUL-4 | 9.2 | Local argosy fully writable |
| MUL-5 | 9.3 | Identity qualified by argosy |
| MUL-6 | 9.4 | Deterministic precedence rule required |
| MUL-7 | 9.4 | Local argosy precedence recommended by default |
| QRY-1 | 10.1 | Semantic search required |
| QRY-2 | 10.1 | Scoping by namespace and/or argosy |
| QRY-3 | 10.1 | Semantic + structured filtering combinable |
| QRY-4 | 10.1 | Direct lookup by qualified identity |
| QRY-5 | 10.2 | Listing all available skills recommended |
| QRY-6 | 10.3 | Unscoped queries search all active argosys |
| QRY-7 | 10.3 | Cross-argosy ranking by relevance, not precedence |
| SEC-1 | 12.1 | Imported content treated as untrusted |
| SEC-2 | 12.1 | Trust tier surfaced for imported skills |
| SEC-3 | 12.1 | Confirmation may be required for unverified imported skills |
| SEC-4 | 12.2 | Human confirmation before external distribution of promoted content |
| SEC-5 | 12.2 | Memory content surfaced during promotion confirmation |
| NFR-1 | 13 | Portability across machines/harnesses/models |
| NFR-2 | 13 | Diffability |
| NFR-3 | 13 | Human-readability without tooling |
| NFR-4 | 13 | Incremental rebuild performance |
| NFR-5 | 13 | Backward version tolerance |

---

*End of document.*
