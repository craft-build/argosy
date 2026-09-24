//! In-process review sessions over immutable repository diffs.
//!
//! `start_review` snapshots a git diff. Agents read that exact snapshot
//! through `review_diff` and attach structured findings, which can be
//! retrieved through `review_findings`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(feature = "mcp")]
use rmcp::schemars;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use snafu::{OptionExt, ResultExt};

use crate::error::{CodeToolSnafu, Result, ReviewEncodingSnafu, ReviewIoSnafu};

const MAX_DIFF_BYTES: usize = 2 * 1024 * 1024;
static NEXT_REVIEW: AtomicU64 = AtomicU64::new(1);

#[cfg_attr(feature = "mcp", derive(rmcp::schemars::JsonSchema))]
#[derive(Debug, Deserialize)]
pub struct StartReviewParams {
    /// Repository root (or a directory inside it).
    pub cwd: PathBuf,
    /// Git revision to compare the working tree against (default `HEAD`).
    /// Mutually exclusive with `commit`.
    pub base: Option<String>,
    /// Review exactly this committed revision instead of the working tree.
    /// Merge commits are compared with their first parent.
    pub commit: Option<String>,
}

#[cfg_attr(feature = "mcp", derive(rmcp::schemars::JsonSchema))]
#[derive(Debug, Deserialize)]
pub struct ReviewDiffParams {
    /// Opaque id returned by `start_review`.
    pub review_id: String,
    /// Repository-relative changed path. Omit to list the files in the snapshot.
    pub path: Option<String>,
}

#[cfg_attr(feature = "mcp", derive(rmcp::schemars::JsonSchema))]
#[derive(Debug, Deserialize)]
pub struct ReportFindingParams {
    /// Opaque id returned by `start_review`.
    pub review_id: String,
    /// Imperative title prefixed with the priority, e.g. `[P1] Preserve the lock`.
    pub title: String,
    /// What is wrong, why it matters, the concrete failure scenario, and how to fix it.
    pub body: String,
    /// Finding priority.
    pub priority: ReviewPriority,
    /// Certainty from 0.0 through 1.0.
    pub confidence: f32,
    /// Repository-relative file path.
    pub path: String,
    /// First affected line (1-indexed).
    pub line_start: u32,
    /// Last affected line, inclusive.
    pub line_end: u32,
    /// Qualified `argosy://` URIs of the styleguide rules that ground the finding.
    #[serde(default)]
    pub rule_uris: Vec<String>,
    /// Concrete replacement or implementation direction, when useful.
    pub suggestion: Option<String>,
}

