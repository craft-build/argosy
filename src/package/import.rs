//! Craft YAML styleguide import.

use std::fs;
use std::path::Path;
use std::str::FromStr;

use snafu::ResultExt;
use yaml_serde::{Mapping, Value};

use crate::bundle::{Finding, Severity};
use crate::concept::{Concept, ConceptId};
use crate::error::{IoSnafu, Result};
use crate::local::LocalArgosy;

/// The outcome of an [`import_styleguide_yaml`] run.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ImportReport {
    /// Rule concepts successfully written this run.
    pub written: usize,
    /// Rule ids whose target concept already existed and were therefore left
    /// untouched — imports are additive and re-runnable; silently overwriting
    /// user edits is how you lose trust.
    pub skipped_existing: Vec<String>,
    /// Rules that could not be converted or written. The batch never aborts
    /// on one bad rule; callers treat a non-empty vec as
    /// failure after the fact.
    pub findings: Vec<Finding>,
    /// YAML files the import considered. Zero means the directory held no
    /// `.yaml`/`.yml` files at all — almost always a wrong path spelling,
    /// which must not look like a clean no-op success.
    pub yaml_files_seen: usize,
}

/// Imports Craft YAML rule sets into the local argosy's `styleguide/`
/// namespace: one concept per rule at `styleguide/<language or "general">/
/// <category or "misc">/<RULE-ID>.md` via [`LocalArgosy::write_concept`].
/// A file-level `metadata:` block supplies default `language`/`category`;
/// examples nest or stay flat; `warning`→`warn`; tags carry over.
pub fn import_styleguide_yaml(local: &LocalArgosy, yaml_dir: &Path) -> Result<ImportReport> {
    let mut report = ImportReport::default();
    let mut entries: Vec<fs::DirEntry> = fs::read_dir(yaml_dir)
        .context(IoSnafu {
            path: yaml_dir.to_path_buf(),
        })?
        .collect::<std::io::Result<Vec<_>>>()
        .context(IoSnafu {
            path: yaml_dir.to_path_buf(),
        })?;
    entries.sort_by_key(std::fs::DirEntry::file_name);

    for entry in entries {
        let path = entry.path();
        let is_yaml = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e == "yaml" || e == "yml");
        if !path.is_file() || !is_yaml {
            continue;
        }
        report.yaml_files_seen += 1;
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) => {
                report.findings.push(Finding::new(
                    Severity::Error,
                    None,
                    Some(path.clone()),
                    format!("failed to read file: {e}"),
                ));
                continue;
            }
        };
        let value: Value = match yaml_serde::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                report.findings.push(Finding::new(
                    Severity::Error,
                    None,
                    Some(path.clone()),
                    format!("failed to parse YAML: {e}"),
                ));
                continue;
            }
        };
        import_rule_file(local, &value, &path, &mut report);
    }
    Ok(report)
}

/// Converts every rule object in one decoded YAML file. Accepts a bare
/// sequence or a mapping with a `rules:` sequence (the two observed Craft
/// shapes); anything else is one file-level finding.
fn import_rule_file(local: &LocalArgosy, value: &Value, file: &Path, report: &mut ImportReport) {
    let rules = match value {
        Value::Sequence(items) => Some(items.clone()),
        Value::Mapping(map) => match map.get("rules") {
            Some(Value::Sequence(items)) => Some(items.clone()),
            _ => None,
        },
        _ => None,
    };
    let Some(rules) = rules else {
        report.findings.push(Finding::new(
            Severity::Error,
            None,
            Some(file.to_path_buf()),
            "expected a sequence of rules or a mapping with a top-level `rules:` key",
        ));
        return;
    };
    let defaults = file_defaults(value, file, report);
    for item in rules {
        let Value::Mapping(rule) = item else {
            report.findings.push(Finding::new(
                Severity::Error,
                None,
                Some(file.to_path_buf()),
                "rule entry is not a mapping",
            ));
            continue;
        };
        import_one_rule(local, &rule, file, &defaults, report);
    }
}

