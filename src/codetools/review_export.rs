//! PR-ready renderings of review findings: SARIF 2.1.0 and reviewdog rdjsonl.
//!
//! Edits become SARIF `fixes[]` and reviewdog `suggestions[]`; both renderers
//! are pure over the recorded findings, so exports cannot drift from them.

use std::collections::BTreeMap;

use serde::Serialize;

use super::review::{ReviewFinding, ReviewPriority};

/// The SARIF 2.1.0 schema locator, so consumers validate the document.
const SARIF_SCHEMA: &str = "https://json.schemastore.org/sarif-2.1.0.json";
/// Tool name in the SARIF driver and the rdjson source.
const TOOL_NAME: &str = "argosy";
const TOOL_URI: &str = "https://github.com/craft-build/argosy";
/// `ruleId` for findings with no rule URIs, so results stay inside
/// `driver.rules`.
const UNGROUNDED_RULE_ID: &str = "argosy/ungrounded";
/// `partialFingerprints` key holding the finding id for cross-run dedup.
const FINGERPRINT_KEY: &str = "argosyFindingId/v1";

/// Renders the findings of one review as a SARIF 2.1.0 log.
///
/// `review_id` becomes the run's `automationDetails.id`.
pub(super) fn to_sarif(review_id: &str, findings: &[ReviewFinding]) -> String {
    let log = SarifLog {
        schema: SARIF_SCHEMA,
        version: "2.1.0",
        runs: vec![SarifRun {
            tool: SarifTool {
                driver: SarifDriver {
                    name: TOOL_NAME,
                    version: env!("CARGO_PKG_VERSION"),
                    information_uri: TOOL_URI,
                    rules: rule_ids(findings)
                        .into_iter()
                        .map(|id| SarifRule { id })
                        .collect(),
                },
            },
            automation_details: Some(SarifAutomationDetails {
                id: format!("argosy/reviews/{review_id}"),
                description: Some(SarifMessage {
                    text: format!("argosy review {review_id}: {} findings", findings.len()),
                }),
            }),
            results: findings.iter().map(sarif_result).collect(),
        }],
    };
    serde_json::to_string_pretty(&log).expect("SARIF serialization is infallible")
}

/// Renders the findings as reviewdog rdjsonl: one Diagnostic JSON object per
/// line.
pub(super) fn to_rdjsonl(findings: &[ReviewFinding]) -> String {
    let mut out = String::new();
    for finding in findings {
        let diagnostic = RdDiagnostic {
            source: RdSource {
                name: TOOL_NAME,
                url: TOOL_URI,
            },
            severity: rdjson_severity(finding.priority),
            code: finding
                .rule_uris
                .first()
                .map(|uri| RdCode { value: uri.clone() }),
            message: message_text(finding),
            range: rdjson_range(finding.line_start, finding.line_end),
            suggestions: finding
                .edits
                .iter()
                .map(|edit| RdSuggestion {
                    range: rdjson_range(edit.line_start, edit.line_end),
                    text: edit.replacement.clone(),
                })
                .collect(),
        };
        out.push_str(
            &serde_json::to_string(&diagnostic).expect("rdjson serialization is infallible"),
        );
        out.push('\n');
    }
    out
}

/// The SARIF `level` for a priority: P0/P1 → `error`, P2 → `warning`,
/// P3 → `note`.
fn sarif_level(priority: ReviewPriority) -> &'static str {
    match priority {
        ReviewPriority::P0 | ReviewPriority::P1 => "error",
        ReviewPriority::P2 => "warning",
        ReviewPriority::P3 => "note",
    }
}

/// The reviewdog severity for a priority: P0/P1 → `ERROR`, P2 → `WARNING`,
/// P3 → `INFO`.
fn rdjson_severity(priority: ReviewPriority) -> &'static str {
    match priority {
        ReviewPriority::P0 | ReviewPriority::P1 => "ERROR",
        ReviewPriority::P2 => "WARNING",
        ReviewPriority::P3 => "INFO",
    }
}

