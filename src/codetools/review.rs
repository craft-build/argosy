//! In-process review sessions over immutable repository diffs.
//!
//! `start_review` snapshots the diff; the other tools read that exact
//! snapshot and attach or retrieve findings.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(feature = "mcp")]
use rmcp::schemars;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use snafu::{OptionExt, ResultExt};

use crate::error::{CodeToolSnafu, Result, ReviewEncodingSnafu, ReviewIoSnafu};

const MAX_DIFF_BYTES: usize = 2 * 1024 * 1024;
/// Default review-session idle TTL.
const DEFAULT_REVIEW_TTL: Duration = Duration::from_secs(7200);
/// Default review-session cap.
const DEFAULT_MAX_REVIEWS: usize = 16;
static NEXT_REVIEW: AtomicU64 = AtomicU64::new(1);

#[cfg_attr(feature = "mcp", derive(rmcp::schemars::JsonSchema))]
#[derive(Debug, Deserialize)]
pub struct StartReviewParams {
    /// Repository root (or a directory inside it).
    pub cwd: PathBuf,
    /// Git revision or range to compare the working tree against (default `HEAD`).
    ///
    /// Mutually exclusive with `commit`; values starting with `-` are rejected.
    pub base: Option<String>,
    /// Committed revision or commit range to review instead of the working tree.
    ///
    /// Values starting with `-` are rejected; merge commits compare with their
    /// first parent.
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
    /// Advisory assessment from the decision endpoint, when one was enabled at
    /// report time; it never changes the finding's presence or priority.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assessment: Option<FindingAssessment>,
}

/// An advisory assessment of a finding by the decision endpoint; it never
/// filters or downgrades the finding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FindingAssessment {
    /// `"supported"` or `"unsupported"`, when the endpoint answered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub support: Option<String>,
    /// Calibrated confidence in the support answer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub support_confidence: Option<f32>,
    /// The chosen severity label (`P0`-`P3`), when the endpoint answered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
    /// Calibrated confidence in the severity answer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity_confidence: Option<f32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReportFindingOutcome {
    pub review_id: String,
    pub finding_id: String,
    /// False when an identical retry had already recorded this finding.
    pub created: bool,
    pub finding_count: usize,
    /// The advisory assessment recorded with this finding, when any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assessment: Option<FindingAssessment>,
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

pub struct ReviewManager {
    sessions: Mutex<HashMap<String, Session>>,
    /// Session idle TTL.
    ttl: Duration,
    /// Session cap.
    max_sessions: usize,
    /// The process-global decision endpoint, for advisory finding scoring.
    #[cfg(feature = "decision")]
    decision: Option<std::sync::Arc<dyn crate::decision::DecisionProvider>>,
}

impl Default for ReviewManager {
    fn default() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            ttl: DEFAULT_REVIEW_TTL,
            max_sessions: DEFAULT_MAX_REVIEWS,
            #[cfg(feature = "decision")]
            decision: None,
        }
    }
}

struct Session {
    diff: String,
    changed_files: Vec<String>,
    findings: Vec<ReviewFinding>,
    /// Last use.
    touched: Instant,
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
    /// Overrides the session-cache bounds.
    pub fn with_limits(mut self, ttl: Duration, max_sessions: usize) -> Self {
        self.ttl = ttl;
        self.max_sessions = max_sessions.max(1);
        self
    }

    /// Drops sessions idle past the TTL.
    fn purge_expired(sessions: &mut HashMap<String, Session>, ttl: Duration) {
        let now = Instant::now();
        sessions.retain(|_, session| now.duration_since(session.touched) < ttl);
    }

    /// Evicts the least-recently-used session until there is room.
    fn evict_for_capacity(sessions: &mut HashMap<String, Session>, max_sessions: usize) {
        while sessions.len() >= max_sessions {
            let Some(oldest) = sessions
                .iter()
                .min_by_key(|(_, session)| session.touched)
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            sessions.remove(&oldest);
        }
    }

