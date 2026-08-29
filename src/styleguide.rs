//! The typed model and namespace contract for `styleguide/` rules: ordinary
//! concepts retrieved by embedding similarity and filtered by
//! `language`/`category`. [`StyleguideRule::list`] is tolerant — only
//! hard-contract concepts become rules — while [`validate`] surfaces
//! violations, warning on missing facets.

use std::ffi::OsStr;
use std::path::Path;

use crate::bundle::{Argosy, Finding, Namespace, Severity, sorted_walk};
use crate::concept::{Concept, ConceptId};
use crate::error::{Error, Result};

/// The `type` every styleguide rule concept must carry.
pub(crate) const TYPE: &str = "Styleguide Rule";

/// A styleguide rule: an owning typed view over one `styleguide/` concept.
/// Owning the [`ConceptId`] and [`Concept`] keeps rules free of lifetimes so
/// [`StyleguideRule::filter`] can consume and return them and query layers
/// can hold them in arbitrary collections; accessors borrow per call.
#[derive(Debug, Clone)]
pub struct StyleguideRule {
    id: ConceptId,
    concept: Concept,
}

impl StyleguideRule {
    /// Lists every concept under `styleguide/` satisfying the hard contract,
    /// sorted by concept id; subdirectories are followed recursively.
    /// Tolerant like [`crate::skill::Skill::list`]: failing or unparseable
    /// concepts are skipped (validation reports them). Absent namespace
    /// yields an empty vec; unreadable directories are hard errors.
    pub fn list(argosy: &Argosy) -> Result<Vec<StyleguideRule>> {
        let Some(dir) = argosy.namespace_dir(&Namespace::Styleguide) else {
            return Ok(Vec::new());
        };
        let walk = sorted_walk(argosy.root(), Path::new("styleguide"));
        if let Some((path, source)) = walk.unreadable.into_iter().next() {
            let path = if path.as_os_str().is_empty() {
                dir
            } else {
                argosy.root().join(path)
            };
            return Err(Error::Io { path, source });
        }
        let mut rules = walk
            .entries
            .iter()
            .filter(|e| {
                !e.is_dir
                    && e.rel.extension() == Some(OsStr::new("md"))
                    && !Namespace::is_listing_file(
                        e.rel.file_name().and_then(|n| n.to_str()).unwrap_or(""),
                    )
            })
            .filter_map(|e| {
                let concept = Concept::from_file(argosy.root().join(&e.rel)).ok()?;
                if !Self::satisfies_hard_contract(&concept) {
                    return None;
                }
                Some(StyleguideRule {
                    id: ConceptId::try_from(e.rel.as_path()).ok()?,
                    concept,
                })
            })
            .collect::<Vec<_>>();
        rules.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(rules)
    }

    /// Narrows `rules` by exact match on the language/category facets.
    /// `None` means
    /// "no constraint"; a rule lacking the requested facet never matches a
    /// `Some(_)` filter.
    pub fn filter(
        rules: Vec<StyleguideRule>,
        language: Option<&str>,
        category: Option<&str>,
    ) -> Vec<StyleguideRule> {
        rules
            .into_iter()
            .filter(|rule| language.is_none_or(|l| rule.language() == Some(l)))
            .filter(|rule| category.is_none_or(|c| rule.category() == Some(c)))
            .collect()
    }

    fn satisfies_hard_contract(concept: &Concept) -> bool {
        concept.concept_type().is_some_and(|t| t.trim() == TYPE)
            && concept.description().is_some_and(|d| !d.trim().is_empty())
    }

    /// The rule's concept id (e.g. `styleguide/rust/naming/snake-case-vars`).
    pub fn id(&self) -> &ConceptId {
        &self.id
    }

    /// The underlying concept.
    pub fn concept(&self) -> &Concept {
        &self.concept
    }

    /// The language this rule applies to (`rust`, `python`,
    /// `general`,...).
    pub fn language(&self) -> Option<&str> {
        self.concept.get_str("language")
    }