/// Title, body, free-text suggestion and any rule URIs beyond the first, as
/// one plain-text message both formats agree on.
fn message_text(finding: &ReviewFinding) -> String {
    let mut text = format!("{}\n\n{}", finding.title, finding.body);
    if finding.rule_uris.len() > 1 {
        text.push_str("\n\nRules: ");
        text.push_str(&finding.rule_uris.join(", "));
    }
    if let Some(suggestion) = &finding.suggestion {
        text.push_str("\n\nSuggestion: ");
        text.push_str(suggestion);
    }
    text
}

/// Distinct rule ids in finding order, so every `result.ruleId` has a
/// `driver.rules` entry.
fn rule_ids(findings: &[ReviewFinding]) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for finding in findings {
        let id = finding
            .rule_uris
            .first()
            .cloned()
            .unwrap_or_else(|| UNGROUNDED_RULE_ID.to_string());
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

/// One finding as a SARIF result.
fn sarif_result(finding: &ReviewFinding) -> SarifResult {
    let region = || SarifRegion {
        start_line: finding.line_start,
        end_line: finding.line_end,
    };
    let mut fingerprints = BTreeMap::new();
    fingerprints.insert(FINGERPRINT_KEY.to_string(), finding.finding_id.clone());
    SarifResult {
        rule_id: finding
            .rule_uris
            .first()
            .cloned()
            .unwrap_or_else(|| UNGROUNDED_RULE_ID.to_string()),
        level: sarif_level(finding.priority),
        message: SarifMessage {
            text: message_text(finding),
        },
        locations: vec![SarifLocation {
            physical_location: SarifPhysicalLocation {
                artifact_location: SarifArtifactLocation {
                    uri: finding.path.clone(),
                },
                region: region(),
            },
        }],
        partial_fingerprints: fingerprints,
        fixes: if finding.edits.is_empty() {
            Vec::new()
        } else {
            vec![SarifFix {
                description: SarifMessage {
                    text: finding.title.clone(),
                },
                artifact_changes: vec![SarifArtifactChange {
                    artifact_location: SarifArtifactLocation {
                        uri: finding.path.clone(),
                    },
                    replacements: finding
                        .edits
                        .iter()
                        .map(|edit| SarifReplacement {
                            deleted_region: SarifRegion {
                                start_line: edit.line_start,
                                end_line: edit.line_end,
                            },
                            inserted_content: SarifInsertedContent {
                                text: edit.replacement.clone(),
                            },
                        })
                        .collect(),
                }],
            }]
        },
    }
}

fn rdjson_range(start: u32, end: u32) -> RdRange {
    RdRange {
        start: RdPosition { line: start },
        end: RdPosition { line: end },
    }
}

// The SARIF 2.1.0 subset argosy emits.

#[derive(Serialize)]
struct SarifLog {
    #[serde(rename = "$schema")]
    schema: &'static str,
    version: &'static str,
    runs: Vec<SarifRun>,
}

#[derive(Serialize)]
struct SarifRun {
    tool: SarifTool,
    #[serde(rename = "automationDetails", skip_serializing_if = "Option::is_none")]
    automation_details: Option<SarifAutomationDetails>,
    results: Vec<SarifResult>,
}

#[derive(Serialize)]
struct SarifTool {
    driver: SarifDriver,
}

#[derive(Serialize)]
struct SarifDriver {
    name: &'static str,
    version: &'static str,
    #[serde(rename = "informationUri")]
    information_uri: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    rules: Vec<SarifRule>,
}

#[derive(Serialize)]
struct SarifRule {
    id: String,
}

#[derive(Serialize)]
struct SarifAutomationDetails {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<SarifMessage>,
}

#[derive(Serialize)]
struct SarifResult {
    #[serde(rename = "ruleId")]
    rule_id: String,
    level: &'static str,
    message: SarifMessage,
    locations: Vec<SarifLocation>,
    #[serde(rename = "partialFingerprints")]
    partial_fingerprints: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    fixes: Vec<SarifFix>,
}

#[derive(Serialize)]
struct SarifMessage {
    text: String,
}

#[derive(Serialize)]
struct SarifLocation {
    #[serde(rename = "physicalLocation")]
    physical_location: SarifPhysicalLocation,
}

#[derive(Serialize)]
struct SarifPhysicalLocation {
    #[serde(rename = "artifactLocation")]
    artifact_location: SarifArtifactLocation,
    region: SarifRegion,
}

#[derive(Serialize)]
struct SarifArtifactLocation {
    uri: String,
}

#[derive(Serialize)]
struct SarifRegion {
    #[serde(rename = "startLine")]
    start_line: u32,
    #[serde(rename = "endLine")]
    end_line: u32,
}

#[derive(Serialize)]
struct SarifFix {
    description: SarifMessage,
    #[serde(rename = "artifactChanges")]
    artifact_changes: Vec<SarifArtifactChange>,
}

#[derive(Serialize)]
struct SarifArtifactChange {
    #[serde(rename = "artifactLocation")]
    artifact_location: SarifArtifactLocation,
    replacements: Vec<SarifReplacement>,
}

#[derive(Serialize)]
struct SarifReplacement {
    #[serde(rename = "deletedRegion")]
    deleted_region: SarifRegion,
    #[serde(rename = "insertedContent")]
    inserted_content: SarifInsertedContent,
}

#[derive(Serialize)]
struct SarifInsertedContent {
    text: String,
}

// The rdjson Diagnostic subset argosy emits per JSONL line.

#[derive(Serialize)]
struct RdDiagnostic {
    source: RdSource,
    severity: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<RdCode>,
    message: String,
    range: RdRange,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    suggestions: Vec<RdSuggestion>,
}

#[derive(Serialize)]
struct RdSource {
    name: &'static str,
    url: &'static str,
}

#[derive(Serialize)]
struct RdCode {
    value: String,
}

#[derive(Serialize)]
struct RdRange {
    start: RdPosition,
    end: RdPosition,
}

#[derive(Serialize)]
struct RdPosition {
    /// 1-based line; the column is unknown for line-level findings.
    line: u32,
}

#[derive(Serialize)]
struct RdSuggestion {
    range: RdRange,
    text: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codetools::review::{FindingEdit, FindingOrigin};

    /// A finding with every field the exporters read.
    fn finding(priority: ReviewPriority, rule_uris: &[&str]) -> ReviewFinding {
        let mut value = ReviewFinding {
            finding_id: "abc123def456".to_string(),
            title: format!("[{priority:?}] Keep the original greeting"),
            body: "Returning `after` changes the observable contract.".to_string(),
            priority,
            confidence: 0.9,
            path: "hello.txt".to_string(),
            line_start: 2,
            line_end: 3,
            rule_uris: rule_uris.iter().map(|uri| uri.to_string()).collect(),
            suggestion: Some("Restore `before`.".to_string()),
            assessment: None,
            origin: FindingOrigin::Agent,
            edits: Vec::new(),
        };
        if rule_uris.is_empty() {
            value.title = format!("[{priority:?}] Ungrounded judgment finding");
        }
        value
    }

    fn edited_finding() -> ReviewFinding {
        let mut value = finding(ReviewPriority::P1, &["argosy://book/styleguide/contracts"]);
        value.edits = vec![
            FindingEdit {
                line_start: 2,
                line_end: 2,
                replacement: "before\n".to_string(),
            },
            FindingEdit {
                line_start: 3,
                line_end: 3,
                replacement: "also-before\n".to_string(),
            },
        ];
        value
    }

    #[test]
    fn sarif_is_a_complete_2_1_0_log() {
        let text = to_sarif("r7", &[edited_finding()]);
        let log: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(log["$schema"], SARIF_SCHEMA);
        assert_eq!(log["version"], "2.1.0");
        assert_eq!(log["runs"][0]["tool"]["driver"]["name"], "argosy");
        assert_eq!(
            log["runs"][0]["tool"]["driver"]["version"],
            env!("CARGO_PKG_VERSION")
        );
        assert_eq!(
            log["runs"][0]["automationDetails"]["id"],
            "argosy/reviews/r7"
        );
        let result = &log["runs"][0]["results"][0];
        assert_eq!(result["ruleId"], "argosy://book/styleguide/contracts");
        assert_eq!(result["level"], "error", "P1 maps to error");
        assert_eq!(
            result["locations"][0]["physicalLocation"]["artifactLocation"]["uri"],
            "hello.txt"
        );
        let region = &result["locations"][0]["physicalLocation"]["region"];
        assert_eq!(region["startLine"], 2);
        assert_eq!(region["endLine"], 3);
        assert_eq!(
            result["partialFingerprints"][FINGERPRINT_KEY],
            "abc123def456"
        );
        let replacements = &result["fixes"][0]["artifactChanges"][0]["replacements"];
        let replacements = replacements.as_array().expect("replacements is an array");
        assert_eq!(replacements.len(), 2, "both edits exported");
        assert_eq!(replacements[0]["deletedRegion"]["startLine"], 2);
        assert_eq!(replacements[0]["insertedContent"]["text"], "before\n");
        // The rule id appears in driver.rules.
        assert_eq!(
            log["runs"][0]["tool"]["driver"]["rules"][0]["id"],
            "argosy://book/styleguide/contracts"
        );
    }

    #[test]
    fn sarif_severity_mapping_covers_every_priority() {
        for (priority, level) in [
            (ReviewPriority::P0, "error"),
            (ReviewPriority::P1, "error"),
            (ReviewPriority::P2, "warning"),
            (ReviewPriority::P3, "note"),
        ] {
            let text = to_sarif("r", &[finding(priority, &["argosy://b/s/r"])]);
            let log: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(log["runs"][0]["results"][0]["level"], level, "{priority:?}");
        }
    }

    #[test]
    fn ungrounded_findings_use_a_synthetic_rule_id() {
        let text = to_sarif("r", &[finding(ReviewPriority::P2, &[])]);
        let log: serde_json::Value = serde_json::from_str(&text).unwrap();
        let result = &log["runs"][0]["results"][0];
        assert_eq!(result["ruleId"], "argosy/ungrounded");
        assert_eq!(
            log["runs"][0]["tool"]["driver"]["rules"][0]["id"],
            "argosy/ungrounded"
        );
    }

    #[test]
    fn findings_without_edits_have_no_fixes() {
        let text = to_sarif("r", &[finding(ReviewPriority::P2, &["argosy://b/s/r"])]);
        let log: serde_json::Value = serde_json::from_str(&text).unwrap();
        let result = &log["runs"][0]["results"][0];
        assert!(result.get("fixes").is_none(), "fixes omitted without edits");
    }

    #[test]
    fn multiple_rule_uris_name_every_rule_in_the_message() {
        let text = to_sarif(
            "r",
            &[finding(
                ReviewPriority::P2,
                &["argosy://b/s/first", "argosy://b/s/second"],
            )],
        );
        let log: serde_json::Value = serde_json::from_str(&text).unwrap();
        let result = &log["runs"][0]["results"][0];
        assert_eq!(result["ruleId"], "argosy://b/s/first");
        assert!(
            result["message"]["text"]
                .as_str()
                .unwrap()
                .contains("Rules: argosy://b/s/first, argosy://b/s/second"),
            "the message keeps the rules the ruleId cannot"
        );
    }

    #[test]
    fn rdjsonl_is_one_json_diagnostic_per_line() {
        let text = to_rdjsonl(&[
            edited_finding(),
            finding(ReviewPriority::P3, &["argosy://b/s/minor"]),
        ]);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(text.ends_with('\n'), "JSONL ends with a newline");

        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["source"]["name"], "argosy");
        assert_eq!(first["severity"], "ERROR", "P1 maps to ERROR");
        assert_eq!(first["code"]["value"], "argosy://book/styleguide/contracts");
        assert_eq!(first["range"]["start"]["line"], 2);
        assert_eq!(first["range"]["end"]["line"], 3);
        assert_eq!(first["suggestions"][0]["range"]["start"]["line"], 2);
        assert_eq!(first["suggestions"][0]["text"], "before\n");
        assert_eq!(first["suggestions"][1]["text"], "also-before\n");

        let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second["severity"], "INFO", "P3 maps to INFO");
        assert!(
            second.get("suggestions").is_none(),
            "no suggestions key without edits"
        );
    }
}
