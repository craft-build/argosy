//! In-process review sessions over immutable repository diffs.
//!
//! `start_review` snapshots the diff; the other tools read that exact
//! snapshot and attach or retrieve findings.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(feature = "mcp")]
use rmcp::schemars;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use snafu::{OptionExt, ResultExt};

use crate::bundle::Namespace;
use crate::context::{ArgosyRef, ProjectContext, QualifiedConceptId};
use crate::error::{CodeToolSnafu, Error, Result, ReviewEncodingSnafu, ReviewIoSnafu};
use crate::trust::{Origin, TrustPolicy};

use super::diffhunks::{self, FileHunks};
use super::review_export;

const MAX_DIFF_BYTES: usize = 2 * 1024 * 1024;
/// Default review-session idle TTL.
const DEFAULT_REVIEW_TTL: Duration = Duration::from_secs(7200);
/// Default review-session cap.
const DEFAULT_MAX_REVIEWS: usize = 16;
/// Pre-pass flood caps: per rule per file, and across one whole review.
const MAX_PATTERN_FINDINGS_PER_RULE_FILE: usize = 10;
const MAX_PATTERN_FINDINGS: usize = 200;
/// Quoted-line cap inside pre-pass finding bodies.
const MAX_QUOTED_LINE_CHARS: usize = 200;
/// Cap on structured edits per finding.
const MAX_EDITS_PER_FINDING: usize = 8;
/// Cap on one edit's replacement text, matching the `suggestion` cap.
const MAX_EDIT_CHARS: usize = 10_000;
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
    /// As returned by `start_review`; see [`StartReviewOutcome::review_id`].
    pub review_id: String,
    /// Repository-relative changed path. Omit to list the files in the snapshot.
    pub path: Option<String>,
}

#[cfg_attr(feature = "mcp", derive(rmcp::schemars::JsonSchema))]
#[derive(Debug, Deserialize)]
pub struct ReportFindingParams {
    /// As returned by `start_review`; see [`StartReviewOutcome::review_id`].
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
    /// Machine-applicable fixes: inclusive line ranges inside the finding's
    /// own range plus replacement text.
    #[serde(default)]
    pub edits: Vec<FindingEdit>,
}

/// One machine-applicable fix: replace the inclusive 1-indexed line range
/// with `replacement`.
///
/// An empty replacement deletes the lines.
#[cfg_attr(feature = "mcp", derive(rmcp::schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingEdit {
    /// First line of the replaced range (1-indexed, inclusive).
    pub line_start: u32,
    /// Last line of the replaced range (inclusive).
    pub line_end: u32,
    /// The text the replaced lines become.
    pub replacement: String,
}

/// The rendering of a `review_findings` call: the structured JSON listing
/// (default), or a PR-ready export document.
#[cfg_attr(feature = "mcp", derive(rmcp::schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    /// The default structured listing; no export document.
    Json,
    /// A SARIF 2.1.0 log for GitHub code scanning and SARIF-aware tooling.
    Sarif,
    /// reviewdog rdjsonl, one Diagnostic JSON object per line, for the
    /// `github-pr-review` and `gitlab-mr-discussion` reporters.
    Rdjsonl,
}

#[cfg_attr(feature = "mcp", derive(rmcp::schemars::JsonSchema))]
#[derive(Debug, Deserialize)]
pub struct ReviewFindingsParams {
    /// As returned by `start_review`; see [`StartReviewOutcome::review_id`].
    pub review_id: String,
    /// Return only this priority.
    pub priority: Option<ReviewPriority>,
    /// Return only findings whose repository-relative path contains this text.
    pub path_contains: Option<String>,
    /// Render an export document (`sarif` or `rdjsonl`) in addition to the
    /// findings listing; `json` (the default) adds none.
    pub format: Option<ExportFormat>,
}

#[cfg_attr(feature = "mcp", derive(rmcp::schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize)]
pub struct StartReviewOutcome {
    /// Opaque id for this review session; pass it to `review_diff`,
    /// `report_finding`, and `review_findings`.
    pub review_id: String,
    pub repository: String,
    pub comparison: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    pub files_changed: usize,
    pub changed_files: Vec<String>,
    /// The deterministic `pattern` pre-pass; absent when no review
    /// grounding is configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prepass: Option<PrepassOutcome>,
}

/// The deterministic `pattern` pre-pass result of one `start_review`.
#[cfg_attr(feature = "mcp", derive(rmcp::schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum PrepassOutcome {
    /// The pre-pass ran; its findings are recorded with `origin: "pattern"`.
    Ran {
        /// Pattern rules that ran over the added lines.
        rules: usize,
        /// Pattern rules skipped because their regex failed to compile.
        invalid_patterns: usize,
        /// Findings recorded into the session.
        findings: usize,
        /// True when flood caps dropped further matches.
        truncated: bool,
    },
    /// Review grounding was configured but could not be opened, so no
    /// pre-pass ran. Reviews still work; only the deterministic pass and
    /// rule-URI existence checks are unavailable.
    Unavailable {
        /// Why grounding failed, for the caller to relay or ignore.
        reason: String,
    },
}