/// File-level facet defaults from a rule set's `metadata:` block: the Craft
/// schema puts `language`/`category` on the file, and every rule in it
/// inherits them unless the rule carries its own. A malformed block is a
/// finding, not an abort — the rules still import with whatever facets they
/// carry themselves.
fn file_defaults(value: &Value, file: &Path, report: &mut ImportReport) -> FileDefaults {
    let mut defaults = FileDefaults {
        language: None,
        category: None,
    };
    let Some(metadata) = value.as_mapping().and_then(|m| m.get("metadata")) else {
        return defaults;
    };
    let Value::Mapping(map) = metadata else {
        report.findings.push(Finding::new(
            Severity::Error,
            None,
            Some(file.to_path_buf()),
            "`metadata` must be a mapping",
        ));
        return defaults;
    };
    for (key, slot) in [
        ("language", &mut defaults.language),
        ("category", &mut defaults.category),
    ] {
        match map.get(key) {
            None | Some(Value::Null) => {}
            Some(Value::String(s)) => {
                let s = s.trim();
                if !s.is_empty() {
                    *slot = Some(s.to_string());
                }
            }
            Some(_) => report.findings.push(Finding::new(
                Severity::Error,
                None,
                Some(file.to_path_buf()),
                format!("`metadata.{key}` must be a string"),
            )),
        }
    }
    defaults
}

/// The per-file facet defaults [`file_defaults`] extracts.
struct FileDefaults {
    language: Option<String>,
    category: Option<String>,
}

/// Converts one decoded rule object: `Err(String)` carries the human-readable
/// reason the rule cannot become a finding-free concept.
fn rule_to_concept(
    rule: &Mapping,
    defaults: &FileDefaults,
) -> std::result::Result<(String, Concept), String> {
    let get_str = |key: &str| rule.get(key).and_then(Value::as_str);
    let id = get_str("id")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "rule has no `id`".to_string())?;
    // A facet the rule carries itself beats the file-level default it
    // inherits; an empty/whitespace value counts as absent either way.
    let facet = |key: &str, default: &Option<String>| {
        get_str(key)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .or(default.as_deref())
            .map(str::to_string)
    };
    // Examples preserve the YAML form: sequences render as `- ` bullets,
    // bare strings verbatim; non-string items are reported, never silently
    // dropped. Nested (`examples.good`/`examples.bad`) and flat
    // (`good:`/`bad:`) shapes are both read; nested wins when both appear.
    let get_examples = |key: &str| -> std::result::Result<(Vec<String>, bool), String> {
        if let Some(nested) = rule.get("examples")
            && !matches!(nested, Value::Null)
        {
            let Value::Mapping(map) = nested else {
                return Err(format!(
                    "rule `{id}`: `examples` must be a mapping with `good`/`bad` keys"
                ));
            };
            return match map.get(key) {
                None | Some(Value::Null) => Ok((Vec::new(), false)),
                Some(value) => example_items(value, id, &format!("examples.{key}")),
            };
        }
        match rule.get(key) {
            None | Some(Value::Null) => Ok((Vec::new(), false)),
            Some(value) => example_items(value, id, key),
        }
    };
    let description = get_str("description")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("rule `{id}`: missing required `description`"))?;
    // Non-string scalars in the string fields would otherwise be silently
    // dropped (`priority: 1` imports without its priority); report them the
    // same way an invalid priority value is.
    for key in ["language", "category", "priority"] {
        if let Some(value) = rule.get(key)
            && !matches!(value, Value::String(_) | Value::Null)
        {
            return Err(format!("rule `{id}`: `{key}` must be a string"));
        }
    }
    let priority = match get_str("priority") {
        Some("error" | "warn" | "info" | "hint") => get_str("priority"),
        // The schema's `warning` is the conventional `warn`.
        Some("warning") => Some("warn"),
        Some(p) => {
            return Err(format!(
                "rule `{id}`: `priority` must be one of error/warn/warning/info/hint, got `{p}`"
            ));
        }
        None => None,
    };
    let pattern = match rule.get("pattern") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Sequence(items)) => {
            let mut parts = Vec::with_capacity(items.len());
            for item in items {
                match item.as_str() {
                    Some(s) => parts.push(s),
                    None => {
                        return Err(format!("rule `{id}`: `pattern` list items must be strings"));
                    }
                }
            }
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n"))
            }
        }
        Some(_) => {
            return Err(format!(
                "rule `{id}`: `pattern` must be a string or list of strings"
            ));
        }
    };
    let tags = match rule.get("tags") {
        None | Some(Value::Null) => Vec::new(),
        Some(value) => match value {
            Value::String(s) => vec![s.clone()],
            Value::Sequence(items) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    match item.as_str() {
                        Some(s) => out.push(s.to_string()),
                        None => {
                            return Err(format!("rule `{id}`: `tags` list items must be strings"));
                        }
                    }
                }
                out
            }
            _ => {
                return Err(format!(
                    "rule `{id}`: `tags` must be a string or list of strings"
                ));
            }
        },
    };

    let mut frontmatter = Mapping::new();
    let mut put = |key: &str, value: &str| {
        frontmatter.insert(
            Value::String(key.to_string()),
            Value::String(value.to_string()),
        );
    };
    put("type", crate::styleguide::TYPE);
    put("description", description);
    if let Some(language) = facet("language", &defaults.language) {
        put("language", &language);
    }
    if let Some(category) = facet("category", &defaults.category) {
        put("category", &category);
    }
    put("rule_id", id);
    if let Some(priority) = priority {
        put("priority", priority);
    }
    if let Some(pattern) = &pattern {
        put("pattern", pattern);
    }
    if !tags.is_empty() {
        frontmatter.insert(
            Value::String("tags".to_string()),
            Value::Sequence(tags.into_iter().map(Value::String).collect()),
        );
    }

    let good = get_examples("good")?;
    let bad = get_examples("bad")?;
    let mut body = description.to_string();
    for (heading, (examples, bulleted)) in [("## Good", &good), ("## Bad", &bad)] {
        if examples.is_empty() {
            continue;
        }
        body.push_str("\n\n");
        body.push_str(heading);
        body.push_str("\n\n");
        for (i, example) in examples.iter().enumerate() {
            if *bulleted {
                body.push_str("- ");
            }
            body.push_str(example);
            if i + 1 < examples.len() {
                body.push('\n');
            }
        }
    }
    body.push('\n');

    let concept = Concept::new(frontmatter, body)
        .map_err(|e| format!("rule `{id}`: cannot build concept: {e}"))?;
    Ok((id.to_string(), concept))
}

