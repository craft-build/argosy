//! Tool outcomes (serialized as structured tool results) and resource
//! shapes.

use serde::Serialize;

use crate::concept::Concept;

use super::UNVERIFIED;

/// One search hit, with the qualified `argosy://` URI and the unit's facets
/// so clients can re-filter without resolving.
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
    /// Similarity score (higher is better).
    pub score: f32,
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

/// One skill in `list_skills`, with origin argosy, shadowing status, and
/// the OKF trust tier.
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
    /// OKF trust tier: the entry point's `verified` value, or
    /// `"unverified"` when absent.
    pub verified: String,
}

/// The `list_skills` tool outcome.
#[derive(Debug, Clone, Serialize)]
pub struct SkillsReport {
    /// All visible skills across every active argosy.
    pub skills: Vec<SkillOut>,
}

/// The `get_skill` tool outcome: the resolved skill plus its entry-point
/// content.
#[derive(Debug, Clone, Serialize)]
pub struct SkillContent {
    /// The precedence-resolved skill, with trust fields.
    pub skill: SkillOut,
    /// Raw markdown with frontmatter of the entry-point concept.
    pub content: String,
}

/// A concept read: the `read_memory` outcome.
#[derive(Debug, Clone, Serialize)]
pub struct UriContent {
    /// The concept's `argosy://` URI.
    pub uri: String,
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
    /// Raw markdown with frontmatter.
    pub content: String,
}

/// A mutating tool's machine-readable summary: what changed and where.
#[derive(Debug, Clone, Serialize)]
pub struct WriteReport {
    /// `"created"`, `"updated"`, or `"deleted"`.
    pub action: &'static str,
    /// The affected `argosy://` URI.
    #[serde(rename = "uri")]
    pub uri: String,
    /// Size of the concept written to disk; omitted for deletions and when
    /// the size could not be read back.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// True iff the index was reconciled: the change is already visible to
    /// `search`. False means on disk but not indexed — see `index_error`.
    pub indexed: bool,
    /// Why reconciliation failed; present only when `indexed` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_error: Option<String>,
}

/// The `promote` tool outcome: the untouched source plus the drafted target
/// concept for the client's confirmation — the server never confirms.
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
}

/// The `argosy://_argosys` resource body.
#[derive(Debug, Clone, Serialize)]
pub struct ArgosysReport {
    /// Every active argosy, local first, imports in registration order.
    pub argosys: Vec<ArgosyInfo>,
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

/// The verified tier of a concept: its `verified` frontmatter value, or
/// `"unverified"` when absent. A non-string value is treated as absent —
/// tool output is for LLM consumers, and a surprise structure is not a
/// trust signal to relay.
pub(super) fn verified_tier(concept: &Concept) -> String {
    concept
        .get_str("verified")
        .map(str::to_string)
        .unwrap_or_else(|| UNVERIFIED.to_string())
}

/// `_meta` identity blob attached to resource reads.
pub(crate) fn identity_meta(argosy: &str, namespace: &str, id: &str) -> serde_json::Value {
    serde_json::json!({
        "argosy": argosy,
        "namespace": namespace,
        "conceptId": id,
    })
}

/// `_meta` blob for the root-index pseudo-resource.
pub(crate) fn meta_with_writable(argosy: &str, writable: bool) -> serde_json::Value {
    serde_json::json!({
        "argosy": argosy,
        "writable": writable,
    })
}