/// One compiled deterministic check, from a styleguide rule's `pattern`
/// frontmatter.
#[derive(Debug, Clone)]
pub struct PatternRule {
    /// The rule's qualified `argosy://` URI.
    pub uri: String,
    /// Origin argosy manifest name.
    pub argosy: String,
    /// `"local"` or `"imported"`.
    pub kind: &'static str,
    /// The rule's effective trust tier (provenance-capped for imports).
    pub trust: String,
    /// Short label for finding titles: `rule_id` when set, else the
    /// concept id's last segment.
    pub label: String,
    /// The rule's `description`, quoted in finding bodies.
    pub description: String,
    /// The rule's `priority` word (`error`/`warn`/`info`), when set.
    pub priority: Option<String>,
    /// The compiled `pattern` regex.
    pub regex: regex::Regex,
}

/// The review grounding of one project: every styleguide-rule URI, plus the
/// deterministic subset carrying compiled `pattern` regexes.
///
/// Built from a project's argosy set (local plus imports, in precedence
/// order); [`ReviewRuleSet::from_context`] is the constructor hosts wire
/// through [`CodeTools::with_review_grounding`](super::CodeTools::with_review_grounding).
#[derive(Debug, Clone, Default)]
pub struct ReviewRuleSet {
    /// Rules carrying a valid `pattern`, in listing (precedence) order.
    pub rules: Vec<PatternRule>,
    /// Every styleguide-rule URI across all active argosys — the set a
    /// finding's `rule_uris` entries must come from.
    pub rule_uris: HashSet<String>,
    /// Rules whose `pattern` failed to compile; the pre-pass skips them.
    pub invalid_patterns: usize,
}

impl ReviewRuleSet {
    /// Collects one opened project's grounding from its argosy set. A rule
    /// with an invalid `pattern` is counted in
    /// [`invalid_patterns`](Self::invalid_patterns), never an error.
    pub fn from_context(context: &ProjectContext, trust: &TrustPolicy) -> Result<Self> {
        let mut set = Self::default();
        for listing in context.list_rules(None, None)? {
            let uri = format!("argosy://{}/{}", listing.argosy, listing.rule.id().as_str());
            set.rule_uris.insert(uri.clone());
            let Some(pattern) = listing
                .rule
                .pattern()
                .map(str::trim)
                .filter(|pattern| !pattern.is_empty())
            else {
                continue;
            };
            let Ok(regex) = regex::Regex::new(pattern) else {
                set.invalid_patterns += 1;
                continue;
            };
            let origin = match context.argosy_named(&listing.argosy) {
                Some(ArgosyRef::Local(_)) => (Origin::Local, "local"),
                _ => (Origin::Imported, "imported"),
            };
            let label = listing
                .rule
                .rule_id()
                .map(str::to_string)
                .unwrap_or_else(|| tail_segment(listing.rule.id().as_str()));
            set.rules.push(PatternRule {
                uri,
                argosy: listing.argosy.clone(),
                kind: origin.1,
                trust: trust.effective_for(origin.0, &listing.argosy, listing.rule.concept()),
                label,
                description: listing
                    .rule
                    .concept()
                    .description()
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
                priority: listing
                    .rule
                    .priority()
                    .map(str::trim)
                    .filter(|priority| !priority.is_empty())
                    .map(str::to_string),
                regex,
            });
        }
        Ok(set)
    }
}

/// The last `/`-separated segment of a concept id.
fn tail_segment(id: &str) -> String {
    id.rsplit('/').next().unwrap_or(id).to_string()
}

/// Opens a project's review grounding by project root.
///
/// The argosy MCP server installs one over its shared session cache;
/// returned errors mean "grounding unavailable" and degrade the review
/// (no pre-pass, no rule-URI existence checks), never fail it.
pub type ReviewGrounding = Arc<dyn Fn(&Path) -> Result<ReviewRuleSet> + Send + Sync>;

#[cfg_attr(feature = "mcp", derive(rmcp::schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
pub enum ReviewPriority {
    P0,
    P1,
    P2,
    P3,
}

#[cfg_attr(feature = "mcp", derive(rmcp::schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingOrigin {
    /// Reported through `report_finding` by the reviewing agent.
    #[default]
    Agent,
    /// Emitted by the deterministic `pattern` pre-pass at `start_review`;
    /// always confidence 1.0 with a rule URI attached.
    Pattern,
}

#[cfg_attr(feature = "mcp", derive(rmcp::schemars::JsonSchema))]
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
    #[serde(default)]
    pub rule_uris: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
    /// Machine-applicable fixes inside the finding's line range; empty on
    /// pattern findings.
    #[serde(default)]
    pub edits: Vec<FindingEdit>,
    /// Advisory assessment from the decision endpoint, when one was enabled at
    /// report time; it never changes the finding's presence or priority.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assessment: Option<FindingAssessment>,
    /// Who recorded the finding (agent report or deterministic pattern
    /// match); defaults to `agent` for findings deserialized without one.
    #[serde(default)]
    pub origin: FindingOrigin,
}

/// An advisory assessment of a finding by the decision endpoint; it never
/// filters or downgrades the finding.
#[cfg_attr(feature = "mcp", derive(rmcp::schemars::JsonSchema))]
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

#[cfg_attr(feature = "mcp", derive(rmcp::schemars::JsonSchema))]
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

#[cfg_attr(feature = "mcp", derive(rmcp::schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize)]
pub struct ReviewFindingsOutcome {
    pub review_id: String,
    pub findings: Vec<ReviewFinding>,
    /// The rendered export document, present only when the caller asked for
    /// `format: "sarif"` or `format: "rdjsonl"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub export: Option<String>,
}

