# argosy

Knowledge book system for coding harnesses. Argosy manages **OKF knowledge
bundles** — directories of markdown concepts with YAML frontmatter — and
serves them to any MCP-compatible harness (OpenCode, Claude Code, editors)
as searchable, referenceable knowledge: `argosy://<name>/<namespace>/<id>`.

A project's argosys live under `.argosy/`:

```
.argosy/
├── default/          the local, writable bundle (argosy init)
├── <name>/           pulled read-only imports (argosy pull)
└── index.db          the derived semantic index (argosy index build)
```

Each bundle holds four reserved namespaces — `document/` (curated prose),
`skill/` (harness skills), `memory/` (session learnings, never packaged),
and `styleguide/` (lintable rules) — plus any producer-defined custom ones.

## Install

```sh
cargo install --path .
```

## Quickstart

```sh
# 1. Create this project's local bundle (named after the directory).
argosy init

# 2. Add knowledge (or let your harness do it via MCP, below).
argosy pull https://github.com/your-org/shared-argosy company-rules

# 3. Build the semantic index.
#    FIRST RUN downloads the embedding model (~90 MB) into the fastembed
#    cache ($FASTEMBED_CACHE or the platform cache dir); later runs are
#    offline.
argosy index build

# 4. Serve the MCP server on stdio.
argosy mcp
```

Wire it into a harness, e.g. OpenCode (`opencode.json`):

```json
{
  "mcp": {
    "servers": {
      "argosy": { "type": "local", "command": ["argosy", "mcp"] }
    }
  }
}
```

The server exposes semantic `search` / `search_rules` tools, `argosy://`
resources, skill listing with trust tiers, and write tools for the local
argosy's `document/`, `memory/`, and `styleguide/` namespaces. Writes
reconcile the index immediately — a concept written through MCP is
searchable in the same session. The embedding model loads lazily: until
the first `search`, startup is instant and works offline. It also serves
a `dream` prompt: a guided memory-consolidation pass (merge, update,
delete, deduplicate) that harnesses can run whenever the local memory
feels redundant.

### Code-intelligence tools

The same server also serves read-mostly **code-intelligence tools** over
the workspace directory it was spawned in, ported from Craft:

| Tool | What it does |
|---|---|
| `outline` | Structural outline of a file or directory (nested symbol tree with signatures and line ranges, or a flat file table). |
| `zoom` | The body of one symbol (or a line range) in a file, with a numbered gutter. |
| `astgrep` | AST search/replace with `$VAR` / `$$$BODY` metavariables — dry-run diffs by default; `apply` writes, with syntax validation and rollback. |
| `conflicts` | Find (and optionally resolve `@ours` / `@theirs` / `@base`) git merge-conflict markers. |
| `inspect` | Quick health check: TODO/FIXME/HACK/XXX scan plus `git status`. |
| `callgraph` | Intra-file call graph: `call_tree`, `callers`, `impact` (blast radius). |
| `repomap` | Token-budgeted, PageRank-ranked map of a repository's definitions. |

`astgrep` (with `rewrite` + `apply`) and `conflicts` (with `resolve`) are
the only ones that write files, and both refuse to write a file that
changed since you last read it through these tools. The tools pull in
~35 tree-sitter grammars; builds that don't need them can drop the
default `code-tools` feature (`--no-default-features` plus the features
you want) for a much faster, smaller compile.

## Reviewer subagent

`argosy agent reviewer <harness>` writes a read-only `reviewer` subagent
definition into the harness's agent directory (`.opencode/agents/`,
`.claude/agents/`, or `.kiro/agents/`). The reviewer reads the code,
grounds findings in the project's styleguide rules via the argosy MCP
tools, and reports prioritized findings (P0–P3) with a final verdict —
adapted from Craft's built-in reviewer. Register the argosy MCP server
with the harness to enable rule grounding.

## Commands

| Command | What it does |
|---|---|
| `argosy init [path]` | Create a bundle (manifest + reserved namespaces). |
| `argosy validate <path>` | Structural + namespace-contract validation with requirement IDs (`--namespace` scopes one namespace). |
| `argosy pull <url> <name>` | `git clone` an external argosy into `.argosy/<name>` (`--global` for the user store). |
| `argosy index build` / `status` / `query` | Build/diff/search the semantic index at `.argosy/index.db`. |
| `argosy package <src> <dest>` | Distributable copy (`--format tar.gz`), integrity sidecar, `memory/` always excluded. |
| `argosy convert styleguide <yaml-dir>` | Import legacy YAML rule sets as styleguide concepts (additive, re-runnable). |
| `argosy agent reviewer <harness>` | Install the read-only `reviewer` subagent definition into a harness (`opencode`, `claude`, `kiro-cli`); `--force` replaces an existing one. |
| `argosy mcp` | Serve the project over MCP on stdio. |

Most commands print machine-readable JSON with `--json` and quiet down
with `--quiet`; exit codes: `0` success, `1` failure, `2` usage.

## Library

Everything above is a thin wrapper over the `argosy` crate
(`Argosy`, `LocalArgosy`, `ProjectContext`, `Index`); custom embedding
and vector-store backends implement the `EmbeddingProvider` /
`VectorStore` traits. See `docs/specification.md` for the format and
requirement IDs.