    /// The session for `review_id`, stamped as most recently used.
    fn touch<'a>(
        sessions: &'a mut HashMap<String, Session>,
        review_id: &str,
    ) -> Result<&'a mut Session> {
        let Some(session) = sessions.get_mut(review_id) else {
            return CodeToolSnafu {
                message: format!(
                    "unknown review id `{review_id}` (it may have expired from the session cache; \
                     call start_review again)"
                ),
            }
            .fail();
        };
        session.touched = Instant::now();
        Ok(session)
    }

    /// Installs the decision endpoint used for advisory finding scoring.
    #[cfg(feature = "decision")]
    pub(crate) fn set_decision(
        &mut self,
        decision: std::sync::Arc<dyn crate::decision::DecisionProvider>,
    ) {
        self.decision = Some(decision);
    }

    fn start(&self, params: StartReviewParams) -> Result<StartReviewOutcome> {
        let root = repository_root(&params.cwd)?;
        if params.base.is_some() && params.commit.is_some() {
            return CodeToolSnafu {
                message: "base and commit are mutually exclusive: use base for working-tree changes or commit for one committed revision",
            }
            .fail();
        }
        let (diff, comparison, base, commit) = if let Some(commit) = params.commit {
            let resolved = resolve_revision(&root, &commit, "commit")?;
            let diff = git_commit_diff(&root, &resolved)?;
            let comparison = format!("Commit {commit} against its first parent");
            (diff, comparison, None, Some(commit))
        } else {
            let base = params.base.unwrap_or_else(|| "HEAD".to_string());
            let resolved = resolve_revision(&root, &base, "base")?;
            let diff = git_diff(&root, &resolved)?;
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

        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        Self::purge_expired(&mut sessions, self.ttl);
        Self::evict_for_capacity(&mut sessions, self.max_sessions);
        sessions.insert(
            id.clone(),
            Session {
                diff,
                changed_files: changed_files.clone(),
                findings: Vec::new(),
                touched: Instant::now(),
            },
        );
        drop(sessions);

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
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        Self::purge_expired(&mut sessions, self.ttl);
        let session = Self::touch(&mut sessions, &params.review_id)?;
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
        let (review_id, mut finding) = validate_finding(params)?;
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        Self::purge_expired(&mut sessions, self.ttl);
        let session = Self::touch(&mut sessions, &review_id)?;
        let created = !session
            .findings
            .iter()
            .any(|existing| existing.finding_id == finding.finding_id);
        // Captured before releasing the lock so the decision call is lock-free.
        #[cfg(feature = "decision")]
        let hunk = created.then(|| diff_for_file(&session.diff, &finding.path).unwrap_or_default());
        if created {
            session.findings.push(finding.clone());
        }
        let finding_count = session.findings.len();
        drop(sessions);

        // Computed outside the lock (a network call); any failure is silent —
        // scoring never affects whether the finding exists.
        #[cfg(feature = "decision")]
        if created
            && let Some(decision) = self.decision.as_ref().filter(|d| d.is_enabled())
            && let Some(assessment) = assess_finding(
                decision.as_ref(),
                &finding,
                hunk.as_deref().unwrap_or_default(),
            )
        {
            finding.assessment = Some(assessment.clone());
            let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(session) = sessions.get_mut(&review_id)
                && let Some(stored) = session
                    .findings
                    .iter_mut()
                    .find(|existing| existing.finding_id == finding.finding_id)
            {
                stored.assessment = Some(assessment);
            }
        }

        Ok(ReportFindingOutcome {
            review_id,
            finding_id: finding.finding_id,
            created,
            finding_count,
            assessment: finding.assessment,
        })
    }

    fn findings(&self, params: ReviewFindingsParams) -> Result<ReviewFindingsOutcome> {
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        Self::purge_expired(&mut sessions, self.ttl);
        let session = Self::touch(&mut sessions, &params.review_id)?;
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

/// Asks the decision endpoint to score a finding, returning `None` when it
/// answers nothing usable. Advisory only — never a filter.
#[cfg(feature = "decision")]
fn assess_finding(
    decision: &dyn crate::decision::DecisionProvider,
    finding: &ReviewFinding,
    hunk: &str,
) -> Option<FindingAssessment> {
    use crate::decision::{ChoiceResolution, DecisionRequest, Question, resolve_choice};

    let state = serde_json::json!({
        "reported_finding": {
            "title": finding.title,
            "body": finding.body,
            "path": finding.path,
            "line_start": finding.line_start,
            "line_end": finding.line_end,
        },
        "code": hunk,
    });
    let request = DecisionRequest::new(state)
        .ask(
            "support",
            Question::choice(
                "Is the reported defect actually present in the code shown?",
                [
                    ("A", "the defect is present in the code"),
                    ("B", "the code does not support the reported defect"),
                ],
            ),
        )
        .ask(
            "severity",
            Question::choice(
                "How severe is the reported defect?",
                [
                    (
                        "P0",
                        "critical: security flaw, data loss, build or test failure",
                    ),
                    (
                        "P1",
                        "urgent: logic error, missing error handling, race condition",
                    ),
                    (
                        "P2",
                        "normal: style violation, minor refactor, coverage gap",
                    ),
                    ("P3", "low: formatting preference or optional improvement"),
                ],
            ),
        );
    let response = decision.decide(&request).ok()?;
    let support_confidence = response.answer_confidence("support");
    let severity_confidence = response.answer_confidence("severity");
    let support = match resolve_choice(
        &response,
        "support",
        "none",
        &[
            ("A".to_string(), "supported".to_string()),
            ("B".to_string(), "unsupported".to_string()),
        ],
    ) {
        ChoiceResolution::Selected { value, .. } => Some(value),
        _ => None,
    };
    let severity = response
        .primary("severity")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let has_support = support.is_some();
    let has_severity = severity.is_some();
    let assessment = FindingAssessment {
        support_confidence: has_support.then_some(support_confidence).flatten(),
        support,
        severity_confidence: has_severity.then_some(severity_confidence).flatten(),
        severity,
    };
    (has_support || has_severity).then_some(assessment)
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
            assessment: None,
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

/// Resolves a caller-supplied `base`/`commit` revision into an inert value —
/// a verified hex object SHA, or `sha..sha`/`sha...sha` for ranges — before
/// it ever reaches `git diff` or `git show`.
///
/// An option-shaped value would be parsed as a git option there, and both
/// commands accept `--output=<path>`: an arbitrary file write. A leading `-`
/// is refused outright; everything else resolves only through `git rev-parse
/// --verify --end-of-options`, and only a hex SHA is accepted.
fn resolve_revision(root: &Path, spec: &str, param: &str) -> Result<String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return CodeToolSnafu {
            message: format!("{param} must not be empty"),
        }
        .fail();
    }
    if spec.starts_with('-') {
        return CodeToolSnafu {
            message: format!(
                "invalid {param} `{spec}`: pass a revision expression \
                 (commit, branch, tag, tree, or range), not a git option"
            ),
        }
        .fail();
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--verify", "--end-of-options"])
        .arg(spec)
        .output()
        .context(ReviewIoSnafu {
            operation: format!("resolving revision `{spec}`"),
        })?;
    if output.status.success() {
        let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !is_object_sha(&sha) {
            // Unreachable with `--verify` today, but the contract is that only
            // an inert value ever reaches `git diff` / `git show`.
            return CodeToolSnafu {
                message: format!(
                    "{param} `{spec}` resolved to unexpected revision `{sha}`; \
                     refusing to pass it to git"
                ),
            }
            .fail();
        }
        return Ok(sha);
    }
    // A range legitimately fails `--verify` with "needed a single revision",
    // so split and resolve each endpoint. Revision syntax cannot otherwise
    // contain `..` — git rejects it in refnames and `^{/regex}` — so the
    // split matches git's own grammar.
    if let Some((left, right, dots)) = split_range(spec) {
        let left_sha = resolve_endpoint(root, left, param, spec)?;
        let right_sha = resolve_endpoint(root, right, param, spec)?;
        return Ok(format!("{left_sha}{dots}{right_sha}"));
    }
    CodeToolSnafu {
        message: format!(
            "{param} `{spec}` does not resolve to a git revision in this repository: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    }
    .fail()
}

/// Splits `a..b`/`a...b` into endpoints and separator; an empty side means
/// `HEAD`. `None` when there is no range separator.
fn split_range(spec: &str) -> Option<(&str, &str, &'static str)> {
    let start = spec.find("..")?;
    let rest = &spec[start..];
    if let Some(right) = rest.strip_prefix("...") {
        Some((&spec[..start], right, "..."))
    } else if let Some(right) = rest.strip_prefix("..") {
        Some((&spec[..start], right, ".."))
    } else {
        None
    }
}

/// Resolves one endpoint of a range to a verified hex SHA, appending
/// `^{commit}` (peeling tags); an empty side means `HEAD`.
fn resolve_endpoint(root: &Path, side: &str, param: &str, spec: &str) -> Result<String> {
    let rev = if side.is_empty() { "HEAD" } else { side };
    if rev.starts_with('-') {
        return CodeToolSnafu {
            message: format!(
                "invalid {param} `{spec}`: pass a revision expression, not a git option"
            ),
        }
        .fail();
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--verify", "--end-of-options"])
        .arg(format!("{rev}^{{commit}}"))
        .output()
        .context(ReviewIoSnafu {
            operation: format!("resolving range endpoint `{rev}`"),
        })?;
    if !output.status.success() {
        return CodeToolSnafu {
            message: format!(
                "{param} `{spec}`: side `{rev}` does not name a commit in this repository: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        }
        .fail();
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !is_object_sha(&sha) {
        return CodeToolSnafu {
            message: format!(
                "{param} `{spec}`: side `{rev}` resolved to unexpected revision `{sha}`; \
                 refusing to pass it to git"
            ),
        }
        .fail();
    }
    Ok(sha)
}

/// A git object id as printed by `rev-parse --verify`: 40 (SHA-1) or 64 (SHA-256) hex digits.
fn is_object_sha(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.chars().all(|c| c.is_ascii_hexdigit())
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

    /// One commit plus an uncommitted working-tree change, the minimum `start_review` needs.
    fn review_repo() -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "--quiet"]);
        git(repo.path(), &["config", "user.email", "review@example.com"]);
        git(repo.path(), &["config", "user.name", "Review Test"]);
        std::fs::write(repo.path().join("hello.txt"), "before\n").unwrap();
        git(repo.path(), &["add", "hello.txt"]);
        git(repo.path(), &["commit", "--quiet", "-m", "initial"]);
        std::fs::write(repo.path().join("hello.txt"), "after\n").unwrap();
        repo
    }

    #[test]
    fn option_shaped_base_is_rejected_before_reaching_git() {
        let repo = review_repo();
        let target = repo.path().join("pwned.txt");
        let tools = super::super::CodeTools::default();
        let err = start_review(
            &tools,
            StartReviewParams {
                cwd: repo.path().to_path_buf(),
                base: Some(format!("--output={}", target.display())),
                commit: None,
            },
        )
        .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("not a git option"), "got: {message}");
        assert!(
            !target.exists(),
            "git must never write the diff to an option-named path"
        );
    }

    #[test]
    fn option_shaped_commit_is_rejected_before_reaching_git() {
        let repo = review_repo();
        let target = repo.path().join("pwned.txt");
        let tools = super::super::CodeTools::default();
        let err = start_review(
            &tools,
            StartReviewParams {
                cwd: repo.path().to_path_buf(),
                base: None,
                commit: Some(format!("-o{}", target.display())),
            },
        )
        .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("not a git option"), "got: {message}");
        assert!(
            !target.exists(),
            "git must never write the diff to an option-named path"
        );
    }

    #[test]
    fn unresolvable_revision_is_reported_actively() {
        let repo = review_repo();
        let tools = super::super::CodeTools::default();
        let err = start_review(
            &tools,
            StartReviewParams {
                cwd: repo.path().to_path_buf(),
                base: Some("does-not-exist".into()),
                commit: None,
            },
        )
        .unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("does not resolve to a git revision"),
            "got: {message}"
        );
        assert!(message.contains("does-not-exist"), "got: {message}");
    }

    #[test]
    fn review_sessions_expire_after_the_idle_ttl() {
        let repo = review_repo();
        // A zero TTL makes every cached session immediately stale.
        let tools =
            super::super::CodeTools::default().with_review_limits(std::time::Duration::ZERO, 16);
        let opened = start_review(
            &tools,
            StartReviewParams {
                cwd: repo.path().to_path_buf(),
                base: None,
                commit: None,
            },
        )
        .unwrap();
        let err = review_findings(
            &tools,
            ReviewFindingsParams {
                review_id: opened.review_id,
                priority: None,
                path_contains: None,
            },
        )
        .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("unknown review id"), "got: {message}");
        assert!(
            message.contains("expired"),
            "the error must suggest re-running start_review: {message}"
        );
    }

    #[test]
    fn the_lru_cap_evicts_the_least_recently_used_review() {
        let repo = review_repo();
        let tools = super::super::CodeTools::default()
            .with_review_limits(std::time::Duration::from_secs(3600), 2);
        let open = || {
            start_review(
                &tools,
                StartReviewParams {
                    cwd: repo.path().to_path_buf(),
                    base: None,
                    commit: None,
                },
            )
            .unwrap()
            .review_id
        };
        let first = open();
        let second = open();
        // Touch the first so the second is the eviction candidate.
        review_diff(
            &tools,
            ReviewDiffParams {
                review_id: first.clone(),
                path: None,
            },
        )
        .unwrap();
        let third = open();

        for (id, kept) in [(&first, true), (&second, false), (&third, true)] {
            let result = review_diff(
                &tools,
                ReviewDiffParams {
                    review_id: id.clone(),
                    path: None,
                },
            );
            assert_eq!(result.is_ok(), kept, "review `{id}` kept={kept}");
        }
    }

    #[test]
    fn reading_a_review_refreshes_its_idle_ttl() {
        let repo = review_repo();
        let tools = super::super::CodeTools::default()
            .with_review_limits(std::time::Duration::from_millis(2000), 16);
        let opened = start_review(
            &tools,
            StartReviewParams {
                cwd: repo.path().to_path_buf(),
                base: None,
                commit: None,
            },
        )
        .unwrap();
        // Touch before the TTL lapses, then sleep past a creation-only TTL.
        std::thread::sleep(std::time::Duration::from_millis(300));
        review_diff(
            &tools,
            ReviewDiffParams {
                review_id: opened.review_id.clone(),
                path: None,
            },
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1700));
        review_findings(
            &tools,
            ReviewFindingsParams {
                review_id: opened.review_id,
                priority: None,
                path_contains: None,
            },
        )
        .expect("an access refreshes the idle clock");
    }

    #[test]
    fn symbolic_revisions_resolve_in_both_review_modes() {
        let repo = review_repo();
        git(repo.path(), &["tag", "first"]);
        git(repo.path(), &["commit", "--quiet", "-am", "second"]);
        git(repo.path(), &["branch", "second"]);
        std::fs::write(repo.path().join("hello.txt"), "later\n").unwrap();
        let tools = super::super::CodeTools::default();

        // Working-tree mode against a tag: the outcome keeps the caller's spelling.
        let opened = start_review(
            &tools,
            StartReviewParams {
                cwd: repo.path().to_path_buf(),
                base: Some("first".into()),
                commit: None,
            },
        )
        .unwrap();
        assert_eq!(opened.base.as_deref(), Some("first"));
        assert_eq!(opened.changed_files, vec!["hello.txt".to_string()]);
        let snapshot = review_diff(
            &tools,
            ReviewDiffParams {
                review_id: opened.review_id.clone(),
                path: Some("hello.txt".into()),
            },
        )
        .unwrap();
        let base_diff = snapshot.diff.unwrap();
        assert!(base_diff.contains("-before"));
        assert!(base_diff.contains("+later"));

        // Commit mode against a branch name.
        let opened = start_review(
            &tools,
            StartReviewParams {
                cwd: repo.path().to_path_buf(),
                base: None,
                commit: Some("second".into()),
            },
        )
        .unwrap();
        assert_eq!(opened.commit.as_deref(), Some("second"));
        assert_eq!(opened.changed_files, vec!["hello.txt".to_string()]);
        let snapshot = review_diff(
            &tools,
            ReviewDiffParams {
                review_id: opened.review_id.clone(),
                path: Some("hello.txt".into()),
            },
        )
        .unwrap();
        let diff = snapshot.diff.unwrap();
        assert!(diff.contains("+after"));
        assert!(diff.contains("-before"));
    }

    #[test]
    fn range_base_diffs_endpoints_not_the_worktree() {
        let repo = review_repo();
        git(repo.path(), &["commit", "--quiet", "-am", "second"]);
        std::fs::write(repo.path().join("hello.txt"), "later\n").unwrap();
        let tools = super::super::CodeTools::default();

        let opened = start_review(
            &tools,
            StartReviewParams {
                cwd: repo.path().to_path_buf(),
                base: Some("HEAD~1..HEAD".into()),
                commit: None,
            },
        )
        .unwrap();
        let snapshot = review_diff(
            &tools,
            ReviewDiffParams {
                review_id: opened.review_id,
                path: Some("hello.txt".into()),
            },
        )
        .unwrap();
        let diff = snapshot.diff.unwrap();
        assert!(diff.contains("+after"), "the committed endpoint delta");
        assert!(diff.contains("-before"), "the committed endpoint delta");
        assert!(
            !diff.contains("+later"),
            "a range diffs its endpoint trees, not the working tree"
        );
    }

    #[test]
    fn three_dot_base_diffs_from_the_merge_base() {
        let repo = review_repo();
        git(repo.path(), &["commit", "--quiet", "-am", "second"]);
        git(
            repo.path(),
            &["checkout", "--quiet", "-b", "side", "HEAD~1"],
        );
        std::fs::write(repo.path().join("side.txt"), "side\n").unwrap();
        git(repo.path(), &["add", "side.txt"]);
        git(repo.path(), &["commit", "--quiet", "-m", "side work"]);
        git(repo.path(), &["checkout", "--quiet", "-"]);
        std::fs::write(repo.path().join("hello.txt"), "later\n").unwrap();
        let tools = super::super::CodeTools::default();

        // Two dots: both endpoint trees changed, so the side file shows as removed.
        let two = start_review(
            &tools,
            StartReviewParams {
                cwd: repo.path().to_path_buf(),
                base: Some("side..HEAD".into()),
                commit: None,
            },
        )
        .unwrap();
        assert!(two.changed_files.contains(&"side.txt".into()));
        assert!(two.changed_files.contains(&"hello.txt".into()));
        let hello = review_diff(
            &tools,
            ReviewDiffParams {
                review_id: two.review_id.clone(),
                path: Some("hello.txt".into()),
            },
        )
        .unwrap();
        let hello_diff = hello.diff.unwrap();
        assert!(hello_diff.contains("+after"));
        assert!(
            !hello_diff.contains("+later"),
            "a range diffs its endpoint trees, not the working tree"
        );
        let side = review_diff(
            &tools,
            ReviewDiffParams {
                review_id: two.review_id,
                path: Some("side.txt".into()),
            },
        )
        .unwrap();
        assert!(side.diff.unwrap().contains("-side"));

        // Three dots: from the merge base, so the side work predates the fork point.
        let three = start_review(
            &tools,
            StartReviewParams {
                cwd: repo.path().to_path_buf(),
                base: Some("side...HEAD".into()),
                commit: None,
            },
        )
        .unwrap();
        assert!(!three.changed_files.contains(&"side.txt".into()));
        assert!(three.changed_files.contains(&"hello.txt".into()));
        let hello = review_diff(
            &tools,
            ReviewDiffParams {
                review_id: three.review_id,
                path: Some("hello.txt".into()),
            },
        )
        .unwrap();
        assert!(hello.diff.unwrap().contains("+after"));
    }

    #[test]
    fn tree_revision_works_as_base() {
        let repo = review_repo();
        let tools = super::super::CodeTools::default();
        let opened = start_review(
            &tools,
            StartReviewParams {
                cwd: repo.path().to_path_buf(),
                base: Some("HEAD^{tree}".into()),
                commit: None,
            },
        )
        .unwrap();
        let snapshot = review_diff(
            &tools,
            ReviewDiffParams {
                review_id: opened.review_id,
                path: Some("hello.txt".into()),
            },
        )
        .unwrap();
        let diff = snapshot.diff.unwrap();
        assert!(
            diff.contains("-before"),
            "the committed tree is the baseline"
        );
        assert!(diff.contains("+after"), "the working tree is the subject");
    }

    #[test]
    fn commit_range_reviews_each_commit_patch() {
        let repo = review_repo();
        git(repo.path(), &["commit", "--quiet", "-am", "second"]);
        std::fs::write(repo.path().join("other.txt"), "other\n").unwrap();
        git(repo.path(), &["add", "other.txt"]);
        git(repo.path(), &["commit", "--quiet", "-am", "third"]);
        std::fs::write(repo.path().join("hello.txt"), "final\n").unwrap();
        let tools = super::super::CodeTools::default();
        let opened = start_review(
            &tools,
            StartReviewParams {
                cwd: repo.path().to_path_buf(),
                base: None,
                commit: Some("HEAD~2..HEAD".into()),
            },
        )
        .unwrap();
        assert!(
            opened.changed_files.contains(&"hello.txt".to_string())
                && opened.changed_files.contains(&"other.txt".to_string()),
            "both commits' files are in the snapshot: {:?}",
            opened.changed_files
        );
        let hello = review_diff(
            &tools,
            ReviewDiffParams {
                review_id: opened.review_id.clone(),
                path: Some("hello.txt".into()),
            },
        )
        .unwrap();
        let hello_diff = hello.diff.unwrap();
        assert!(hello_diff.contains("+after"), "the second commit's patch");
        assert!(
            !hello_diff.contains("+final"),
            "ranges show committed work, not the working tree"
        );
        let other = review_diff(
            &tools,
            ReviewDiffParams {
                review_id: opened.review_id,
                path: Some("other.txt".into()),
            },
        )
        .unwrap();
        assert!(
            other.diff.unwrap().contains("+other"),
            "the third commit's patch"
        );
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

    #[cfg(feature = "decision")]
    struct FakeDecision {
        answers: serde_json::Value,
    }

    #[cfg(feature = "decision")]
    impl crate::decision::DecisionProvider for FakeDecision {
        fn is_enabled(&self) -> bool {
            true
        }
        fn decide(
            &self,
            _request: &crate::decision::DecisionRequest,
        ) -> Result<crate::decision::DecisionResponse> {
            Ok(serde_json::from_value(serde_json::json!({ "answers": self.answers })).unwrap())
        }
    }

    #[cfg(feature = "decision")]
    fn sample_finding() -> ReviewFinding {
        ReviewFinding {
            finding_id: "f1".into(),
            title: "[P1] Keep the original greeting".into(),
            body: "Returning `after` changes the contract.".into(),
            priority: ReviewPriority::P1,
            confidence: 0.9,
            path: "hello.txt".into(),
            line_start: 1,
            line_end: 1,
            rule_uris: Vec::new(),
            suggestion: None,
            assessment: None,
        }
    }

    #[cfg(feature = "decision")]
    #[test]
    fn assess_finding_maps_support_and_severity() {
        let decision = FakeDecision {
            answers: serde_json::json!({
                "support": {"choice": "A", "answer_confidence": 0.8},
                "severity": {"choice": "P2", "answer_confidence": 0.6},
            }),
        };
        let assessment = assess_finding(&decision, &sample_finding(), "context").unwrap();
        assert_eq!(assessment.support.as_deref(), Some("supported"));
        assert_eq!(assessment.support_confidence, Some(0.8));
        assert_eq!(assessment.severity.as_deref(), Some("P2"));
        assert_eq!(assessment.severity_confidence, Some(0.6));
    }

    #[cfg(feature = "decision")]
    #[test]
    fn assess_finding_is_none_when_the_endpoint_answers_nothing() {
        let decision = FakeDecision {
            answers: serde_json::json!({"support": {"choice": "Z"}}),
        };
        assert!(assess_finding(&decision, &sample_finding(), "context").is_none());
    }

    #[cfg(feature = "decision")]
    #[test]
    fn report_finding_records_an_advisory_assessment() {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "--quiet"]);
        git(repo.path(), &["config", "user.email", "review@example.com"]);
        git(repo.path(), &["config", "user.name", "Review Test"]);
        std::fs::write(repo.path().join("hello.txt"), "before\n").unwrap();
        git(repo.path(), &["add", "hello.txt"]);
        git(repo.path(), &["commit", "--quiet", "-m", "initial"]);
        std::fs::write(repo.path().join("hello.txt"), "after\n").unwrap();

        let tools =
            super::super::CodeTools::default().with_decision(std::sync::Arc::new(FakeDecision {
                answers: serde_json::json!({
                    "support": {"choice": "A", "answer_confidence": 0.7},
                    "severity": {"choice": "P1", "answer_confidence": 0.5},
                }),
            }));
        let opened = start_review(
            &tools,
            StartReviewParams {
                cwd: repo.path().to_path_buf(),
                base: None,
                commit: None,
            },
        )
        .unwrap();
        let outcome = report_finding(
            &tools,
            ReportFindingParams {
                review_id: opened.review_id.clone(),
                title: "[P1] Keep the original greeting".into(),
                body: "When callers expect `before`, returning `after` changes the contract."
                    .into(),
                priority: ReviewPriority::P1,
                confidence: 0.95,
                path: "hello.txt".into(),
                line_start: 1,
                line_end: 1,
                rule_uris: Vec::new(),
                suggestion: None,
            },
        )
        .unwrap();
        let assessment = outcome.assessment.expect("assessment recorded");
        assert_eq!(assessment.support.as_deref(), Some("supported"));
        assert_eq!(assessment.severity.as_deref(), Some("P1"));

        // The stored finding carries it for later audit through review_findings.
        let findings = review_findings(
            &tools,
            ReviewFindingsParams {
                review_id: opened.review_id,
                priority: None,
                path_contains: None,
            },
        )
        .unwrap();
        assert_eq!(
            findings.findings[0]
                .assessment
                .as_ref()
                .and_then(|a| a.support.as_deref()),
            Some("supported")
        );
    }
}