#[cfg_attr(feature = "mcp", derive(rmcp::schemars::JsonSchema))]
#[derive(Debug, Deserialize)]
pub struct ReviewFindingsParams {
    /// Opaque id returned by `start_review`.
    pub review_id: String,
    /// Return only this priority.
    pub priority: Option<ReviewPriority>,
    /// Return only findings whose repository-relative path contains this text.
    pub path_contains: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StartReviewOutcome {
    pub review_id: String,
    pub repository: String,
    pub comparison: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    pub files_changed: usize,
    pub changed_files: Vec<String>,
}

#[cfg_attr(feature = "mcp", derive(rmcp::schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
pub enum ReviewPriority {
    P0,
    P1,
    P2,
    P3,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReviewFinding {
    pub finding_id: String,
    pub title: String,
    pub body: String,
    pub priority: ReviewPriority,
    pub confidence: f32,
    pub path: String,
    pub line_start: u32,
    pub line_end: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rule_uris: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReportFindingOutcome {
    pub review_id: String,
    pub finding_id: String,
    /// False when an identical retry had already recorded this finding.
    pub created: bool,
    pub finding_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReviewFindingsOutcome {
    pub review_id: String,
    pub findings: Vec<ReviewFinding>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReviewDiffOutcome {
    pub review_id: String,
    pub changed_files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
}

#[derive(Default)]
pub struct ReviewManager {
    sessions: Mutex<HashMap<String, Session>>,
}

struct Session {
    diff: String,
    changed_files: Vec<String>,
    findings: Vec<ReviewFinding>,
}

pub fn start_review(
    tools: &super::CodeTools,
    params: StartReviewParams,
) -> Result<StartReviewOutcome> {
    tools.reviews.start(params)
}

pub fn review_diff(
    tools: &super::CodeTools,
    params: ReviewDiffParams,
) -> Result<ReviewDiffOutcome> {
    tools.reviews.diff(params)
}

pub fn report_finding(
    tools: &super::CodeTools,
    params: ReportFindingParams,
) -> Result<ReportFindingOutcome> {
    tools.reviews.report_finding(params)
}

pub fn review_findings(
    tools: &super::CodeTools,
    params: ReviewFindingsParams,
) -> Result<ReviewFindingsOutcome> {
    tools.reviews.findings(params)
}

impl ReviewManager {
    fn start(&self, params: StartReviewParams) -> Result<StartReviewOutcome> {
        let root = repository_root(&params.cwd)?;
        if params.base.is_some() && params.commit.is_some() {
            return CodeToolSnafu {
                message: "base and commit are mutually exclusive: use base for working-tree changes or commit for one committed revision",
            }
            .fail();
        }
        let (diff, comparison, base, commit) = if let Some(commit) = params.commit {
            if commit.trim().is_empty() {
                return CodeToolSnafu {
                    message: "commit must not be empty",
                }
                .fail();
            }
            let diff = git_commit_diff(&root, &commit)?;
            let comparison = format!("Commit {commit} against its first parent");
            (diff, comparison, None, Some(commit))
        } else {
            let base = params.base.unwrap_or_else(|| "HEAD".to_string());
            if base.trim().is_empty() {
                return CodeToolSnafu {
                    message: "base must not be empty",
                }
                .fail();
            }
            let diff = git_diff(&root, &base)?;
            let comparison = format!("Working tree compared with {base}");
            (diff, comparison, Some(base), None)
        };
        if diff.trim().is_empty() {
            return CodeToolSnafu {
                message: format!(
                    "no file changes found for `{comparison}` in `{}`",
                    root.display()
                ),
            }
            .fail();
        }
        if diff.len() > MAX_DIFF_BYTES {
            return CodeToolSnafu {
                message: format!(
                    "review diff is {} bytes; the maximum is {MAX_DIFF_BYTES}",
                    diff.len()
                ),
            }
            .fail();
        }

        let id = review_id();
        let repository = root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("repository")
            .to_string();
        let changed_files = changed_files(&diff);
        let files_changed = changed_files.len();

        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                id.clone(),
                Session {
                    diff,
                    changed_files: changed_files.clone(),
                    findings: Vec::new(),
                },
            );

        Ok(StartReviewOutcome {
            review_id: id,
            repository,
            comparison,
            base,
            commit,
            files_changed,
            changed_files,
        })
    }

    fn diff(&self, params: ReviewDiffParams) -> Result<ReviewDiffOutcome> {
        let sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        let session = sessions.get(&params.review_id).context(CodeToolSnafu {
            message: format!("unknown review id `{}`", params.review_id),
        })?;
        let Some(path) = params.path else {
            return Ok(ReviewDiffOutcome {
                review_id: params.review_id,
                changed_files: session.changed_files.clone(),
                path: None,
                diff: None,
            });
        };
        let path = path.trim();
        let diff = diff_for_file(&session.diff, path).with_context(|| CodeToolSnafu {
            message: format!(
                "`{path}` is not in review `{}`; call review_diff without path to list the snapshot",
                params.review_id
            ),
        })?;
        Ok(ReviewDiffOutcome {
            review_id: params.review_id,
            changed_files: session.changed_files.clone(),
            path: Some(path.to_string()),
            diff: Some(diff),
        })
    }

    fn report_finding(&self, params: ReportFindingParams) -> Result<ReportFindingOutcome> {
        let (review_id, finding) = validate_finding(params)?;
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        let session = sessions.get_mut(&review_id).context(CodeToolSnafu {
            message: format!("unknown review id `{review_id}`"),
        })?;
        let created = !session
            .findings
            .iter()
            .any(|existing| existing.finding_id == finding.finding_id);
        if created {
            session.findings.push(finding.clone());
        }
        Ok(ReportFindingOutcome {
            review_id,
            finding_id: finding.finding_id,
            created,
            finding_count: session.findings.len(),
        })
    }

    fn findings(&self, params: ReviewFindingsParams) -> Result<ReviewFindingsOutcome> {
        let sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        let session = sessions.get(&params.review_id).context(CodeToolSnafu {
            message: format!("unknown review id `{}`", params.review_id),
        })?;
        let findings = session
            .findings
            .iter()
            .filter(|finding| {
                params
                    .priority
                    .is_none_or(|priority| finding.priority == priority)
                    && params.path_contains.as_ref().is_none_or(|needle| {
                        finding
                            .path
                            .to_ascii_lowercase()
                            .contains(&needle.to_ascii_lowercase())
                    })
            })
            .cloned()
            .collect();
        Ok(ReviewFindingsOutcome {
            review_id: params.review_id,
            findings,
        })
    }
}

fn validate_finding(params: ReportFindingParams) -> Result<(String, ReviewFinding)> {
    let title = params.title.trim();
    let body = params.body.trim();
    let path = params.path.trim();
    let suggestion = params
        .suggestion
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let expected_prefix = format!("[{}]", params.priority.as_str());
    let path_is_safe = !path.is_empty()
        && Path::new(path).is_relative()
        && Path::new(path).components().all(|component| {
            matches!(
                component,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        });
    if title.is_empty()
        || title.len() > 500
        || !title.starts_with(&expected_prefix)
        || body.is_empty()
        || body.len() > 10_000
        || !path_is_safe
        || path.len() > 4096
        || params.line_start == 0
        || params.line_end < params.line_start
        || !params.confidence.is_finite()
        || !(0.0..=1.0).contains(&params.confidence)
        || suggestion
            .as_ref()
            .is_some_and(|value| value.len() > 10_000)
        || params
            .rule_uris
            .iter()
            .any(|uri| !uri.starts_with("argosy://"))
    {
        return CodeToolSnafu {
            message: format!(
                "invalid finding: title must start with {expected_prefix}, body and repository-relative path must be non-empty, lines must be a valid 1-indexed range, confidence must be 0.0-1.0, and every rule_uris entry must be an argosy:// URI"
            ),
        }
        .fail();
    }

    let mut hasher = Sha256::new();
    for value in [
        params.review_id.as_str(),
        title,
        body,
        params.priority.as_str(),
        path,
        &params.line_start.to_string(),
        &params.line_end.to_string(),
        &params.confidence.to_bits().to_string(),
    ] {
        hasher.update(value.as_bytes());
        hasher.update([0]);
    }
    for uri in &params.rule_uris {
        hasher.update(uri.as_bytes());
        hasher.update([0]);
    }
    if let Some(value) = &suggestion {
        hasher.update(value.as_bytes());
    }
    let finding_id: String = hasher.finalize()[..12]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();

    Ok((
        params.review_id,
        ReviewFinding {
            finding_id,
            title: title.to_string(),
            body: body.to_string(),
            priority: params.priority,
            confidence: params.confidence,
            path: path.to_string(),
            line_start: params.line_start,
            line_end: params.line_end,
            rule_uris: params.rule_uris,
            suggestion,
        },
    ))
}

impl ReviewPriority {
    fn as_str(self) -> &'static str {
        match self {
            Self::P0 => "P0",
            Self::P1 => "P1",
            Self::P2 => "P2",
            Self::P3 => "P3",
        }
    }
}

fn repository_root(cwd: &Path) -> Result<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context(ReviewIoSnafu {
            operation: format!("running git in `{}`", cwd.display()),
        })?;
    if !output.status.success() {
        return CodeToolSnafu {
            message: format!(
                "`{}` is not inside a git repository: {}",
                cwd.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        }
        .fail();
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&output.stdout).trim(),
    ))
}

fn git_diff(root: &Path, base: &str) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "diff",
            "--no-ext-diff",
            "--no-color",
            "--find-renames",
            base,
            "--",
        ])
        .output()
        .context(ReviewIoSnafu {
            operation: format!("generating the working-tree diff from `{base}`"),
        })?;
    if !output.status.success() {
        return CodeToolSnafu {
            message: format!(
                "cannot diff revision `{base}`: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        }
        .fail();
    }
    String::from_utf8(output.stdout).context(ReviewEncodingSnafu {
        artifact: "git working-tree diff",
    })
}

fn git_commit_diff(root: &Path, commit: &str) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "show",
            "--format=",
            "--first-parent",
            "--root",
            "--no-ext-diff",
            "--no-color",
            "--find-renames",
            commit,
            "--",
        ])
        .output()
        .context(ReviewIoSnafu {
            operation: format!("generating the diff for commit `{commit}`"),
        })?;
    if !output.status.success() {
        return CodeToolSnafu {
            message: format!(
                "cannot show commit `{commit}`: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        }
        .fail();
    }
    String::from_utf8(output.stdout).context(ReviewEncodingSnafu {
        artifact: "git commit diff",
    })
}

fn review_id() -> String {
    let mut hasher = Sha256::new();
    hasher.update(std::process::id().to_le_bytes());
    hasher.update(NEXT_REVIEW.fetch_add(1, Ordering::Relaxed).to_le_bytes());
    hasher.update(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .to_le_bytes(),
    );
    hasher.finalize()[..12]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn changed_files(diff: &str) -> Vec<String> {
    diff.lines().filter_map(diff_header_path).collect()
}

fn diff_for_file(diff: &str, wanted: &str) -> Option<String> {
    let mut selected = false;
    let mut out = String::new();
    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            if selected {
                break;
            }
            selected = diff_header_path(line).as_deref() == Some(wanted);
        }
        if selected {
            out.push_str(line);
            out.push('\n');
        }
    }
    (!out.is_empty()).then_some(out)
}

