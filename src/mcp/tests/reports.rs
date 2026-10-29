//! Every field the generated output schema marks `required` must appear in
//! the serialized output, even when empty — `skip_serializing_if` on a
//! non-`Option` field breaks that (schemars marks such fields `required`).

use serde::Serialize;
use serde_json::Value;

use super::*;
use crate::codetools::review::{ReviewFinding, ReviewFindingsOutcome, ReviewPriority};

/// Recursively assert required/declared properties agree, resolving
/// schemars' local `#/$defs` refs.
fn check(root: &Value, schema: &Value, instance: &Value, at: &str, errors: &mut Vec<String>) {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let name = reference
            .strip_prefix("#/$defs/")
            .expect("schemars emits local $defs refs");
        return check(root, &root["$defs"][name], instance, at, errors);
    }
    let Some(object) = instance.as_object() else {
        return;
    };
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for key in required.iter().filter_map(Value::as_str) {
            if !object.contains_key(key) {
                errors.push(format!(
                    "{at}: schema requires `{key}` but serialization omits it"
                ));
            }
        }
    }
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for (key, value) in object {
            let Some(sub) = properties.get(key) else {
                errors.push(format!("{at}: serialization emits undeclared `{key}`"));
                continue;
            };
            match (value.as_array(), sub.get("items")) {
                (Some(array), Some(items)) => {
                    for (i, item) in array.iter().enumerate() {
                        check(root, items, item, &format!("{at}.{key}[{i}]"), errors);
                    }
                }
                _ => check(root, sub, value, &format!("{at}.{key}"), errors),
            }
        }
    }
}

/// Assert `instance`'s serialized form satisfies the schema generated for `T`.
fn assert_matches_output_schema<T: Serialize + rmcp::schemars::JsonSchema>(at: &str, instance: &T) {
    let schema = serde_json::to_value(rmcp::schemars::schema_for!(T)).expect("schema serializes");
    let instance = serde_json::to_value(instance).expect("instance serializes");
    let mut errors = Vec::new();
    check(&schema, &schema, &instance, at, &mut errors);
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

#[test]
fn search_hit_with_empty_facets_matches_output_schema() {
    // The regression case: no heading, no tags, every facet `None`.
    let hit = SearchHitOut {
        uri: "argosy://acme/styleguide/testing/MOCK-RULE".into(),
        argosy: "acme".into(),
        namespace: "styleguide".into(),
        concept_id: "testing/MOCK-RULE".into(),
        score: 0.0,
        chunk_ordinal: 0,
        heading_path: String::new(),
        start_line: 1,
        end_line: 1,
        passage: String::new(),
        concept_type: None,
        description: None,
        tags: Vec::new(),
        language: None,
        category: None,
        priority: None,
        kind: "local",
        trust: "unverified".into(),
        source_commit: None,
        applicability: None,
        good: None,
        bad: None,
    };
    assert_matches_output_schema("SearchHitOut", &hit);
    assert_matches_output_schema("SearchReport", &SearchReport { hits: vec![hit] });
}

#[test]
fn review_finding_with_empty_extras_matches_output_schema() {
    let finding = ReviewFinding {
        finding_id: "f-1".into(),
        title: "[P2] Mock finding".into(),
        body: String::new(),
        priority: ReviewPriority::P2,
        confidence: 1.0,
        path: "src/mock.rs".into(),
        line_start: 1,
        line_end: 2,
        rule_uris: Vec::new(),
        suggestion: None,
        edits: Vec::new(),
        assessment: None,
        origin: Default::default(),
    };
    assert_matches_output_schema("ReviewFinding", &finding);
    assert_matches_output_schema(
        "ReviewFindingsOutcome",
        &ReviewFindingsOutcome {
            review_id: "r-1".into(),
            findings: vec![finding],
            export: None,
        },
    );
}
