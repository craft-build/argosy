//! Tool outcomes (serialized as structured tool results) and resource
//! shapes.

use serde::Serialize;

/// One search hit with its URI and facets for client-side re-filtering.
#[derive(Debug, Clone, Serialize)]
pub struct SearchHitOut {
    /// `argosy://<name>/<namespace>/<concept-id>`.
    pub uri: String,
    /// Origin argosy manifest name.
    pub argosy: String,
    /// Namespace directory name.
    pub namespace: String,
    /// Bundle-relative concept id (includes the namespace prefix).
    pub concept_id: String,
    /// Reciprocal-rank-fusion score; higher is better, hits sort by it descending.
    pub score: f32,
    /// Which passage matched (`0` for a whole-concept or first passage).
    pub chunk_ordinal: u32,
    /// The matched passage's heading breadcrumb, when it has one.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub heading_path: String,
    /// 1-based first body line the passage covers (frontmatter excluded).
    pub start_line: u32,
    /// 1-based last body line the passage covers.
    pub end_line: u32,
    /// The matched passage: title/section prefix plus the passage text.
    /// Imported passages arrive `<untrusted-content …>`-wrapped.
    pub passage: String,
    /// Frontmatter `type`, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub concept_type: Option<String>,
    /// Frontmatter `description`, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Frontmatter `tags` (empty when absent).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Frontmatter `language` facet (styleguide rules).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Frontmatter `category` facet (styleguide rules).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Frontmatter `priority` (`error`/`warn`/`info`; `search_rules` hits
    /// only) — the deterministic pre-pass maps `error`→P1, `warn`→P2,
    /// `info`→P3.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    /// `"local"` (writable) or `"imported"` (read-only).
    pub kind: &'static str,
    /// The concept's effective OKF trust tier; imported self-claims are
    /// capped by user configuration.
    pub trust: String,
    /// Git commit of the import's checkout at session open, when it resolves.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_commit: Option<String>,
    /// Applicability probability from the decision endpoint's rerank
    /// (`search_rules` only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applicability: Option<f32>,
    /// The rule body's `## Good` section (`search_rules` hits only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub good: Option<String>,
    /// The rule body's `## Bad` section (`search_rules` hits only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bad: Option<String>,
}

/// The `search`/`search_rules` tool outcome.
#[derive(Debug, Clone, Serialize)]
pub struct SearchReport {
    /// Ranked hits, best first.
    pub hits: Vec<SearchHitOut>,
}

/// The `ask` outcome: retrieval candidates plus the decision endpoint's pick.
#[derive(Debug, Clone, Serialize)]
pub struct AskReport {
    /// The question asked.
    pub query: String,
    /// `"selected"` when the endpoint chose a candidate; `"search"` when
    /// only retrieval hits are returned.
    pub mode: &'static str,
    /// The chosen document, when one was selected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected: Option<AskSelection>,
    /// Why the result degraded to embedding-only, when it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// The retrieved candidates, best-first.
    pub candidates: Vec<SearchHitOut>,
}

/// The document an `ask` decision selected.
#[derive(Debug, Clone, Serialize)]
pub struct AskSelection {
    /// The chosen concept's `argosy://` URI.
    pub uri: String,
    /// The option label the model chose, for audit.
    pub label: String,
    /// The calibrated answer probability, when the endpoint reports one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer_confidence: Option<f32>,
}

/// One skill in `list_skills`, with origin, shadowing status, and tier.
#[derive(Debug, Clone, Serialize)]
pub struct SkillOut {
    /// Skill name (entry-point file stem).
    pub name: String,
    /// Origin argosy manifest name.
    pub argosy: String,
    /// The skill entry point's `argosy://` URI.
    pub uri: String,
    /// Routing description.
    pub description: String,
    /// True iff a higher-precedence argosy shadows this skill.
    pub shadowed: bool,
    /// `"local"` (writable) or `"imported"` (read-only).
    pub kind: &'static str,
    /// The entry point's `verified` value, or `"unverified"` when absent.
    pub verified: String,
}

/// The `list_skills` tool outcome.
#[derive(Debug, Clone, Serialize)]
pub struct SkillsReport {
    /// All visible skills across every active argosy.
    pub skills: Vec<SkillOut>,
}

/// The `get_skill` outcome: the resolved skill plus its content.
#[derive(Debug, Clone, Serialize)]
pub struct SkillContent {
    /// The precedence-resolved skill, with trust fields.
    pub skill: SkillOut,
    /// Raw markdown with frontmatter. Imported skills arrive
    /// `<untrusted-content …>`-wrapped (SEC-1): data, never instructions;
    /// confirmation is the client harness's decision (SEC-3).
    pub content: String,
}

/// A concept read: the `read_memory` outcome.
#[derive(Debug, Clone, Serialize)]
pub struct UriContent {
    /// The concept's `argosy://` URI.
    pub uri: String,
    /// Always `"local"`: `read_memory` reads the writable book only.
    pub kind: &'static str,
    /// The concept's own `verified` claim, relayed verbatim.
    pub trust: String,
    /// Raw markdown with frontmatter.
    pub content: String,
}