fn diff_header_path(line: &str) -> Option<String> {
    line.strip_prefix("diff --git ")?
        .rsplit_once(" b/")
        .map(|(_, value)| value.trim_end_matches('"').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    #[test]
    fn commit_mode_isolates_the_selected_commit() {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "--quiet"]);
        git(repo.path(), &["config", "user.email", "review@example.com"]);
        git(repo.path(), &["config", "user.name", "Review Test"]);
        std::fs::write(repo.path().join("value.txt"), "one\n").unwrap();
        git(repo.path(), &["add", "value.txt"]);
        git(repo.path(), &["commit", "--quiet", "-m", "one"]);
        std::fs::write(repo.path().join("value.txt"), "two\n").unwrap();
        git(repo.path(), &["commit", "--quiet", "-am", "two"]);
        let selected = Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        let selected = String::from_utf8(selected.stdout).unwrap();
        let selected = selected.trim();
        std::fs::write(repo.path().join("value.txt"), "three\n").unwrap();
        git(repo.path(), &["commit", "--quiet", "-am", "three"]);

        let diff = git_commit_diff(repo.path(), selected).unwrap();
        assert!(diff.contains("+two"));
        assert!(diff.contains("-one"));
        assert!(!diff.contains("three"));
    }

    #[test]
    fn findings_are_returned_to_the_caller() {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "--quiet"]);
        git(repo.path(), &["config", "user.email", "review@example.com"]);
        git(repo.path(), &["config", "user.name", "Review Test"]);
        std::fs::write(repo.path().join("hello.txt"), "before\n").unwrap();
        git(repo.path(), &["add", "hello.txt"]);
        git(repo.path(), &["commit", "--quiet", "-m", "initial"]);
        std::fs::write(repo.path().join("hello.txt"), "after\n").unwrap();

        let tools = super::super::CodeTools::default();
        let opened = start_review(
            &tools,
            StartReviewParams {
                cwd: repo.path().to_path_buf(),
                base: None,
                commit: None,
            },
        )
        .unwrap();
        std::fs::write(repo.path().join("hello.txt"), "later\n").unwrap();
        let snapshot = review_diff(
            &tools,
            ReviewDiffParams {
                review_id: opened.review_id.clone(),
                path: Some("hello.txt".into()),
            },
        )
        .unwrap();
        assert!(snapshot.diff.unwrap().contains("+after"));
        let first = report_finding(
            &tools,
            ReportFindingParams {
                review_id: opened.review_id.clone(),
                title: "[P1] Keep the original greeting".into(),
                body: "When callers expect `before`, returning `after` changes the observable contract. Preserve the old value or update every caller.".into(),
                priority: ReviewPriority::P1,
                confidence: 0.95,
                path: "hello.txt".into(),
                line_start: 1,
                line_end: 1,
                rule_uris: vec!["argosy://rules/styleguide/contracts".into()],
                suggestion: Some("Restore `before` until callers migrate.".into()),
            },
        )
        .unwrap();
        assert!(first.created);
        let duplicate = report_finding(
            &tools,
            ReportFindingParams {
                review_id: opened.review_id.clone(),
                title: "[P1] Keep the original greeting".into(),
                body: "When callers expect `before`, returning `after` changes the observable contract. Preserve the old value or update every caller.".into(),
                priority: ReviewPriority::P1,
                confidence: 0.95,
                path: "hello.txt".into(),
                line_start: 1,
                line_end: 1,
                rule_uris: vec!["argosy://rules/styleguide/contracts".into()],
                suggestion: Some("Restore `before` until callers migrate.".into()),
            },
        )
        .unwrap();
        assert!(!duplicate.created);
        assert_eq!(duplicate.finding_count, 1);
        let findings = review_findings(
            &tools,
            ReviewFindingsParams {
                review_id: opened.review_id.clone(),
                priority: Some(ReviewPriority::P1),
                path_contains: Some("HELLO".into()),
            },
        )
        .unwrap();
        assert_eq!(findings.findings.len(), 1);
    }
}
