//! Prompts: reusable workflows served as `prompts/list` / `prompts/get`.
//! Each is a curation workflow — every step names a tool this server
//! already exposes, so any MCP harness can run it without special client
//! support.

use rmcp::model::{GetPromptResult, Prompt, PromptMessage, Role};

/// The `dream` prompt body: a memory-consolidation pass over the local
/// argosy, adapted from craft's `/dream`.
pub const DREAM_PROMPT: &str = r#"# Dream: Memory Consolidation

Review the local argosy's memory and the recent conversation, then consolidate memory so it stays useful and current. This is a curation pass, not a work pass.

## Steps

0. Every tool call below takes `cwd` — pass the project's absolute root directory on each call (argosys live outside the project tree, keyed by that root).
1. Read the `argosy://_argosys` resource and note the argosy with `"kind": "local"` — that is the only writable argosy, and the scope of this pass. Resources resolve against the directory the server was spawned in, so this workflow assumes the server runs in the project it curates.
2. Enumerate its memory: call `search` with a broad query (e.g. "session notes, decisions, gotchas, learnings"), `namespaces: ["memory"]`, `argosy` set to the local argosy's name, and a high `k` (e.g. 200). Collect the hits' `concept_id` paths.
3. Read each entry with `read_memory` using its path.
4. Decide what to do with each entry:
   - **Merge**: if two entries cover the same topic, combine them into one and delete the redundant copy.
   - **Update**: if an entry is outdated or incomplete based on the recent conversation, rewrite it with current information.
   - **Delete**: if an entry is stale, wrong, or no longer relevant, delete it.
   - **Add**: if the recent conversation surfaced a non-obvious gotcha, decision, or pattern that is NOT yet in memory, add it as a new concise entry.