    /// The rule category (`naming`, `error-handling`,...).
    pub fn category(&self) -> Option<&str> {
        self.concept.get_str("category")
    }

    /// A producer's stable rule identifier, if set.
    pub fn rule_id(&self) -> Option<&str> {
        self.concept.get_str("rule_id")
    }

    /// The producer's priority (`error`/`warn`/`info` by convention;
    /// the value is not validated).
    pub fn priority(&self) -> Option<&str> {
        self.concept.get_str("priority")
    }

    /// A machine-checkable pattern, if the producer supplied one.
    pub fn pattern(&self) -> Option<&str> {
        self.concept.get_str("pattern")
    }

    /// The body section under a `## Good` heading, trimmed. `None`
    /// when the heading is absent.
    pub fn good_examples(&self) -> Option<&str> {
        self.section("Good")
    }

    /// The body section under a `## Bad` heading, trimmed. `None`
    /// when the heading is absent.
    pub fn bad_examples(&self) -> Option<&str> {
        self.section("Bad")
    }

    /// Extracts the text between a `## <name>` heading and the next heading
    /// of any level (or end of body), trimmed. Headings inside fenced code
    /// blocks don't count (`#`-starting comment lines are idiomatic in
    /// example code); a present-but-empty section yields `None`.
    fn section(&self, name: &str) -> Option<&str> {
        let body = self.concept.body();
        let mut started: Option<usize> = None;
        let mut in_fence = false;
        let mut byte = 0;
        for line in body.split_inclusive('\n') {
            // CommonMark tolerates up to three leading spaces on headings and
            // fences; deeper indentation is a code block and invisible here.
            let indent = line.bytes().take_while(|b| *b == b' ').count();
            if indent <= 3 {
                let content = line[indent..].trim_end();
                if content.starts_with("```") || content.starts_with("~~~") {
                    in_fence = !in_fence;
                }
                if !in_fence {
                    match started {
                        None if is_target_heading(content, name) => {
                            started = Some(byte + line.len())
                        }
                        Some(start) if is_any_heading(content) => {
                            let text = body[start..byte].trim();
                            return (!text.is_empty()).then_some(text);
                        }
                        _ => {}
                    }
                }
            }
            byte += line.len();
        }
        started.and_then(|start| {
            let text = body[start..].trim();
            (!text.is_empty()).then_some(text)
        })
    }
}

/// True iff `line` is exactly the level-2 ATX heading `## <name>`, tolerating
/// a CommonMark closing sequence (`## Good ##`); `## Gooder` does not match
/// `Good`.
fn is_target_heading(line: &str, name: &str) -> bool {
    let Some(rest) = line.strip_prefix("## ") else {
        return false;
    };
    let Some(rest) = rest.trim().strip_prefix(name) else {
        return false;
    };
    // After the name: nothing, or a CommonMark closing sequence (spaces,
    // one or more `#`s, optional trailing spaces).
    let rest = rest.trim();
    rest.is_empty()
        || rest
            .strip_prefix('#')
            .is_some_and(|r| r.trim_matches(['#', ' ']).is_empty())
}

/// True iff `line` is an ATX heading of any level: leading `#`s followed by a
/// space or end of line (so `#hashtag` and `#5 items` are prose, not headings).
fn is_any_heading(line: &str) -> bool {
    let hashes = line.bytes().take_while(|b| *b == b'#').count();
    hashes > 0 && (line.len() == hashes || line.as_bytes()[hashes] == b' ')
}