/// One `good:`/`bad:` example value in either shape: a sequence of strings
/// (rendered as bullets) or one bare string (rendered verbatim). Anything
/// else is an error, not a silent drop.
fn example_items(
    value: &Value,
    id: &str,
    key: &str,
) -> std::result::Result<(Vec<String>, bool), String> {
    match value {
        Value::Sequence(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item.as_str() {
                    Some(s) => out.push(s.to_string()),
                    None => {
                        return Err(format!("rule `{id}`: `{key}` list items must be strings"));
                    }
                }
            }
            Ok((out, true))
        }
        Value::String(s) => Ok((vec![s.clone()], false)),
        _ => Err(format!(
            "rule `{id}`: `{key}` must be a string or list of strings"
        )),
    }
}

/// Writes one converted rule under `styleguide/` (or records why not).
fn import_one_rule(
    local: &LocalArgosy,
    rule: &Mapping,
    file: &Path,
    defaults: &FileDefaults,
    report: &mut ImportReport,
) {
    let (rule_id, concept) = match rule_to_concept(rule, defaults) {
        Ok(ok) => ok,
        Err(reason) => {
            report.findings.push(Finding::new(
                Severity::Error,
                None,
                Some(file.to_path_buf()),
                reason,
            ));
            return;
        }
    };
    let language = concept.get_str("language").unwrap_or("general");
    let category = concept.get_str("category").unwrap_or("misc");
    let target = format!("styleguide/{language}/{category}/{rule_id}");
    let id = match ConceptId::from_str(&target) {
        Ok(id) => id,
        Err(e) => {
            report.findings.push(Finding::new(
                Severity::Error,
                None,
                Some(file.to_path_buf()),
                format!("rule `{rule_id}`: not a valid concept path `{target}`: {e}"),
            ));
            return;
        }
    };
    let rel = id.to_relative_path();
    if local.root().join(&rel).exists() {
        report.skipped_existing.push(rule_id);
        return;
    }
    match local.write_rule(&id, &concept) {
        Ok(_) => report.written += 1,
        Err(e) => report.findings.push(Finding::new(
            Severity::Error,
            None,
            Some(rel),
            format!("rule `{rule_id}`: {e}"),
        )),
    }
}