5. Apply all changes with `write_memory` (full markdown with frontmatter — keep the entry's valid frontmatter; a missing or empty `type` is auto-filled as `type: Memory`) and `delete_memory`.

## Rules

- Only the local argosy is writable. Imported argosys are read-only — never try to change them.
- Keep entries concise. Each one should justify its existence.
- Prefer fewer, higher-quality entries over many small ones.
- Do not duplicate information that is obvious from the code or README.
- Do not remove entries that are still relevant, even if old.
- Convert relative dates ("yesterday", "last week") to absolute `YYYY-MM-DD` so entries stay interpretable as time passes.
- If a memory contradicts what the recent conversation revealed, fix or delete it at the source rather than leaving both versions.
- A merge is only done once the merged entry is written and the redundant copies are deleted in the same pass.
- Report a one-paragraph summary of what you consolidated at the end. If nothing changed and memory is already tight, say so explicitly — a no-op is a valid outcome."#;

/// The `dream` prompt's one-line description, shared by the listing and
/// the resolved result.
const DREAM_DESCRIPTION: &str = "Consolidate and deduplicate the local argosy's memory: enumerate memory concepts via search, read them, then merge, update, delete, or add entries with write_memory and delete_memory. Use it after a long session or whenever memory feels redundant.";

/// The `scan` prompt body: a project-documentation pass that investigates
/// the project at `cwd` and writes the core document set into the local
/// argosy. Investigation is done with the harness's own file tools, while
/// every persistence step names a tool this server exposes.
pub const SCAN_PROMPT: &str = r#"# Scan: Project Documentation

Investigate the project at `cwd` and write what you learn into the local argosy as curated documents, so future sessions start oriented instead of re-reading the whole tree. This is a documentation pass — do not change any project files.

## Steps

0. Every tool call below takes `cwd` — pass the project's absolute root directory on each call (argosys live outside the project tree, keyed by that root).
1. Read the `argosy://_argosys` resource and note the argosy with `"kind": "local"` — that is the only writable argosy, and where these documents go. Then check what already exists: call `search` with a broad query (e.g. "project summary architecture tech stack development"), `namespaces: ["document"]`, `argosy` set to the local argosy's name, and a high `k` (e.g. 50). Read the hits with `read_memory` — an existing document is updated in place, never duplicated.
2. Investigate the project in two passes with your own file tools (list, read, grep) over the project root; if this server exposes the code-intelligence tools (`repomap`, `outline`, `inspect`), use them for the lay of the land.
   - Pass A — inventory: README and everything under `docs/` (note what each file covers), every package manifest and workspace member (`Cargo.toml`, `package.json`, `go.mod`, `pyproject.toml`, …), entry points, build/CI config (`Makefile`, `.github/workflows`, `justfile`, …), the test layout, and the project's own command surface (subcommands, flags, config files users write).
   - Pass B — subsystems: from the inventory, name the major components. For EACH one, read its entry point and enough code to state its responsibility, its key files (real paths), and what it talks to — cover the whole system, not just the best-documented part: platform-specific code, examples, and vendored/generated pieces included. Extract build prerequisites (system libraries, toolchain, tools) from CI and docs.
   - Before writing, check yourself: can you name every major subsystem, trace how a typical request or run flows through them, and give the build, test, and lint commands with their prerequisites? Investigate any gap until you can.
3. Write the core set with `write_document` (paths are bundle-relative concept paths; the `.md` extension is implicit — `document/summary` lands at `document/summary.md`):
   - `document/summary` — what the project is, what it does, and for whom; the one-page orientation.
   - `document/architecture` — the major components, how they connect, where the code lives (real paths), and the flow of a typical request or run.
   - `document/tech` — languages, frameworks, runtime, and key dependencies with versions taken from the manifests, not from memory.
   - `document/development` — how to build, test, lint, and run: the actual commands, taken from CI, Makefile, or docs.
4. Add further documents only when the project clearly warrants them — e.g. `document/decisions/<slug>` (one dated concept per major architectural decision), `document/glossary` (domain terms), `document/conventions` (patterns the codebase consistently follows). Skip anything that would be padding.
5. Audit pass — do not skip: re-read every document you wrote and verify its concrete claims against the project with targeted greps and reads: each named flag, config key, and command; each file path; each count or version; and every "every/all" claim (e.g. "all crates enforce X"). Where a claim fails, fix the document immediately with `write_document` (same path) — correcting it or replacing the detail with a pointer to the file that defines it.
6. If step 1 found a document the project has outgrown, delete it with `delete_document`.
7. Report a one-paragraph summary of what you wrote, updated, skipped, and deleted, including anything the audit pass corrected.

## Rules

- Every document needs YAML frontmatter with a `type` (e.g. `type: Reference`) and a one-line `description` — unlike memory, an untyped document is rejected, not auto-filled.
- Ground every claim in something you actually read: real paths, real commands, versions from the manifests. If you are not sure, say so or leave it out.
- Write for a newcomer session that knows nothing about this project: lead with what matters, keep each document tight, prefer tables of facts over prose.
- Every fact has one home: decide which single document owns each fact; where another document needs it, cross-reference the owner by its path (`document/development`) instead of restating it.
- Distill, don't copy: point at canonical files (README, docs) for the long version instead of transcribing them.
- Re-runs are updates: reuse the same paths so `write_document` reports `updated`, never create near-duplicates under new names.
- Only the local argosy is writable. Imported argosys are read-only — never try to change them.
- Record no secrets or credentials.
- Do not modify the project itself — this pass writes only to the argosy."#;

/// The `scan` prompt's one-line description, shared by the listing and
/// the resolved result.
const SCAN_DESCRIPTION: &str = "Investigate the project at cwd and write its core documents (summary, architecture, tech stack, development guide) into the local argosy with write_document, updating existing documents in place. Use it when onboarding a project whose argosy is empty or its documents have drifted from the code.";

/// The advertised prompt set. The set is static — no `list_changed`
/// notifications.
pub fn prompt_definitions() -> Vec<Prompt> {
    vec![
        Prompt::new("dream", Some(DREAM_DESCRIPTION), None),
        Prompt::new("scan", Some(SCAN_DESCRIPTION), None),
    ]
}

/// Resolves one prompt by name to its messages, or `None` when the name is
/// unknown. Pure and stateless: prompts are static workflows, so unlike
/// tools they never touch [`super::McpState`]. Each workflow runs as a
/// single user-role message.
pub fn get_prompt_result(name: &str) -> Option<GetPromptResult> {
    let (body, description) = match name {
        "dream" => (DREAM_PROMPT, DREAM_DESCRIPTION),
        "scan" => (SCAN_PROMPT, SCAN_DESCRIPTION),
        _ => return None,
    };
    Some(
        GetPromptResult::new(vec![PromptMessage::new_text(Role::User, body)])
            .with_description(description),
    )
}