#[cfg_attr(feature = "mcp", derive(rmcp::schemars::JsonSchema))]
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
    /// Optional project-grounding source: compiled rule patterns and the
    /// valid rule-URI set. `None` disables the pre-pass and rule-URI
    /// existence checks.
    grounding: Option<ReviewGrounding>,
}

impl Default for ReviewManager {
    fn default() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            ttl: DEFAULT_REVIEW_TTL,
            max_sessions: DEFAULT_MAX_REVIEWS,
            #[cfg(feature = "decision")]
            decision: None,
            grounding: None,
        }
    }
}

struct Session {
    /// Repository root — the review-grounding key.
    root: PathBuf,
    diff: String,
    changed_files: Vec<String>,
    /// Per-file hunk structure (added lines, new-line ranges), parsed once
    /// from the immutable snapshot.
    hunks: HashMap<String, FileHunks>,
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

    /// Installs the project-grounding source (see [`ReviewGrounding`]).
    pub fn with_grounding(mut self, grounding: ReviewGrounding) -> Self {
        self.grounding = Some(grounding);
        self
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
        let hunks = diffhunks::parse(&diff);
        let (prepass, prepass_findings) = match self.grounding.as_ref() {
            Some(grounding) => match grounding(&root) {
                Ok(set) => {
                    let (findings, truncated) = run_prepass(&set, &hunks, &id);
                    let report = PrepassOutcome::Ran {
                        rules: set.rules.len(),
                        invalid_patterns: set.invalid_patterns,
                        findings: findings.len(),
                        truncated,
                    };
                    (Some(report), findings)
                }
                Err(reason) => (
                    Some(PrepassOutcome::Unavailable {
                        reason: format!("{reason:#}"),
                    }),
                    Vec::new(),
                ),
            },
            None => (None, Vec::new()),
        };

        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        Self::purge_expired(&mut sessions, self.ttl);
        Self::evict_for_capacity(&mut sessions, self.max_sessions);
        sessions.insert(
            id.clone(),
            Session {
                root,
                diff,
                changed_files: changed_files.clone(),
                hunks: hunks
                    .into_iter()
                    .map(|file| (file.path.clone(), file))
                    .collect(),
                findings: prepass_findings,
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
            prepass,
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
        let (review_id, finding) = validate_finding(params)?;
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        Self::purge_expired(&mut sessions, self.ttl);
        let session = Self::touch(&mut sessions, &review_id)?;
        // Grounding checks before the finding exists: the path and lines
        // must come from this snapshot, and each rule URI must resolve.
        // (`validate_rule_uris` may invoke the grounding closure, which
        // takes the knowledge-session lock; lock order is always reviews →
        // knowledge, and the knowledge path never takes the reviews lock.)
        Self::validate_against_snapshot(session, &finding)?;
        self.validate_rule_uris(&session.root, &finding.rule_uris)?;
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
        let assessment = if created
            && let Some(decision) = self.decision.as_ref().filter(|d| d.is_enabled())
            && let Some(assessment) = assess_finding(
                decision.as_ref(),
                &finding,
                hunk.as_deref().unwrap_or_default(),
            ) {
            Some(assessment)
        } else {
            None
        };
        #[cfg(not(feature = "decision"))]
        let assessment: Option<FindingAssessment> = None;
        if let Some(assessment) = &assessment {
            let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(session) = sessions.get_mut(&review_id)
                && let Some(stored) = session
                    .findings
                    .iter_mut()
                    .find(|existing| existing.finding_id == finding.finding_id)
            {
                stored.assessment = Some(assessment.clone());
            }
        }

        Ok(ReportFindingOutcome {
            review_id,
            finding_id: finding.finding_id,
            created,
            finding_count,
            assessment,
        })
    }

    fn findings(&self, params: ReviewFindingsParams) -> Result<ReviewFindingsOutcome> {
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        Self::purge_expired(&mut sessions, self.ttl);
        let session = Self::touch(&mut sessions, &params.review_id)?;
        let findings: Vec<ReviewFinding> = session
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
        // The export renders the filtered set, not the whole session.
        let export = match params.format {
            Some(ExportFormat::Sarif) => {
                Some(review_export::to_sarif(&params.review_id, &findings))
            }
            Some(ExportFormat::Rdjsonl) => Some(review_export::to_rdjsonl(&findings)),
            None | Some(ExportFormat::Json) => None,
        };
        Ok(ReviewFindingsOutcome {
            review_id: params.review_id,
            findings,
            export,
        })
    }

    /// A finding must sit inside the snapshot it is reported against: its
    /// path must be a changed file, and its line range inside one hunk's
    /// new-file line range, so no invented file or line survives.
    fn validate_against_snapshot(session: &Session, finding: &ReviewFinding) -> Result<()> {
        if !session
            .changed_files
            .iter()
            .any(|path| path == &finding.path)
        {
            return CodeToolSnafu {
                message: format!(
                    "path `{}` is not in this review snapshot; call review_diff without path \
                     to list the changed files",
                    finding.path
                ),
            }
            .fail();
        }
        let Some(file) = session.hunks.get(&finding.path) else {
            return CodeToolSnafu {
                message: format!(
                    "no diff hunks for `{}` in this snapshot; findings must point at changed \
                     lines",
                    finding.path
                ),
            }
            .fail();
        };
        let covered = file
            .new_ranges
            .iter()
            .any(|&(start, end)| finding.line_start >= start && finding.line_end <= end);
        if !covered {
            let ranges = if file.new_ranges.is_empty() {
                "none — the file has only removed lines".to_string()
            } else {
                file.new_ranges
                    .iter()
                    .map(|&(start, end)| format!("{start}-{end}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            return CodeToolSnafu {
                message: format!(
                    "line range {}-{} of `{}` lies outside the snapshot's changed hunks \
                     (new-file line ranges: {ranges}); point the finding at lines the diff \
                     actually covers",
                    finding.line_start, finding.line_end, finding.path
                ),
            }
            .fail();
        }
        Ok(())
    }

    /// Every `rule_uris` entry must parse as a styleguide-rule URI and,
    /// when grounding is available, resolve to a rule in this project's
    /// argosys — so findings cannot cite invented rules.
    fn validate_rule_uris(&self, root: &Path, uris: &[String]) -> Result<()> {
        if uris.is_empty() {
            return Ok(());
        }
        // Grounding is optional: without it (or when the project has no
        // argosy) existence cannot be checked, but syntax and namespace
        // still can.
        let set = self
            .grounding
            .as_ref()
            .and_then(|grounding| grounding(root).ok());
        for uri in uris {
            let qid = QualifiedConceptId::from_uri(uri).map_err(|err| Error::CodeTool {
                message: format!(
                    "invalid rule_uris entry `{uri}`: {err:#}; use a uri returned by \
                     search_rules",
                ),
            })?;
            if qid.namespace != Namespace::Styleguide {
                return CodeToolSnafu {
                    message: format!(
                        "rule_uris entry `{uri}` is not a styleguide rule; ground findings \
                         in rules returned by search_rules, or report the finding ungrounded \
                         with an empty rule_uris",
                    ),
                }
                .fail();
            }
            if let Some(set) = &set
                && !set.rule_uris.contains(uri)
            {
                return CodeToolSnafu {
                    message: format!(
                        "rule_uris entry `{uri}` does not resolve to a styleguide rule in \
                         this project's argosys; use a uri returned by search_rules (call \
                         start_review again if the book changed since this review started)",
                    ),
                }
                .fail();
            }
        }
        Ok(())
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
    validate_edits(&params.edits, params.line_start, params.line_end)?;

    let mut finding = ReviewFinding {
        finding_id: String::new(),
        title: title.to_string(),
        body: body.to_string(),
        priority: params.priority,
        confidence: params.confidence,
        path: path.to_string(),
        line_start: params.line_start,
        line_end: params.line_end,
        rule_uris: params.rule_uris,
        suggestion,
        edits: params.edits,
        assessment: None,
        origin: FindingOrigin::Agent,
    };
    let review_id = params.review_id;
    finding.finding_id = finding_id(&review_id, &finding);
    Ok((review_id, finding))
}

/// Validates a finding's edits: inclusive ranges inside the finding's own
/// range (which grounding already confines to a changed hunk), non-overlapping,
/// within the caps. An empty replacement (deleting lines) is valid.
fn validate_edits(edits: &[FindingEdit], line_start: u32, line_end: u32) -> Result<()> {
    if edits.len() > MAX_EDITS_PER_FINDING {
        return CodeToolSnafu {
            message: format!(
                "invalid finding edits: at most {MAX_EDITS_PER_FINDING} per finding, got {}",
                edits.len()
            ),
        }
        .fail();
    }
    let mut sorted: Vec<&FindingEdit> = edits.iter().collect();
    sorted.sort_by_key(|edit| (edit.line_start, edit.line_end));
    for pair in sorted.windows(2) {
        let (first, second) = (pair[0], pair[1]);
        if second.line_start <= first.line_end {
            return CodeToolSnafu {
                message: format!(
                    "invalid finding edits: ranges {}-{} and {}-{} overlap",
                    first.line_start, first.line_end, second.line_start, second.line_end
                ),
            }
            .fail();
        }
    }
    for edit in sorted {
        if edit.line_start == 0
            || edit.line_end < edit.line_start
            || edit.line_start < line_start
            || edit.line_end > line_end
            || edit.replacement.len() > MAX_EDIT_CHARS
        {
            return CodeToolSnafu {
                message: format!(
                    "invalid finding edits: each edit must be an inclusive 1-indexed range \
                     inside the finding's range {line_start}-{line_end} with replacement text \
                     up to {MAX_EDIT_CHARS} bytes (an empty replacement deletes the lines)"
                ),
            }
            .fail();
        }
    }
    Ok(())
}

/// The deduplication key of one finding within its review: every field,
/// plus the origin, so an agent re-report of a pattern finding stays
/// distinct (and vice versa).
fn finding_id(review_id: &str, finding: &ReviewFinding) -> String {
    let mut hasher = Sha256::new();
    hasher.update(match finding.origin {
        FindingOrigin::Agent => &b"agent"[..],
        FindingOrigin::Pattern => &b"pattern"[..],
    });
    hasher.update([0]);
    for value in [
        review_id,
        &finding.title,
        &finding.body,
        finding.priority.as_str(),
        &finding.path,
        &finding.line_start.to_string(),
        &finding.line_end.to_string(),
        &finding.confidence.to_bits().to_string(),
    ] {
        hasher.update(value.as_bytes());
        hasher.update([0]);
    }
    for uri in &finding.rule_uris {
        hasher.update(uri.as_bytes());
        hasher.update([0]);
    }
    if let Some(value) = &finding.suggestion {
        hasher.update(value.as_bytes());
    }
    for edit in &finding.edits {
        hasher.update(edit.line_start.to_string().as_bytes());
        hasher.update([0]);
        hasher.update(edit.line_end.to_string().as_bytes());
        hasher.update([0]);
        hasher.update(edit.replacement.as_bytes());
        hasher.update([0]);
    }
    hasher.finalize()[..12]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
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

/// A rule `priority` word mapped onto a finding priority: `error` → P1,
/// `warn` (and absent/unknown) → P2, `info` → P3. A line-level regex can
/// establish a style violation, never the security or data-loss case P0
/// is reserved for.
fn pattern_priority(priority: Option<&str>) -> ReviewPriority {
    match priority {
        Some("error") => ReviewPriority::P1,
        Some("info") => ReviewPriority::P3,
        _ => ReviewPriority::P2,
    }
}

/// Runs the compiled rule patterns over each file's added lines, grouping
/// consecutive matched lines into one finding per run. Returns the findings
/// and whether flood caps truncated the set.
fn run_prepass(
    set: &ReviewRuleSet,
    hunks: &[FileHunks],
    review_id: &str,
) -> (Vec<ReviewFinding>, bool) {
    let mut findings = Vec::new();
    let mut truncated = false;
    'rules: for rule in &set.rules {
        for file in hunks {
            let matched: Vec<(u32, String)> = file
                .added
                .iter()
                .filter(|(_, text)| rule.regex.is_match(text))
                .cloned()
                .collect();
            // Contiguous line numbers collapse into one run (one finding).
            let mut runs: Vec<&[(u32, String)]> = Vec::new();
            let mut start = 0;
            for index in 1..=matched.len() {
                if index == matched.len() || matched[index].0 != matched[index - 1].0 + 1 {
                    runs.push(&matched[start..index]);
                    start = index;
                }
            }
            for (recorded, run) in runs.into_iter().enumerate() {
                if findings.len() >= MAX_PATTERN_FINDINGS {
                    truncated = true;
                    break 'rules;
                }
                if recorded >= MAX_PATTERN_FINDINGS_PER_RULE_FILE {
                    truncated = true;
                    break;
                }
                findings.push(pattern_finding(rule, &file.path, run, review_id));
            }
        }
    }
    (findings, truncated)
}

/// One grouped pattern-match run as a recorded finding: deterministic, so
/// confidence is 1.0 and the rule URI is always attached.
fn pattern_finding(
    rule: &PatternRule,
    path: &str,
    run: &[(u32, String)],
    review_id: &str,
) -> ReviewFinding {
    let priority = pattern_priority(rule.priority.as_deref());
    let quoted: String = run
        .iter()
        .map(|(no, text)| format!("  {no}: {}\n", cap_line(text, MAX_QUOTED_LINE_CHARS)))
        .collect();
    let mut finding = ReviewFinding {
        finding_id: String::new(),
        title: format!(
            "[{}] `{}` pattern match in `{}`",
            priority.as_str(),
            rule.label,
            path
        ),
        body: format!(
            "Deterministic match of styleguide rule {} (argosy `{}`, {}, trust `{}`).\n\n{}\n\n\
             Matched added lines:\n{}The `pattern` is line-level: read the surrounding code \
             and the rule's Good/Bad examples before acting; a matching line is not always a \
             violation.",
            rule.uri, rule.argosy, rule.kind, rule.trust, rule.description, quoted
        ),
        priority,
        confidence: 1.0,
        path: path.to_string(),
        line_start: run.first().map(|(no, _)| *no).unwrap_or(1),
        line_end: run.last().map(|(no, _)| *no).unwrap_or(1),
        rule_uris: vec![rule.uri.clone()],
        suggestion: None,
        edits: Vec::new(),
        assessment: None,
        origin: FindingOrigin::Pattern,
    };
    finding.finding_id = finding_id(review_id, &finding);
    finding
}

/// `text` shortened to `max` characters plus an ellipsis.
fn cap_line(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut cut: String = text.chars().take(max - 1).collect();
        cut.push('…');
        cut
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
    diff.lines()
        .filter_map(diffhunks::diff_header_path)
        .collect()
}

fn diff_for_file(diff: &str, wanted: &str) -> Option<String> {
    let mut selected = false;
    let mut out = String::new();
    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            if selected {
                break;
            }
            selected = diffhunks::diff_header_path(line).as_deref() == Some(wanted);
        }
        if selected {
            out.push_str(line);
            out.push('\n');
        }
    }
    (!out.is_empty()).then_some(out)
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
                format: None,
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
                format: None,
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
                edits: Vec::new(),
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
                edits: Vec::new(),
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
                format: None,
            },
        )
        .unwrap();
        assert_eq!(findings.findings.len(), 1);
    }