/// Validates the `styleguide/` namespace contract of the bundle at `root`.
/// Absent `styleguide/` is fine. OKF conformance is enforced generically by
/// [`Argosy::validate`] and is not duplicated here: unparseable or untyped
/// concepts are skipped, so the type check fires only on a present-but-wrong
/// `type`. Unreadable directories are the structural layer's findings.
pub(crate) fn validate(root: &Path) -> Vec<Finding> {
    let ns_dir = root.join("styleguide");
    if !ns_dir.is_dir() {
        return Vec::new();
    }
    let walk = sorted_walk(root, Path::new("styleguide"));
    let mut findings = Vec::new();
    for entry in &walk.entries {
        if entry.is_dir || entry.rel.extension() != Some(OsStr::new("md")) {
            continue;
        }
        let name = entry.rel.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if Namespace::is_listing_file(name) {
            continue;
        }
        let Ok(concept) = Concept::from_file(root.join(&entry.rel)) else {
            continue;
        };
        // An untyped concept — including a present-but-empty `type` — is
        // wholly the generic pass's finding; running rule-contract
        // checks on it would only double-report one root cause.
        let Some(ty) = concept.concept_type() else {
            continue;
        };
        if ty.trim().is_empty() {
            continue;
        }
        if ty.trim() != TYPE {
            findings.push(Finding::new(
                Severity::Error,
                Some("STG-2"),
                Some(entry.rel.clone()),
                format!("styleguide rule has type `{ty}`, expected `{TYPE}`"),
            ));
        }
        if concept.description().is_none_or(|d| d.trim().is_empty()) {
            findings.push(Finding::new(
                Severity::Error,
                Some("STG-3"),
                Some(entry.rel.clone()),
                "styleguide rule must set a non-empty, self-contained `description`; it is \
                 the text retrieval matches against",
            ));
        }
        if concept
            .get_str("language")
            .is_none_or(|l| l.trim().is_empty())
        {
            findings.push(Finding::new(
                Severity::Warning,
                Some("STG-4"),
                Some(entry.rel.clone()),
                "styleguide rule should set `language` so retrieval can filter by it",
            ));
        }
        if concept
            .get_str("category")
            .is_none_or(|c| c.trim().is_empty())
        {
            findings.push(Finding::new(
                Severity::Warning,
                Some("STG-4"),
                Some(entry.rel.clone()),
                "styleguide rule should set `category` so retrieval can filter by it",
            ));
        }
    }
    findings
}

impl Argosy {
    /// Validates only the `styleguide/` namespace contract, for
    /// callers that want a single namespace rather than the full
    /// [`Argosy::validate`] report.
    pub fn validate_styleguide(&self) -> Vec<Finding> {
        validate(self.root())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::str::FromStr;

    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    fn fixture_rule(name: &str) -> StyleguideRule {
        let argosy = Argosy::open(fixture(name)).unwrap();
        let rules = StyleguideRule::list(&argosy).unwrap();
        assert_eq!(rules.len(), 1, "fixture should yield exactly one rule");
        rules.into_iter().next().unwrap()
    }

    fn error_ids(findings: &[Finding]) -> Vec<Option<&'static str>> {
        findings
            .iter()
            .filter(|f| f.severity == Severity::Error)
            .map(|f| f.id)
            .collect()
    }

    #[test]
    fn list_returns_fixture_rule_with_facets_and_examples() {
        let argosy = Argosy::open(fixture("valid-acme-billing")).unwrap();
        let rules = StyleguideRule::list(&argosy).unwrap();
        assert_eq!(rules.len(), 1);
        let rule = &rules[0];
        assert_eq!(rule.id().as_str(), "styleguide/rust/naming/snake-case-vars");
        assert_eq!(rule.language(), Some("rust"));
        assert_eq!(rule.category(), Some("naming"));
        assert!(rule.good_examples().unwrap().contains("retry_count"));
        assert!(rule.bad_examples().unwrap().contains("retryCount"));
    }