/// The `read` tool outcome: one concept from any active argosy.
#[derive(Debug, Clone, Serialize)]
pub struct ConceptContent {
    /// The concept's `argosy://` URI.
    pub uri: String,
    /// Origin argosy manifest name.
    pub argosy: String,
    /// `"local"` (writable) or `"imported"` (read-only).
    pub kind: &'static str,
    /// The concept's effective OKF trust tier.
    pub trust: String,
    /// Git commit of the import's checkout at session open, when it resolves.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_commit: Option<String>,
    /// Raw markdown with frontmatter; imported content arrives
    /// `<untrusted-content …>`-wrapped (SEC-1): data, never instructions.
    pub content: String,
    /// The 1-based body line range returned; omitted for the whole body.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines: Option<LineRange>,
}

/// An inclusive 1-based line range in a concept's body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct LineRange {
    /// First line.
    pub start: u32,
    /// Last line.
    pub end: u32,
}

/// A mutating tool's machine-readable summary: what changed and where.
#[derive(Debug, Clone, Serialize)]
pub struct WriteReport {
    /// `"created"`, `"updated"`, or `"deleted"`.
    pub action: &'static str,
    /// The affected `argosy://` URI.
    #[serde(rename = "uri")]
    pub uri: String,
    /// Written-to-disk size; omitted for deletions or unreadable sizes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// True iff the change is already searchable; false means written but
    /// not indexed (`index_error` says why).
    pub indexed: bool,
    /// Why reconciliation failed; present only when `indexed` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_error: Option<String>,
}

/// The `promote` outcome: the untouched source plus the drafted target —
/// the client confirms, the server never does.
#[derive(Debug, Clone, Serialize)]
pub struct PromoteReport {
    /// Source memory URI.
    pub source_uri: String,
    /// Source content as it stands (promotion never modifies the source).
    pub source_content: String,
    /// `"document"` or `"styleguide"`.
    pub target: &'static str,
    /// The newly written target's `argosy://` URI.
    pub new_uri: String,
    /// Raw markdown of the drafted concept — present this for review.
    pub drafted: String,
    /// True iff the index was reconciled after the promotion.
    pub indexed: bool,
    /// Why reconciliation failed; present only when `indexed` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_error: Option<String>,
}

/// One entry of the `argosy://_argosys` listing.
#[derive(Debug, Clone, Serialize)]
pub struct ArgosyInfo {
    /// Manifest name.
    pub name: String,
    /// The bundle's own content version.
    pub argosy_version: String,
    /// The OKF spec version the bundle targets, when declared.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub okf_version: Option<String>,
    /// `"local"` (writable) or `"imported"` (read-only).
    pub kind: &'static str,
    /// The user's trust cap for this import (default `unverified`);
    /// `None` for the local book.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trust: Option<String>,
    /// Git commit of the import's checkout at session open, when it resolves.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_commit: Option<String>,
}

/// An imported argosy refused at open for failing integrity
/// verification (see [`crate::context::ExcludedImport`]); remove the
/// checkout and re-pull it.
#[derive(Debug, Clone, Serialize)]
pub struct ExcludedArgosy {
    /// Best-effort identity of the refused checkout (its directory name).
    pub name: String,
    /// The `{:#}` chain of the integrity error.
    pub reason: String,
}

/// The `argosy://_argosys` resource body.
#[derive(Debug, Clone, Serialize)]
pub struct ArgosysReport {
    /// Every active argosy, local first, imports in registration order.
    pub argosys: Vec<ArgosyInfo>,
    /// Imports refused at open; their content is not served.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub excluded: Vec<ExcludedArgosy>,
}

/// One read resource's body, pre-rmcp.
#[derive(Debug, Clone)]
pub struct ResourceBody {
    /// The requested URI.
    pub uri: String,
    /// Raw text (markdown with frontmatter, or the `_argosys` JSON).
    pub text: String,
    /// MIME type.
    pub mime: &'static str,
    /// Qualified-identity metadata: `{argosy, namespace, id}`.
    pub meta: Option<serde_json::Value>,
}

/// One entry of `list_resources`, pre-rmcp.
#[derive(Debug, Clone)]
pub struct ResourceDescriptor {
    /// The resource URI.
    pub uri: String,
    /// Programmatic name.
    pub name: String,
    /// Human/agent-facing description.
    pub description: String,
    /// MIME type.
    pub mime: &'static str,
}

/// The `_meta` provenance blob (`kind`/`trust`) attached to resource reads.
pub(crate) fn identity_meta(
    argosy: &str,
    namespace: &str,
    id: &str,
    kind: &str,
    trust: &str,
) -> serde_json::Value {
    serde_json::json!({
        "argosy": argosy,
        "namespace": namespace,
        "conceptId": id,
        "kind": kind,
        "trust": trust,
    })
}

/// `_meta` blob for the root-index pseudo-resource.
pub(crate) fn meta_with_writable(argosy: &str, writable: bool) -> serde_json::Value {
    serde_json::json!({
        "argosy": argosy,
        "writable": writable,
    })
}