    /// A one-rule grounding set: `uris` are the resolvable rule URIs.
    fn rule_set(rules: Vec<PatternRule>, uris: &[&str]) -> ReviewRuleSet {
        ReviewRuleSet {
            rules,
            rule_uris: uris.iter().map(|uri| uri.to_string()).collect(),
            invalid_patterns: 0,
        }
    }

    fn pattern_rule(uri: &str, pattern: &str, priority: Option<&str>) -> PatternRule {
        PatternRule {
            uri: uri.to_string(),
            argosy: "test-book".to_string(),
            kind: "local",
            trust: "unverified".to_string(),
            label: "no-forbidden-token".to_string(),
            description: "Never write the forbidden token.".to_string(),
            priority: priority.map(str::to_string),
            regex: regex::Regex::new(pattern).unwrap(),
        }
    }

    fn grounded_tools(set: ReviewRuleSet) -> super::super::CodeTools {
        super::super::CodeTools::default()
            .with_review_grounding(Arc::new(move |_root| Ok(set.clone())))
    }

    #[test]
    fn prepass_records_grouped_pattern_findings() {
        // Three consecutive added lines matching one rule → one grouped finding.
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "--quiet"]);
        git(repo.path(), &["config", "user.email", "review@example.com"]);
        git(repo.path(), &["config", "user.name", "Review Test"]);
        std::fs::write(repo.path().join("notes.txt"), "clean\n").unwrap();
        git(repo.path(), &["add", "notes.txt"]);
        git(repo.path(), &["commit", "--quiet", "-m", "initial"]);
        std::fs::write(
            repo.path().join("notes.txt"),
            "clean\nTODO a\nTODO b\nTODO c\n",
        )
        .unwrap();

        let uri = "argosy://test-book/styleguide/no-todos";
        let tools = grounded_tools(rule_set(
            vec![pattern_rule(uri, r"\bTODO\b", Some("error"))],
            &[uri],
        ));
        let opened = start_review(
            &tools,
            StartReviewParams {
                cwd: repo.path().to_path_buf(),
                base: None,
                commit: None,
            },
        )
        .unwrap();
        assert_eq!(
            opened.prepass,
            Some(PrepassOutcome::Ran {
                rules: 1,
                invalid_patterns: 0,
                findings: 1,
                truncated: false,
            }),
            "three consecutive matches group into one finding"
        );
        let findings = review_findings(
            &tools,
            ReviewFindingsParams {
                review_id: opened.review_id,
                priority: None,
                path_contains: None,
                format: None,
            },
        )
        .unwrap();
        assert_eq!(findings.findings.len(), 1);
        let finding = &findings.findings[0];
        assert_eq!(finding.origin, FindingOrigin::Pattern);
        assert_eq!(finding.priority, ReviewPriority::P1, "error → P1");
        assert_eq!(finding.path, "notes.txt");
        assert_eq!((finding.line_start, finding.line_end), (2, 4));
        assert_eq!(finding.confidence, 1.0);
        assert_eq!(finding.rule_uris, vec![uri.to_string()]);
        assert!(
            finding.title.starts_with("[P1] "),
            "pattern findings follow the title convention: {}",
            finding.title
        );
    }

    #[test]
    fn pattern_priority_words_map_onto_finding_priorities() {
        assert_eq!(pattern_priority(Some("error")), ReviewPriority::P1);
        assert_eq!(pattern_priority(Some("warn")), ReviewPriority::P2);
        assert_eq!(pattern_priority(Some("info")), ReviewPriority::P3);
        assert_eq!(pattern_priority(None), ReviewPriority::P2);
        // P0 stays reserved for the security/data-loss case a line regex
        // cannot establish, and unknown words read as warn.
        assert_eq!(pattern_priority(Some("banana")), ReviewPriority::P2);
    }

    #[test]
    fn prepass_caps_floods_and_reports_truncation() {
        // 12 separated matching lines exceed the per-rule-per-file cap of 10.
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "--quiet"]);
        git(repo.path(), &["config", "user.email", "review@example.com"]);
        git(repo.path(), &["config", "user.name", "Review Test"]);
        std::fs::write(repo.path().join("flood.txt"), "seed\n").unwrap();
        git(repo.path(), &["add", "flood.txt"]);
        git(repo.path(), &["commit", "--quiet", "-m", "initial"]);
        // Matches on every other line: 12 runs, none consecutive.
        let after: String = (0..12).map(|n| format!("TODO {n}\nplain {n}\n")).collect();
        std::fs::write(repo.path().join("flood.txt"), after).unwrap();

        let uri = "argosy://test-book/styleguide/no-todos";
        let tools = grounded_tools(rule_set(vec![pattern_rule(uri, r"\bTODO\b", None)], &[uri]));
        let opened = start_review(
            &tools,
            StartReviewParams {
                cwd: repo.path().to_path_buf(),
                base: None,
                commit: None,
            },
        )
        .unwrap();
        match opened.prepass {
            Some(PrepassOutcome::Ran {
                findings,
                truncated,
                ..
            }) => {
                assert_eq!(findings, 10, "the per-rule-per-file cap");
                assert!(truncated);
            }
            other => panic!("expected a truncated run, got {other:?}"),
        }
    }

    #[test]
    fn prepass_reports_unavailable_grounding_but_still_opens_the_review() {
        let repo = review_repo();
        let tools = super::super::CodeTools::default().with_review_grounding(Arc::new(|_root| {
            Err(Error::Validation {
                reason: "no argosy for this project".into(),
            })
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
        match opened.prepass {
            Some(PrepassOutcome::Unavailable { reason }) => {
                assert!(reason.contains("no argosy"), "got: {reason}");
            }
            other => panic!("expected unavailable grounding, got {other:?}"),
        }
        // The session still works for manual review.
        review_diff(
            &tools,
            ReviewDiffParams {
                review_id: opened.review_id,
                path: None,
            },
        )
        .unwrap();
    }

    fn agent_finding(
        opened: &StartReviewOutcome,
        path: &str,
        lines: (u32, u32),
    ) -> ReportFindingParams {
        ReportFindingParams {
            review_id: opened.review_id.clone(),
            title: "[P1] Keep the original greeting".into(),
            body: "When callers expect `before`, returning `after` changes the contract.".into(),
            priority: ReviewPriority::P1,
            confidence: 0.95,
            path: path.into(),
            line_start: lines.0,
            line_end: lines.1,
            rule_uris: Vec::new(),
            suggestion: None,
            edits: Vec::new(),
        }
    }

    #[test]
    fn report_finding_rejects_paths_outside_the_snapshot() {
        let repo = review_repo();
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
        let err =
            report_finding(&tools, agent_finding(&opened, "invented.txt", (1, 1))).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("not in this review snapshot"), "{message}");
        assert!(
            message.contains("review_diff without path"),
            "the error must say how to list valid paths: {message}"
        );
    }

    #[test]
    fn report_finding_rejects_lines_outside_the_changed_hunks() {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "--quiet"]);
        git(repo.path(), &["config", "user.email", "review@example.com"]);
        git(repo.path(), &["config", "user.name", "Review Test"]);
        // Six lines; only the last changes, so the hunk (with its three
        // context lines) covers new-file lines 3-6 — line 1 is untouched.
        std::fs::write(repo.path().join("hello.txt"), "a\nb\nc\nd\ne\nf\n").unwrap();
        git(repo.path(), &["add", "hello.txt"]);
        git(repo.path(), &["commit", "--quiet", "-m", "initial"]);
        std::fs::write(repo.path().join("hello.txt"), "a\nb\nc\nd\ne\ng\n").unwrap();
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
        // Line 1 exists in the file but no hunk covers it.
        let err = report_finding(&tools, agent_finding(&opened, "hello.txt", (1, 1))).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("outside the snapshot's changed hunks"),
            "{message}"
        );
        assert!(message.contains("3-6"), "names the valid range: {message}");
    }

    #[test]
    fn report_finding_rejects_invented_rule_uris_when_grounding_resolves() {
        let repo = review_repo();
        let uri = "argosy://test-book/styleguide/real-rule";
        let tools = grounded_tools(rule_set(Vec::new(), &[uri]));
        let opened = start_review(
            &tools,
            StartReviewParams {
                cwd: repo.path().to_path_buf(),
                base: None,
                commit: None,
            },
        )
        .unwrap();
        let mut params = agent_finding(&opened, "hello.txt", (1, 1));
        params.rule_uris = vec!["argosy://test-book/styleguide/invented-rule".into()];
        let err = report_finding(&tools, params).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("does not resolve"), "{message}");
        assert!(
            message.contains("search_rules"),
            "the error points at the grounded source: {message}"
        );
    }

    #[test]
    fn report_finding_rejects_non_styleguide_and_malformed_rule_uris_without_grounding() {
        let repo = review_repo();
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
        for uri in [
            // A document URI is well-formed but not a rule.
            "argosy://book/document/architecture",
            // Non-canonical spelling is rejected by the URI grammar.
            "argosy://book/styleguide/rule.md",
        ] {
            let mut params = agent_finding(&opened, "hello.txt", (1, 1));
            params.rule_uris = vec![uri.into()];
            let err = report_finding(&tools, params).unwrap_err();
            assert!(
                err.to_string().contains(uri),
                "the error names the offending uri: {}",
                err
            );
        }
    }

    #[test]
    fn report_finding_accepts_snapshot_grounded_findings() {
        let repo = review_repo();
        let uri = "argosy://test-book/styleguide/greeting-contract";
        let tools = grounded_tools(rule_set(Vec::new(), &[uri]));
        let opened = start_review(
            &tools,
            StartReviewParams {
                cwd: repo.path().to_path_buf(),
                base: None,
                commit: None,
            },
        )
        .unwrap();
        let mut params = agent_finding(&opened, "hello.txt", (1, 1));
        params.rule_uris = vec![uri.into()];
        let outcome = report_finding(&tools, params).unwrap();
        assert!(outcome.created);
    }

    #[test]
    fn report_finding_rejects_edits_outside_the_finding_range() {
        let repo = review_repo();
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
        let mut params = agent_finding(&opened, "hello.txt", (1, 1));
        params.edits = vec![FindingEdit {
            line_start: 1,
            line_end: 2,
            replacement: "before\n".into(),
        }];
        let err = report_finding(&tools, params).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("inside the finding's range 1-1"),
            "got: {message}"
        );
    }

    #[test]
    fn report_finding_rejects_overlapping_edits() {
        let repo = review_repo();
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
        let mut params = agent_finding(&opened, "hello.txt", (1, 1));
        params.edits = vec![
            FindingEdit {
                line_start: 1,
                line_end: 1,
                replacement: "a\n".into(),
            },
            FindingEdit {
                line_start: 1,
                line_end: 1,
                replacement: "b\n".into(),
            },
        ];
        let err = report_finding(&tools, params).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("overlap"), "got: {message}");
        assert!(message.contains("1-1 and 1-1"), "got: {message}");
    }

    #[test]
    fn distinct_edits_are_not_deduplicated() {
        let repo = review_repo();
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
        for replacement in ["before\n", "before\n\n"] {
            let mut params = agent_finding(&opened, "hello.txt", (1, 1));
            params.edits = vec![FindingEdit {
                line_start: 1,
                line_end: 1,
                replacement: replacement.into(),
            }];
            let outcome = report_finding(&tools, params).unwrap();
            assert!(outcome.created, "replacement {replacement:?} is distinct");
        }
        let findings = review_findings(
            &tools,
            ReviewFindingsParams {
                review_id: opened.review_id,
                priority: None,
                path_contains: None,
                format: None,
            },
        )
        .unwrap();
        assert_eq!(findings.findings.len(), 2, "edits join the dedup key");
    }

    #[test]
    fn review_findings_exports_sarif_and_rdjsonl() {
        let repo = review_repo();
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
        let mut params = agent_finding(&opened, "hello.txt", (1, 1));
        params.rule_uris = vec!["argosy://test-book/styleguide/contracts".into()];
        params.edits = vec![FindingEdit {
            line_start: 1,
            line_end: 1,
            replacement: "before\n".into(),
        }];
        report_finding(&tools, params).unwrap();

        let plain = review_findings(
            &tools,
            ReviewFindingsParams {
                review_id: opened.review_id.clone(),
                priority: None,
                path_contains: None,
                format: None,
            },
        )
        .unwrap();
        assert!(plain.export.is_none());
        assert_eq!(plain.findings.len(), 1);

        let sarif = review_findings(
            &tools,
            ReviewFindingsParams {
                review_id: opened.review_id.clone(),
                priority: None,
                path_contains: None,
                format: Some(ExportFormat::Sarif),
            },
        )
        .unwrap()
        .export
        .expect("sarif export rendered");
        let log: serde_json::Value = serde_json::from_str(&sarif).unwrap();
        assert_eq!(log["version"], "2.1.0");
        let result = &log["runs"][0]["results"][0];
        assert_eq!(result["ruleId"], "argosy://test-book/styleguide/contracts");
        assert_eq!(result["level"], "error", "P1 maps to error");
        assert_eq!(
            result["fixes"][0]["artifactChanges"][0]["replacements"][0]["insertedContent"]["text"],
            "before\n"
        );
        assert_eq!(
            log["runs"][0]["automationDetails"]["id"],
            format!("argosy/reviews/{}", opened.review_id)
        );

        let rdjsonl = review_findings(
            &tools,
            ReviewFindingsParams {
                review_id: opened.review_id,
                priority: None,
                path_contains: None,
                format: Some(ExportFormat::Rdjsonl),
            },
        )
        .unwrap()
        .export
        .expect("rdjsonl export rendered");
        let lines: Vec<&str> = rdjsonl.lines().collect();
        assert_eq!(lines.len(), 1, "one diagnostic per line: {rdjsonl}");
        let diagnostic: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(diagnostic["severity"], "ERROR");
        assert_eq!(diagnostic["suggestions"][0]["text"], "before\n");
    }

    #[test]
    fn rule_set_collects_compiles_and_caps_trust() {
        use crate::bundle::Namespace;
        use crate::context::ProjectContext;
        use crate::trust::TrustPolicy;

        let write_rule = |root: &Path, id: &str, frontmatter: &str| {
            let argosy = crate::local::LocalArgosy::open(root).unwrap();
            let concept: crate::concept::Concept =
                format!("---\ntype: Styleguide Rule\ndescription: d\n{frontmatter}---\nbody\n")
                    .parse()
                    .unwrap();
            argosy
                .write_concept(Namespace::Styleguide, &id.parse().unwrap(), &concept)
                .unwrap();
        };

        // Local book: a valid pattern with a priority and a verbatim tier, a
        // broken pattern, and a plain rule.
        let local = tempfile::tempdir().unwrap();
        crate::local::LocalArgosy::init(local.path(), Some("local-book"), None).unwrap();
        write_rule(
            local.path(),
            "styleguide/rust/no-todo",
            "pattern: \"\\\\bTODO\\\\b\"\npriority: error\nverified: human-reviewed\n",
        );
        write_rule(
            local.path(),
            "styleguide/rust/broken",
            "pattern: \"(unclosed\"\n",
        );
        write_rule(local.path(), "styleguide/rust/plain", "");

        // Imported book: a pattern rule claiming a tier it is not capped for.
        let imported = tempfile::tempdir().unwrap();
        crate::local::LocalArgosy::init(imported.path(), Some("imported-book"), None).unwrap();
        write_rule(
            imported.path(),
            "styleguide/rust/imported-rule",
            "pattern: \"dbg!\"\nverified: human-reviewed\n",
        );

        let context =
            ProjectContext::open(local.path(), vec![imported.path().to_path_buf()]).unwrap();
        let set = ReviewRuleSet::from_context(&context, &TrustPolicy::new()).unwrap();
        assert_eq!(set.rules.len(), 2, "valid patterns only");
        assert_eq!(set.invalid_patterns, 1, "the broken regex is counted");
        assert_eq!(
            set.rule_uris.len(),
            4,
            "every rule is addressable, pattern or not"
        );

        let no_todo = set
            .rules
            .iter()
            .find(|rule| rule.label == "no-todo")
            .expect("rule_id sets the label");
        assert_eq!(no_todo.kind, "local");
        assert_eq!(no_todo.trust, "human-reviewed", "local claims relay");
        assert_eq!(no_todo.priority.as_deref(), Some("error"));

        let imported_rule = set
            .rules
            .iter()
            .find(|rule| rule.label == "imported-rule")
            .expect("unqualified labels fall back to the id tail");
        assert_eq!(imported_rule.kind, "imported");
        assert_eq!(
            imported_rule.trust, "unverified",
            "an uncapped import never keeps its self-claimed tier"
        );
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
            edits: Vec::new(),
            assessment: None,
            origin: FindingOrigin::Agent,
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
                edits: Vec::new(),
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
                format: None,
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
