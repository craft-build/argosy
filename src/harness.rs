//! Coding-harness integration: installing argosy's `reviewer` subagent
//! definition into a harness's project agent directory. The three
//! supported harnesses all discover agents as markdown files — frontmatter
//! for configuration, body for the system prompt — differing only in
//! location and frontmatter fields. The reviewer body is shared: a
//! read-only review workflow, adapted from Craft's built-in reviewer,
//! that grounds findings in the project's styleguide rules through the
//! argosy MCP tools.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use snafu::{OptionExt, ResultExt, ensure};

use crate::error::{IoSnafu, Result, ValidationSnafu};

/// The coding harnesses argosy can install agent definitions into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Harness {
    /// OpenCode — `.opencode/agents/` (V2 agent markdown).
    OpenCode,
    /// Claude Code — `.claude/agents/` subagent markdown.
    Claude,
    /// Kiro (IDE and `kiro-cli` share `.kiro/agents/`).
    KiroCli,
}

impl Harness {
    /// Every supported harness, in help order.
    pub const ALL: [Harness; 3] = [Harness::OpenCode, Harness::Claude, Harness::KiroCli];

    /// The harness's stable id — the spelling the CLI takes and reports.
    pub fn as_id(&self) -> &'static str {
        match self {
            Harness::OpenCode => "opencode",
            Harness::Claude => "claude",
            Harness::KiroCli => "kiro-cli",
        }
    }

    /// Resolves a harness id ([`Harness::as_id`] spelling). An unknown id
    /// is an error naming the valid ones.
    pub fn from_id(id: &str) -> Result<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|h| h.as_id() == id)
            .with_context(|| ValidationSnafu {
                reason: format!(
                    "unknown harness `{id}`; expected one of {}",
                    Self::ALL
                        .iter()
                        .map(|h| h.as_id())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            })
    }

    /// The project-relative directory the harness reads agent definitions
    /// from.
    fn agents_dir(&self) -> PathBuf {
        match self {
            Harness::OpenCode => PathBuf::from(".opencode/agents"),
            Harness::Claude => PathBuf::from(".claude/agents"),
            Harness::KiroCli => PathBuf::from(".kiro/agents"),
        }
    }

    /// The project-relative path the reviewer definition is written to.
    pub fn reviewer_definition_rel(&self) -> PathBuf {
        self.agents_dir().join("reviewer.md")
    }

    /// The full reviewer definition file: harness-specific frontmatter
    /// followed by the shared reviewer system prompt body.
    pub fn reviewer_definition(&self) -> String {
        let frontmatter = match self {
            // OpenCode V2: `mode: subagent` keeps it out of the primary
            // slot; the deny rules make read-only structural, not just
            // prompted.
            Harness::OpenCode => format!(
                "description: {REVIEWER_DESCRIPTION}\n\
                 mode: subagent\n\
                 permissions:\n\
                 \x20 - action: edit\n\
                 \x20   resource: \"*\"\n\
                 \x20   effect: deny\n\
                 \x20 - action: shell\n\
                 \x20   resource: \"*\"\n\
                 \x20   effect: deny\n"
            ),
            // Claude Code: `name` + `description` are the required pair;
            // `model: inherit` follows the parent. MCP tools are spelled
            // `mcp__<server>__<tool>` — the `argosy` server name is the
            // documented convention; if the server is registered under
            // another name the native tools still work and the prompt's
            // ungrounded-finding rule applies.
            Harness::Claude => format!(
                "name: reviewer\n\
                 description: {REVIEWER_DESCRIPTION}\n\
                 tools: Read, Glob, Grep, mcp__argosy__search, \
                 mcp__argosy__search_rules, mcp__argosy__read, \
                 mcp__argosy__read_memory\n\
                 model: inherit\n"
            ),
            // Kiro: tool tags — `read` (file reads, listing, search) plus
            // `@mcp` (every MCP server from mcp.json, which is where the
            // argosy server is registered). No write/shell tags, so the
            // reviewer is structurally read-only here too.
            Harness::KiroCli => format!(
                "name: reviewer\n\
                 description: {REVIEWER_DESCRIPTION}\n\
                 tools: [\"read\", \"@mcp\"]\n"
            ),
        };
        format!("---\n{frontmatter}---\n\n{REVIEWER_PROMPT}")
    }
}