    #[test]
    fn list_tolerates_malformed_concepts() {
        let argosy = Argosy::open(fixture("rule-malformed")).unwrap();
        let rules = StyleguideRule::list(&argosy).unwrap();
        // The malformed concept is skipped, not an Err for the whole list.
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].id().as_str(), "styleguide/good");
        //...and validation is where the breakage is surfaced (from the
        // generic concept pass).
        let report = Argosy::validate(fixture("rule-malformed"));
        let ids: Vec<_> = report.errors().map(|f| f.id).collect();
        assert_eq!(ids, vec![Some("STG-1")]);
    }

    #[test]
    fn examples_sections_skip_fenced_code_and_empty_sections() {
        let concept = Concept::from_str(
            "---\ntype: Styleguide Rule\ndescription: d\n---\n\
             ## Good\n\n```python\n# retry on timeout\nget(url)\n```\n\n## Bad\n\n## Gooder\n\nx\n",
        )
        .unwrap();
        let rule = StyleguideRule {
            id: ConceptId::from_str("styleguide/x").unwrap(),
            concept,
        };
        assert_eq!(
            rule.good_examples(),
            Some("```python\n# retry on timeout\nget(url)\n```")
        );
        // Present-but-empty is indistinguishable from absent downstream.
        assert_eq!(rule.bad_examples(), None);
    }

    #[test]
    fn examples_sections_stop_at_the_next_heading() {
        let concept = Concept::from_str(
            "---\ntype: Styleguide Rule\ndescription: d\n---\n\
             Intro.\n\n## Good\n\nok();\n\n## Bad\n\nbad();\n\n## Notes\n\nmore\n",
        )
        .unwrap();
        let rule = StyleguideRule {
            id: ConceptId::from_str("styleguide/x").unwrap(),
            concept,
        };
        assert_eq!(rule.good_examples(), Some("ok();"));
        assert_eq!(rule.bad_examples(), Some("bad();"));
        assert_eq!(rule.section("Nope"), None);
    }

    #[test]
    fn examples_sections_handle_closers_tilde_fences_and_prose_hashes() {
        let concept = Concept::from_str(
            "---\ntype: Styleguide Rule\ndescription: d\n---\n\
             ## Good ##\n\nok();\n\n~~~\n## Bad\nfence content, not a heading\n~~~\n\n\
             ## Bad\n\n    ## Good\n#5 indented-code and hashtag lines are prose\nbad();\n\n\
             ## Notes\n\nn\n",
        )
        .unwrap();
        let rule = StyleguideRule {
            id: ConceptId::from_str("styleguide/x").unwrap(),
            concept,
        };
        // ATX closing sequence tolerated; tilde fence contents don't terminate.
        assert_eq!(
            rule.good_examples(),
            Some("ok();\n\n~~~\n## Bad\nfence content, not a heading\n~~~")
        );
        // Neither the 4-space-indented code line nor `#5`/`#hashtag` prose
        // counts as a heading, so the section runs to `## Notes`.
        assert_eq!(
            rule.bad_examples(),
            Some("## Good\n#5 indented-code and hashtag lines are prose\nbad();")
        );
    }

    #[test]
    fn filter_narrows_by_language_and_category_exactly() {
        let mk = |id: &str, language: Option<&str>, category: Option<&str>| {
            let mut fm = yaml_serde::Mapping::new();
            fm.insert(
                yaml_serde::Value::String("type".into()),
                yaml_serde::Value::String("Styleguide Rule".into()),
            );
            fm.insert(
                yaml_serde::Value::String("description".into()),
                yaml_serde::Value::String("d".into()),
            );
            for (k, v) in [("language", language), ("category", category)] {
                if let Some(v) = v {
                    fm.insert(
                        yaml_serde::Value::String(k.into()),
                        yaml_serde::Value::String(v.into()),
                    );
                }
            }
            let concept = Concept::new(fm, "body".to_string()).unwrap();
            StyleguideRule {
                id: ConceptId::from_str(id).unwrap(),
                concept,
            }
        };
        let rules = vec![
            mk("styleguide/a", Some("rust"), Some("naming")),
            mk("styleguide/b", Some("python"), Some("naming")),
            mk("styleguide/c", Some("rust"), None),
        ];
        let named = |rules: &[StyleguideRule]| {
            rules
                .iter()
                .map(|r| r.id.as_str().to_string())
                .collect::<Vec<_>>()
        };

        assert_eq!(
            named(&StyleguideRule::filter(rules.clone(), Some("rust"), None)),
            vec!["styleguide/a", "styleguide/c"]
        );
        assert_eq!(
            named(&StyleguideRule::filter(rules.clone(), None, Some("naming"))),
            vec!["styleguide/a", "styleguide/b"]
        );
        assert_eq!(
            named(&StyleguideRule::filter(
                rules.clone(),
                Some("rust"),
                Some("naming")
            )),
            vec!["styleguide/a"]
        );
        // `Rust` ≠ `rust`: matching is exact.
        assert!(StyleguideRule::filter(rules.clone(), Some("Rust"), None).is_empty());
        assert_eq!(named(&StyleguideRule::filter(rules, None, None)).len(), 3);
    }

    #[test]
    fn unfaceted_rule_lists_but_filter_excludes_it() {
        let rule = fixture_rule("rule-unfaceted");
        assert_eq!(rule.language(), None);
        assert_eq!(rule.category(), None);
        assert!(StyleguideRule::filter(vec![rule.clone()], Some("rust"), None).is_empty());
        assert_eq!(StyleguideRule::filter(vec![rule], None, None).len(), 1);
    }

    #[test]
    fn stg2_rule_with_wrong_type_is_an_error() {
        let findings = validate(&fixture("rule-wrong-type"));
        assert_eq!(error_ids(&findings), vec![Some("STG-2")]);
        //...and the wrongly-typed concept never becomes a listed rule.
        let argosy = Argosy::open(fixture("rule-wrong-type")).unwrap();
        assert!(StyleguideRule::list(&argosy).unwrap().is_empty());
    }

    #[test]
    fn stg1_untyped_rule_is_the_generic_passes_job_not_stg2s() {
        // `validate` here knows nothing of `type` absence — that is the
        // structural layer's. The composed report must not double up.
        assert!(validate(&fixture("rule-untyped")).is_empty());
        let report = Argosy::validate(fixture("rule-untyped"));
        let ids: Vec<_> = report.errors().map(|f| f.id).collect();
        assert_eq!(ids, vec![Some("STG-1")]);
    }

    #[test]
    fn stg3_rule_requires_description() {
        let findings = validate(&fixture("rule-missing-description"));
        assert_eq!(error_ids(&findings), vec![Some("STG-3")]);
    }

    #[test]
    fn stg4_unfaceted_rule_warns_but_is_not_an_error() {
        let findings = validate(&fixture("rule-unfaceted"));
        assert!(error_ids(&findings).is_empty());
        let warnings: Vec<_> = findings
            .iter()
            .filter(|f| f.severity == Severity::Warning && f.id == Some("STG-4"))
            .collect();
        // One warning per missing facet.
        assert_eq!(warnings.len(), 2);
    }

    #[test]
    fn stg2_empty_type_is_untyped_so_it_is_not_double_reported() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("argosy.md"),
            "---\ntype: Argosy Manifest\nname: t\nargosy_version: \"1.0.0\"\n\
             okf_version: \"0.2\"\ndescription: t\n---\n# t\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("styleguide")).unwrap();
        std::fs::write(
            root.join("styleguide/x.md"),
            "---\ntype: \"\"\ndescription: d\n---\nbody\n",
        )
        .unwrap();
        assert_eq!(validate(root), vec![]);
        let report = Argosy::validate(root);
        let errors: Vec<_> = report.errors().collect();
        // The generic pass's untyped finding only — no type check on top.
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].id, Some("STG-1"));
    }

    #[test]
    fn validate_styleguide_stays_silent_on_valid_fixture() {
        let argosy = Argosy::open(fixture("valid-acme-billing")).unwrap();
        assert!(argosy.validate_styleguide().is_empty());
    }
}
