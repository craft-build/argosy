# 10 — The MCP Server (`argosy mcp`)

| | |
|---|---|
| Depends on | 09 |
| Creates | `src/mcp.rs`; extends `src/cli.rs` (adds `mcp` subcommand), `Cargo.toml` (feature `mcp`), `tests/mcp.rs` |
| Spec sections | §9, §10, §11 (the server *is* a harness performing the lifecycle), §12.1 (`SEC-1`–`SEC-3`); reference doc §3 (role, tool/resource mapping, transport) |

---

## 1. Context

The MCP server makes argosys usable by any MCP-compatible harness without embedding Rust: it is launched from the project's working directory, discovers the standard argosy set itself (`.argosy/default` local, `.argosy/<name>` checkouts, then the global user store), performs validation/index-reconcile/activation internally (life-cycle §11 steps 1–4), and exposes the result as MCP Tools and Resources (reference doc §3.1). Transport: **stdio default**, HTTP secondary (reference doc §3.3).

The implementation is a translation layer: every tool/resource handler is a thin adapter over `ProjectContext`, `LocalArgosy`, `Index`, and the doc 05 `argosy://` URI support. Any logic that isn't MCP-shaped (serialization, schema, dispatch) belongs in the library — same discipline as doc 09.

**stdio constraint**: on stdio transport, stdout is the protocol channel. All diagnostics go to stderr (use `eprintln!`/`tracing` with a stderr writer — if adding `tracing`, keep it binary-side only, not in the library). A single stray `println!` corrupts the protocol; add a code comment at the top of `mcp.rs` stating this.

## 2. Requirements

### 2.1 Command and features