/// The reviewer's `description` frontmatter field — the text harnesses
/// show in their agent catalogs and delegate on, so it states both the
/// job and the trigger.
const REVIEWER_DESCRIPTION: &str = "Reviews code against argosy styleguide rules and best \
     practices; reports prioritized findings (P0-P3) without modifying files. Use when asked \
     to review code, changes, or a diff.";

/// The reviewer subagent's base system prompt: Craft's built-in reviewer
/// adapted for an MCP harness — findings go in the final message, rule
/// grounding goes through the argosy MCP tools, and reachability checks
/// use the harness's own code search. Defect classes, priorities, and the
/// verdict contract carry over so reviews mean the same thing everywhere.
const REVIEWER_PROMPT: &str = r#"You are a code reviewer. Review code against styleguide rules and best practices. Report findings with clear priority levels. Be thorough but constructive.

# Critical rules

- ALWAYS read the code before reviewing. Never review from descriptions alone.
- Use styleguide rules as the foundation for all findings. Link findings to specific rules.
- Prioritize findings correctly. Not all issues are equal.
- Be specific, actionable, and respectful. Explain WHY something matters.
- You are a reviewer, not an editor: never modify, create, or delete files. Report findings only.

# Styleguide grounding

This project's rules live in its argosy, served over MCP. The argosy is stored outside the project tree (under the user's argosy state dir), so the MCP tools are the way to read it — do not hunt for rule files on disk. When the argosy MCP server is connected, ground the review in it. Every argosy tool call takes `cwd` — pass the project's absolute root directory on every call, or the call is rejected:

- `search_rules` — semantic match of styleguide rules against a natural-language description of the code under review (narrow by `language`/`category`); each hit carries the rule's `## Good`/`## Bad` sections when present.
- `search` — semantic search over every concept (documents, skills, rules, memory) for wider context.
- `read` — direct read of a known concept by bundle-relative path, from any active argosy (`argosy` selects an import; defaults to the local argosy).
- `read_memory` — direct read of a known concept in the local argosy.

When no rules match, review against general best practices and say that the finding is ungrounded. Never invent rule IDs.

# Priority levels

- **P0 - Critical**: Security vulnerabilities, data loss risks, build errors, test failures. Must fix.
- **P1 - Urgent**: Logic errors, missing error handling, race conditions, memory leaks. Should fix.
- **P2 - Normal**: Style violations, minor refactoring, test coverage gaps. Could fix.
- **P3 - Low**: Formatting preferences, optional improvements, future ideas. Nice to have.

# Review workflow

1. Read the files to review. Never review without reading.
2. Get styleguide context — describe the code under review and call `search_rules` for the rules that govern it.
3. Check against rules: naming, error handling, documentation, security, testing, architecture.
4. Report each finding in your final message (format below) with priority, file location, and rule references.
5. Synthesize a verdict after reviewing all files.

# What to look for

High-value defect classes that an undifferentiated scan misses:

- **Dropped invariants in removed lines.** When code is deleted, check what guarantees that code enforced. A removed validation, lock, or null-check often leaves callers assuming a contract that no longer holds.
- **Broken caller contracts for changed signatures.** If a function signature, return type, or exported name changed, find every caller (search the repo) and confirm they still compile and behave correctly. The diff shows the change, not its blast radius.
- **Wrapper/proxy methods that re-enter through a global.** A refactor that adds an indirection can accidentally route back through a global singleton, registry, or trait object instead of the intended delegate, bypassing the new path entirely.

Use the harness's code-search tools to confirm reachability rather than guessing from the diff alone.

# Finding format

Report each finding as an entry in your final message:

- **Title**: imperative mood, prefixed with priority (e.g., "[P1] Add error handling for network timeout")
- **Body**: what the issue is, why it matters, which rule it violates, how to fix it. The body must state a **concrete failure scenario** — the inputs, state, or sequence of events that triggers the bug — not just "this looks wrong." A finding without a trigger path is a hunch.
- **Location**: file path and line reference.
- **Confidence**: 0.0-1.0 based on certainty.

# Verdict

End your final message with a short summary:

- Overall verdict: approve | approve_with_nits | request_changes | needs_discussion
- Priority breakdown (P0/P1/P2/P3 counts)
- Key concerns
- Files reviewed

Keep the verdict short and synthetic. The findings list above it is the durable record; do not restate every finding verbatim in the verdict.

# Guidelines

- Focus on correctness first, then security, maintainability, performance, style last.
- Never report findings without reading the code first.
- Suggest concrete fixes, not just problems.
- Acknowledge tradeoffs when they exist.
- Every tool result grows your context. Minimize use of verbose tool calls; batch independent reads when the harness supports it.
"#;

/// The outcome of installing the reviewer agent definition for one
/// harness: what was written, where, and whether it replaced something.
#[derive(Debug, Serialize)]
pub struct ReviewerSetup {
    /// The harness the definition was written for, by id
    /// (`opencode`, `claude`, `kiro-cli`).
    pub harness: String,
    /// The definition file as written (the project root joined with the
    /// harness-relative path).
    pub path: PathBuf,
    /// Whether an existing definition was replaced.
    pub overwritten: bool,
}