- `argosy mcp [--transport stdio|http] [--bind 127.0.0.1:PORT]`
  - No argosy-selection flags: the server loads the standard set discovered from its working directory — exactly what `index build` indexes — via `ProjectContext::open_project`: the local bundle at `.argosy/default`, every other checkout in `.argosy/<name>`, then every argosy in the global user store, in that precedence order. (Requiring a `--project-root` pointing at one argosy would defeat the server's purpose: one flag to keep in sync per launch, and imported argosys invisible.)
  - Opens `ProjectContext`, builds the default backend, runs reconcile (so the server answers with a fresh index rather than trusting staleness — §11 step 3), then serves.
- Cargo feature `mcp` (default on) gates `rmcp`; `--no-default-features` library builds stay rmcp-free (doc 00 §4). New dependencies: `rmcp` (latest stable; pin minimal features: `server`, stdio; `transport-streamable-http-server` or the SDK's current HTTP server transport for the secondary mode), `tokio` (rt-multi-thread, macros — binary only).
- Startup failures (invalid argosy, unbuildable index) print a human-readable error to stderr and exit `1` before the transport starts — never serve a half-broken context.

### 2.2 Resources (read-only, addressed by `argosy://` URI — reference doc §3.2)

| Resource | Backing call |
|---|---|
| `argosy://<name>/<namespace>/<concept-id>` — any concept in any active argosy (`QRY-4`; documents, skills, memory, styleguide rules alike) | `ProjectContext::read_uri` |
| `argosy://_argosys` (reserved pseudo-path; document the scheme extension) — list active argosys: name, version, local/imported (`MUL-5`, §9 activation state) | `ProjectContext` accessors |
| `argosy://<name>/_index` — the bundle's root `index.md` where present (OKF §8 progressive-disclosure browsing) | read root `index.md` of the named argosy |

- Unknown argosy/namespace/concept → MCP "resource not found" error, not empty content.
- Resource responses are the concept's **raw markdown-with-frontmatter** (harness authors want the real file, `NFR-3`), with its qualified identity in the response metadata.

### 2.3 Tools (invocable)

Implement exactly this set — each maps 1:1 onto library APIs already built:

| Tool | Backing call | Parameters | Notes |
|---|---|---|---|
| `search` | `Index::search` | `query: string`, `k?: int` (default 8), `namespaces?: string[]`, `argosy?: string`, `tags?: string[]`, `type?: string`, `language?`, `category?` | `QRY-1`–`QRY-3`, `QRY-6`; hits returned with qualified URIs, scores, and metadata |
| `list_skills` | `ProjectContext::list_skills` | — | `QRY-5`; include `argosy`, `description`, and `shadowed` flag (precedence visibility, `MUL-6`) |
| `get_skill` | `ProjectContext::resolve_skill` + read | `name: string` | resolves collisions by precedence (`MUL-7`); returns the skill concept content |
| `search_rules` | `Index::search` with `namespaces=[Styleguide]` + `language`/`category` filter | `query: string`, `language?`, `category?`, `k?` | the review-flow query (`STG-4`, §5.4); semantic match of rules against code text |
| `read_memory` | `ProjectContext::read_uri` | `path: string` (bundle-relative, local argosy) | direct read, spec §10.2 |
| `write_memory` | `LocalArgosy::write_memory` | `path`, `content` (full markdown+frontmatter) | tool, not resource (mutating) |
| `delete_memory` | `LocalArgosy::delete_memory` | `path` | — |
| `write_rule` / `delete_rule` | `LocalArgosy::write_rule` / `delete_rule` | `path`, `content` | local argosy only by construction (`MUL-3`/`MUL-4`); enables user rule extension (§5.4) |
| `write_document` / `delete_document` | `LocalArgosy::write_document` / `delete_document` | `path`, `content` | local argosy only by construction (`MUL-3`/`MUL-4`); curated document authoring/editing (`DOC-1` conformance enforced); overwrite is the edit path, reported as `updated` |
| `promote` | `LocalArgosy::promote_memory` | `source_path`, `target: "document"|"styleguide"`, `new_path`, `description?` | returns source content + drafted concept (the `SEC-5` confirmation hook — the *client* decides whether to confirm; `PROM-5`) |

- Every mutating tool returns the affected `argosy://` URI plus a short machine-readable summary (e.g. `{"written": "argosy://acme/memory/foo", "bytes": N}`).
- **Trust surfacing** (`SEC-1`/`SEC-2`): `list_skills` and `get_skill` include each skill's origin argosy name and its OKF `verified` tier when present (`unverified` when absent) — this is the hook §12.1 asks harnesses to surface; the server exposes the data, the client harness decides policy (`SEC-3` stays a client decision — document this in the tool description strings so downstream LLMs see it).
- Tool descriptions must be written for LLM consumers: one sentence of what it does, one of when to use it, note the read-only nature of imported argosys on mutating tools.

### 2.4 Prompts (MCP `prompts` capability)

Reusable workflows the harness can run with the server's existing tools — no new server logic. The set is static (no `list_changed` notifications); unknown prompt names are protocol-level method-not-found, matching the unknown-tool policy.

| Prompt | Arguments | Body |
|---|---|---|
| `dream` | — (craft's `/dream` is `max_args: 0`) | memory-consolidation pass over the local argosy: discover the local argosy's name from `argosy://_argosys`, enumerate `memory/` via `search` (`namespaces: ["memory"]`, local argosy, high `k`), read each via `read_memory`, then **merge / update / delete / add** via `write_memory` / `delete_memory`; rules keep entries concise, prefer fewer high-quality entries, use absolute `YYYY-MM-DD` dates, fix contradictions at the source, never touch read-only imported argosys, and end with a one-paragraph summary (an explicit no-op is valid) |

- Served as a single user-role `PromptMessage`; descriptions follow the tool-description discipline (what it does, when to reach for it).
- Handlers are pure functions (`prompt_definitions`, `get_prompt_result`) — prompts carry no server state, unlike tools.

### 2.5 Testing (`tests/mcp.rs`)

- Structure handlers as plain async functions over a `McpState { context: ProjectContext, index: Index<…> }` so they are unit-testable without a live transport: construct state over fixture copies + `MockEmbedder`/`MemStore` where possible (the server must be generic over provider/store like doc 06's `Index`, with the CLI's `mcp` verb instantiating the concrete backend).
- One end-to-end rmcp test over an in-process transport (the SDK supports duplex/in-memory client-server pairs): initialize → `list_skills` → `search` → `write_memory` → `read_memory` verifies the write → `promote` → URI resource read of the promoted concept.
- No network in tests; no ONNX (mock embedder); HTTP transport gets only a smoke check that the flag parses and fails gracefully binding a used port.

## 3. Non-Goals

- No auth on the HTTP transport (document it as trusted-network-only; access control is spec §15 future work).
- No notifications/subscriptions for bundle changes (reconcile-on-start is the freshness model; long-lived staleness is acceptable v1 — note it in `--help`).
- No `SEC-4` distribution-confirmation flow — packaging is doc 09's `argosy package`, deliberately not an MCP tool (distribution is a human-driven act).

## 4. Success Criteria

- [ ] `mcp` subcommand wired into the doc 09 clap tree behind the `mcp` feature; `--help` documents working-directory discovery semantics (no argosy-selection flags) and the stdio-default transport.
- [ ] Handler unit tests (fixture state, mock backend): every §2.2 resource and §2.3 tool — including error paths (unknown concept URI, write to a bad path, promote without description to styleguide, query scoped to inactive argosy name).
- [ ] In-process rmcp end-to-end test (§2.4 flow) passes.
- [ ] Trust fields present: `list_skills` output asserts origin argosy + `verified`/`unverified` tier from fixtures (`SEC-2`).
- [ ] Prompt coverage: wire test asserts `list_prompts` returns exactly `dream`, `get_prompt("dream")` returns one user-role message naming the workflow's tools (`search`, `read_memory`, `write_memory`, `delete_memory`), and an unknown prompt name is a protocol error.
- [ ] Imported read-only structural test: no tool accepts an `argosy` parameter for writes — assert at the schema level that write tools have no argosy selector (document the invariant; the type system enforces the rest).
- [ ] `cargo build --no-default-features` and full-feature builds both compile; `fmt`/`clippy`/`cargo test` clean.
- [ ] Manual smoke (note in PR): `argosy mcp` launched over stdio from a fixture project directory (cwd) answers an `initialize` handshake — verified once with a real MCP client or SDK example, result recorded.