/// Writes the `reviewer` subagent definition for `harness` into
/// `project_root`'s harness agent directory, creating the directory as
/// needed. An existing definition is an error unless `force` replaces
/// it — regenerating silently would discard user edits.
pub fn setup_reviewer(harness: Harness, project_root: &Path, force: bool) -> Result<ReviewerSetup> {
    let dest = project_root.join(harness.reviewer_definition_rel());
    let existed = dest.exists();
    ensure!(
        !existed || force,
        ValidationSnafu {
            reason: format!(
                "reviewer definition for {} already exists at {}; delete it or overwrite it \
                 explicitly to replace it",
                harness.as_id(),
                dest.display()
            )
        }
    );
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).context(IoSnafu {
            path: parent.to_path_buf(),
        })?;
    }
    // Crash-atomic like every other argosy write: stage then rename, so a
    // crash mid-write can never leave a truncated harness definition the
    // harness would happily load.
    let tmp = dest.with_file_name(format!(
        "{}.tmp",
        dest.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("reviewer")
    ));
    let staged =
        fs::write(&tmp, harness.reviewer_definition()).and_then(|()| fs::rename(&tmp, &dest));
    if staged.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    staged.context(IoSnafu { path: dest.clone() })?;
    Ok(ReviewerSetup {
        harness: harness.as_id().to_string(),
        path: dest,
        overwritten: existed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;
    use tempfile::TempDir;

    #[test]
    fn from_id_round_trips_every_harness_and_rejects_unknown_ids() {
        for harness in Harness::ALL {
            assert_eq!(Harness::from_id(harness.as_id()).unwrap(), harness);
        }
        let err = Harness::from_id("cursor").unwrap_err();
        assert!(matches!(err, Error::Validation { .. }), "got {err:?}");
        assert!(
            err.to_string().contains("opencode, claude, kiro-cli"),
            "error lists the valid ids: {err}"
        );
    }

    #[test]
    fn reviewer_definitions_carry_harness_frontmatter_and_shared_prompt() {
        for harness in Harness::ALL {
            let definition = harness.reviewer_definition();
            assert!(
                definition.starts_with("---\n"),
                "{}: frontmatter block opens the file",
                harness.as_id()
            );
            // Exactly one frontmatter block: the body must not close and
            // reopen one (a `---` line at body start would swallow the
            // prompt).
            assert_eq!(
                definition.match_indices("\n---\n").count(),
                1,
                "{}: exactly one frontmatter close",
                harness.as_id()
            );
            assert!(
                definition.ends_with('\n'),
                "{}: file ends on a newline",
                harness.as_id()
            );
            match harness {
                Harness::OpenCode => {
                    assert!(definition.contains("mode: subagent"));
                    assert!(definition.contains("action: edit"));
                    assert!(definition.contains("action: shell"));
                }
                Harness::Claude => {
                    assert!(definition.contains("name: reviewer"));
                    assert!(definition.contains("model: inherit"));
                    assert!(definition.contains("tools: Read, Glob, Grep"));
                    assert!(definition.contains("mcp__argosy__search_rules"));
                }
                Harness::KiroCli => {
                    assert!(definition.contains("name: reviewer"));
                    assert!(definition.contains("tools: [\"read\", \"@mcp\"]"));
                }
            }
            // The shared reviewer prompt body, harness-independently.
            for marker in [
                "You are a code reviewer",
                "search_rules",
                // The argosy tools require `cwd` since the multi-project
                // refactor; a prompt that omits it gets every call rejected.
                "pass the project's absolute root directory",
                "**P0 - Critical**",
                "**P3 - Low**",
                "concrete failure scenario",
                "approve_with_nits",
                "never modify, create, or delete files",
            ] {
                assert!(
                    definition.contains(marker),
                    "{}: missing `{marker}`",
                    harness.as_id()
                );
            }
        }
    }

    #[test]
    fn reviewer_definition_rel_matches_each_harness_layout() {
        assert_eq!(
            Harness::OpenCode.reviewer_definition_rel(),
            Path::new(".opencode/agents/reviewer.md")
        );
        assert_eq!(
            Harness::Claude.reviewer_definition_rel(),
            Path::new(".claude/agents/reviewer.md")
        );
        assert_eq!(
            Harness::KiroCli.reviewer_definition_rel(),
            Path::new(".kiro/agents/reviewer.md")
        );
    }

    #[test]
    fn setup_writes_each_harness_definition_creating_directories() {
        let tmp = TempDir::new().unwrap();
        for harness in Harness::ALL {
            let report = setup_reviewer(harness, tmp.path(), false).unwrap();
            assert_eq!(report.harness, harness.as_id());
            assert_eq!(
                report.path,
                tmp.path().join(harness.reviewer_definition_rel())
            );
            assert!(report.path.is_file(), "definition written");
            assert!(!report.overwritten, "fresh install is not an overwrite");
            assert_eq!(
                fs::read_to_string(&report.path).unwrap(),
                harness.reviewer_definition()
            );
        }
    }

    #[test]
    fn setup_refuses_an_existing_definition_and_leaves_it_alone() {
        let tmp = TempDir::new().unwrap();
        setup_reviewer(Harness::Claude, tmp.path(), false).unwrap();
        let path = tmp.path().join(".claude/agents/reviewer.md");
        fs::write(&path, "user edits\n").unwrap();

        let err = setup_reviewer(Harness::Claude, tmp.path(), false).unwrap_err();
        assert!(matches!(err, Error::Validation { .. }), "got {err:?}");
        assert!(
            err.to_string().contains("already exists"),
            "error names the collision: {err}"
        );
        // The user's file is untouched.
        assert_eq!(fs::read_to_string(&path).unwrap(), "user edits\n");
    }

    #[test]
    fn setup_force_replaces_an_existing_definition() {
        let tmp = TempDir::new().unwrap();
        setup_reviewer(Harness::KiroCli, tmp.path(), false).unwrap();
        let path = tmp.path().join(".kiro/agents/reviewer.md");
        fs::write(&path, "stale definition\n").unwrap();

        let report = setup_reviewer(Harness::KiroCli, tmp.path(), true).unwrap();
        assert!(report.overwritten);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            Harness::KiroCli.reviewer_definition()
        );
    }

    #[test]
    fn report_serializes_for_the_json_flag() {
        let tmp = TempDir::new().unwrap();
        let report = setup_reviewer(Harness::OpenCode, tmp.path(), false).unwrap();
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["harness"], "opencode");
        assert_eq!(json["overwritten"], false);
        assert!(
            json["path"]
                .as_str()
                .is_some_and(|p| p.ends_with(".opencode/agents/reviewer.md"))
        );
    }
}
