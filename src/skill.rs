//! The typed model and namespace contract for `skill/` concepts. A skill
//! is instructions a harness loads on demand: a single concept file under
//! `skill/`, or a directory holding an entry point plus materials.
//! [`Skill::list`] is tolerant — one broken skill cannot poison consumers;
//! [`validate`] surfaces the breakage. Materials stay plain concepts.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use snafu::ResultExt;

use crate::bundle::{Argosy, Finding, Namespace, Severity, is_real_dir, is_real_file};
use crate::concept::{Concept, ConceptId};
use crate::error::{IoSnafu, Result};

/// The `type` every skill entry-point concept must carry.
const TYPE: &str = "Skill";

/// How a skill is laid out on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillForm {
    /// A single concept file directly under `skill/`.
    SingleFile,
    /// A directory under `skill/` holding the entry point plus any
    /// supporting materials. `root` is the bundle-relative skill directory.
    Directory { root: PathBuf },
}

/// A discovered skill: its name (the lookup key for listing and collision
/// checks), its entry-point concept id, and its routing description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    /// The bundle's `skill/` directory.
    pub namespace_dir: PathBuf,
    /// The entry point's file stem — e.g. `deploy` for both `skill/deploy.md`
    /// and `skill/deploy/deploy.md`.
    pub name: String,
    /// The entry-point concept's id within the bundle.
    pub entry_point: ConceptId,
    /// The entry point's `description`: what the skill does and
    /// when a harness should reach for it.
    pub description: String,
    /// File- or directory-form.
    pub form: SkillForm,
}

impl Skill {
    /// Lists every contract-satisfying skill under `skill/`, sorted by
    /// name. Discovery looks at the top level only: a `.md` file is a
    /// file-form candidate, a directory a directory-form candidate whose
    /// entry point is `<dir>/<basename>.md`. Candidates failing any check
    /// are skipped silently; use [`Argosy::validate_skills`] to see why.
    pub fn list(argosy: &Argosy) -> Result<Vec<Skill>> {
        let Some(ns_dir) = argosy.namespace_dir(&Namespace::Skill) else {
            return Ok(Vec::new());
        };
        let mut skills = Vec::new();
        for candidate in candidates(&ns_dir)? {
            if let Some(skill) = load(&candidate, &ns_dir, argosy.root()) {
                skills.push(skill);
            }
        }
        skills.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(skills)
    }
}

/// One top-level `skill/` entry that could be a skill.
struct Candidate {
    /// Bundle-relative path of the entry-point concept.
    entry_rel: PathBuf,
    form: SkillForm,
}

/// Enumerates top-level candidates, skipping OKF listing/history files
/// (index/log files) and stray non-markdown files (a validation
/// question, not discovery).
fn candidates(ns_dir: &Path) -> Result<Vec<Candidate>> {
    let rd = fs::read_dir(ns_dir).context(IoSnafu {
        path: ns_dir.to_path_buf(),
    })?;
    let mut entries: Vec<fs::DirEntry> = rd.filter_map(std::result::Result::ok).collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);

    let mut out = Vec::new();
    for entry in entries {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        // The index is a derivative artifact, not bundle content — consistent
        // with `walk_bundle` skipping `.argosy` at every depth.
        if name == ".argosy" {
            continue;
        }
        // Per-entry tolerance (a stat failure must not poison the whole
        // listing); validation surfaces such entries via the structural
        // layer's generic finding.
        let Ok(ty) = entry.file_type() else {
            continue;
        };
        if ty.is_dir() {
            out.push(Candidate {
                entry_rel: PathBuf::from("skill").join(name).join(format!("{name}.md")),
                form: SkillForm::Directory {
                    root: PathBuf::from("skill").join(name),
                },
            });
        } else if ty.is_file()
            && !Namespace::is_listing_file(name)
            && Path::new(name).extension() == Some(OsStr::new("md"))
        {
            out.push(Candidate {
                entry_rel: PathBuf::from("skill").join(name),
                form: SkillForm::SingleFile,
            });
        }
    }
    Ok(out)
}

/// Builds the typed view of a candidate, or `None` if it violates the
/// entry-point contract (tolerant listing: breakage belongs to validation).
fn load(candidate: &Candidate, ns_dir: &Path, root: &Path) -> Option<Skill> {
    let path = root.join(&candidate.entry_rel);
    // Reads never follow symlinks: a symlinked entry point (file-form
    // candidates are already filtered by `file_type`; directory-form ones
    // land here) is not a skill.
    if !is_real_file(&path) {
        return None;
    }
    let concept = Concept::from_file(&path).ok()?;
    if concept.concept_type().map(str::trim) != Some(TYPE) {
        return None;
    }
    let description = concept.description()?.trim();
    if description.is_empty() || concept.body().trim().is_empty() {
        return None;
    }
    Some(Skill {
        namespace_dir: ns_dir.to_path_buf(),
        name: candidate
            .entry_rel
            .file_stem()
            .and_then(OsStr::to_str)?
            .to_string(),
        entry_point: ConceptId::try_from(candidate.entry_rel.as_path()).ok()?,
        description: description.to_string(),
        form: candidate.form.clone(),
    })
}

/// Validates the `skill/` namespace contract of the bundle at `root`.
/// Absent `skill/` is fine. Unreadable directories and unparseable or
/// untyped concepts are the structural layer's job (see
/// [`Argosy::validate`]) and are skipped here rather than doubly reported;
/// attested computations are optional, so they have no check.
pub(crate) fn validate(root: &Path) -> Vec<Finding> {
    let ns_dir = root.join("skill");
    // Only a real directory counts — a symlinked namespace must read as
    // absent here exactly as it does in listing, or validation would
    // report on files outside the bundle.
    if !is_real_dir(&ns_dir) {
        return Vec::new();
    }
    let mut findings = Vec::new();
    // An unreadable `skill/` is already the structural layer's finding;
    // reporting it here too would double-report one root cause.
    let mut entries: Vec<fs::DirEntry> = match fs::read_dir(&ns_dir) {
        Ok(rd) => rd.filter_map(std::result::Result::ok).collect(),
        Err(_) => return findings,
    };
    entries.sort_by_key(std::fs::DirEntry::file_name);

    for entry in entries {
        let name = entry.file_name();
        let name = name.to_string_lossy().into_owned();
        if name == ".argosy" {
            continue; // derivative index dir, consistent with `walk_bundle`
        }
        let rel = PathBuf::from("skill").join(&name);
        // A per-entry stat failure is already the structural layer's;
        // reporting it here too would double-report one root cause.
        let Ok(ty) = entry.file_type() else {
            continue;
        };
        // A symlinked entry is the structural layer's finding (reads
        // never follow symlinks); contract checks on it would read
        // through the link.
        if ty.is_symlink() {
            continue;
        }

        if ty.is_file() {
            if Namespace::is_listing_file(&name) {
                continue;
            }
            if Path::new(&name).extension() != Some(OsStr::new("md")) {
                findings.push(Finding::new(
                    Severity::Warning,
                    Some("SKL-1"),
                    Some(rel),
                    "a skill is a markdown concept file or a directory; this stray file \
                     is neither (SKL-1)",
                ));
                continue;
            }
            if let Ok(concept) = Concept::from_file(root.join(&rel)) {
                entry_point_findings(&rel, &concept, &mut findings);
            }
        } else if ty.is_dir() {
            // An unreadable skill directory is the structural layer's
            // finding; claiming the entry point is missing would be
            // a guess, so this layer stays out of it.
            let Ok(dir_entries) = fs::read_dir(root.join(&rel)) else {
                continue;
            };
            let dir_entries: Vec<fs::DirEntry> =
                dir_entries.filter_map(std::result::Result::ok).collect();
            let entry_name = format!("{name}.md");
            let entry_rel = rel.join(&entry_name);
            // The entry point must exist *as a file*: a directory named
            // `deploy.md` does not satisfy the entry-point contract. An
            // entry whose own metadata failed to stat falls to the
            // structural layer, so guessing here would only add noise —
            // treat it as seen. A symlinked entry point is likewise the
            // structural layer's finding; claiming it is missing here
            // would double-report one root cause.
            let entry_point = dir_entries
                .iter()
                .find(|e| e.file_name() == *OsStr::new(&entry_name));
            let entry_point_file = entry_point.is_some_and(|e| {
                e.file_type()
                    .map_or(true, |t| t.is_file() || t.is_symlink())
            });
            if !entry_point_file {
                findings.push(Finding::new(
                    Severity::Error,
                    Some("SKL-2"),
                    Some(rel.clone()),
                    format!(
                        "directory-form skill `skill/{name}/` must contain its entry point \
                         `{entry_name}`"
                    ),
                ));
                continue;
            }
            if entry_point.is_some_and(|e| e.file_type().is_ok_and(|t| t.is_symlink())) {
                continue;
            }
            if let Ok(concept) = Concept::from_file(root.join(&entry_rel)) {
                entry_point_findings(&entry_rel, &concept, &mut findings);
            }
            // Supporting materials should live under `references/`.
            // Info only — there is no hardness to check here and overreach
            // would penalize legitimate layouts.
            let stray = dir_entries.iter().any(|e| {
                let n = e.file_name();
                let s = n.to_str().unwrap_or("");
                n != *OsStr::new(&entry_name)
                    && n != "references"
                    && n != ".argosy"
                    // OKF listing/history files are legitimate anywhere.
                    && !Namespace::is_listing_file(s)
            });
            if stray {
                findings.push(Finding::new(
                    Severity::Info,
                    Some("SKL-6"),
                    Some(rel),
                    "supporting materials live outside `references/`; the `references/` \
                     convention keeps entry points uncluttered (SKL-6)",
                ));
            }
        } else {
            findings.push(Finding::new(
                Severity::Error,
                Some("SKL-1"),
                Some(rel),
                "a skill must be a markdown concept file or a directory; this entry is \
                 neither (SKL-1)",
            ));
        }
    }
    findings
}

/// The entry-point checks on a parsed concept. An untyped concept is
/// wholly the structural layer's finding, so all checks are skipped for it;
/// the type check fires only on a present-but-wrong `type`.
pub(crate) fn entry_point_findings(rel: &Path, concept: &Concept, findings: &mut Vec<Finding>) {
    let Some(ty) = concept.concept_type() else {
        return;
    };
    // A present-but-empty `type` is "untyped" as far as OKF conformance is
    // concerned (`is_okf_conformant`): the generic pass already reports it,
    // so the type check must not double-report.
    if ty.trim().is_empty() {
        return;
    }
    if ty.trim() != TYPE {
        findings.push(Finding::new(
            Severity::Error,
            Some("SKL-3"),
            Some(rel.to_path_buf()),
            format!("skill entry point has type `{ty}`, expected `{TYPE}`"),
        ));
    }
    if concept.description().is_none_or(|d| d.trim().is_empty()) {
        findings.push(Finding::new(
            Severity::Error,
            Some("SKL-4"),
            Some(rel.to_path_buf()),
            "skill entry point must set a non-empty `description`; harness routing depends \
             on it",
        ));
    }
    if concept.body().trim().is_empty() {
        findings.push(Finding::new(
            Severity::Error,
            Some("SKL-5"),
            Some(rel.to_path_buf()),
            "skill entry point body holds the skill's instructions and must not be empty",
        ));
    }
}

impl Argosy {
    /// Validates only the `skill/` namespace contract, for
    /// callers that want a single namespace rather than the full
    /// [`Argosy::validate`] report.
    pub fn validate_skills(&self) -> Vec<Finding> {
        validate(self.root())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    fn error_ids(findings: &[Finding]) -> Vec<Option<&'static str>> {
        findings
            .iter()
            .filter(|f| f.severity == Severity::Error)
            .map(|f| f.id)
            .collect()
    }

    #[test]
    fn list_returns_file_and_directory_form_skills() {
        let argosy = Argosy::open(fixture("valid-acme-billing")).unwrap();
        let skills = Skill::list(&argosy).unwrap();
        assert_eq!(skills.len(), 2);

        let ledger = &skills[0];
        assert_eq!(ledger.name, "reconcile-ledger");
        assert_eq!(ledger.entry_point.as_str(), "skill/reconcile-ledger");
        assert!(ledger.description.contains("payment processor"));
        assert_eq!(ledger.form, SkillForm::SingleFile);

        let keys = &skills[1];
        assert_eq!(keys.name, "rotate-api-keys");
        assert_eq!(
            keys.entry_point.as_str(),
            "skill/rotate-api-keys/rotate-api-keys"
        );
        assert_eq!(
            keys.form,
            SkillForm::Directory {
                root: PathBuf::from("skill/rotate-api-keys")
            }
        );
    }

    #[test]
    fn list_never_surfaces_supporting_materials_as_skills() {
        let argosy = Argosy::open(fixture("valid-acme-billing")).unwrap();
        let skills = Skill::list(&argosy).unwrap();
        let names: Vec<_> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(!names.contains(&"checklist"));
        //...but the checklist is still a raw concept of the namespace.
        let concepts = argosy.concepts(&Namespace::Skill).unwrap();
        assert!(
            concepts
                .iter()
                .any(|(id, _)| id.as_str() == "skill/rotate-api-keys/references/checklist")
        );
    }

    #[test]
    fn list_on_absent_namespace_is_empty() {
        let argosy = Argosy::open(fixture("untyped-concept")).unwrap();
        assert!(Skill::list(&argosy).unwrap().is_empty());
        assert!(argosy.validate_skills().is_empty());
    }

    #[test]
    fn skl1_stray_non_markdown_file_is_a_warning() {
        let findings = validate(&fixture("skill-stray-file"));
        assert_eq!(
            error_ids(&findings),
            vec![],
            "stray file must not be an error"
        );
        let warnings: Vec<_> = findings
            .iter()
            .filter(|f| f.severity == Severity::Warning && f.id == Some("SKL-1"))
            .collect();
        assert_eq!(warnings.len(), 1);
        assert_eq!(
            warnings[0].path.as_deref(),
            Some(Path::new("skill/notes.txt"))
        );
    }

    #[test]
    fn skl2_directory_skill_requires_entry_point() {
        let findings = validate(&fixture("skill-missing-entry-point"));
        assert_eq!(error_ids(&findings), vec![Some("SKL-2")]);
        assert_eq!(findings[0].path.as_deref(), Some(Path::new("skill/deploy")));
    }

    #[test]
    fn skl3_skill_entry_point_must_be_typed_skill() {
        let findings = validate(&fixture("skill-wrong-type"));
        assert_eq!(error_ids(&findings), vec![Some("SKL-3")]);
    }

    #[test]
    fn skl1_untyped_skill_is_the_generic_passes_job() {
        assert!(validate(&fixture("skill-untyped")).is_empty());
        let report = Argosy::validate(fixture("skill-untyped"));
        let ids: Vec<_> = report.errors().map(|f| f.id).collect();
        // `skill/` has no generic concept-conformance ID of its own, so the
        // finding carries none rather than a misleading bundle-level one.
        assert_eq!(ids, vec![None]);
    }

    #[test]
    fn skl4_skill_requires_description() {
        let findings = validate(&fixture("skill-missing-description"));
        assert_eq!(error_ids(&findings), vec![Some("SKL-4")]);
    }

    #[test]
    fn skl5_skill_requires_nonempty_body() {
        let findings = validate(&fixture("skill-empty-body"));
        assert_eq!(error_ids(&findings), vec![Some("SKL-5")]);
    }

    #[test]
    fn skl6_materials_outside_references_are_info_only() {
        let findings = validate(&fixture("skill-stray-materials"));
        assert!(error_ids(&findings).is_empty());
        let infos: Vec<_> = findings
            .iter()
            .filter(|f| f.severity == Severity::Info && f.id == Some("SKL-6"))
            .collect();
        assert_eq!(infos.len(), 1);
        // The valid fixture keeps materials under `references/` — no finding.
        assert!(validate(&fixture("valid-acme-billing")).is_empty());
    }

    #[test]
    fn validate_skills_stays_silent_on_valid_fixture() {
        let argosy = Argosy::open(fixture("valid-acme-billing")).unwrap();
        assert!(argosy.validate_skills().is_empty());
    }

    const MANIFEST: &str = "---\ntype: Argosy Manifest\nname: t\nargosy_version: \"1.0.0\"\n\
                            okf_version: \"0.2\"\ndescription: t\n---\n# t\n";
    const VALID_SKILL: &str = "---\ntype: Skill\ndescription: does things\n---\ndo the things\n";

    /// Builds a minimal argosy in a temp dir: a valid manifest plus the given
    /// `(relative path, contents)` files.
    fn temp_bundle(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("argosy.md"), MANIFEST).unwrap();
        for (rel, contents) in files {
            let path = dir.path().join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        dir
    }

    #[test]
    fn dot_argosy_inside_skill_is_neither_skill_nor_finding() {
        let bundle = temp_bundle(&[
            ("skill/.argosy/index.db", "placeholder"),
            ("skill/deploy.md", VALID_SKILL),
        ]);
        assert_eq!(validate(bundle.path()), vec![]);
        let argosy = Argosy::open(bundle.path()).unwrap();
        let skills = Skill::list(&argosy).unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "deploy");
    }

    #[test]
    fn skl3_empty_type_is_untyped_so_it_is_not_double_reported() {
        let bundle = temp_bundle(&[(
            "skill/deploy.md",
            "---\ntype: \"\"\ndescription: d\n---\nbody\n",
        )]);
        assert_eq!(validate(bundle.path()), vec![]);
        let report = Argosy::validate(bundle.path());
        let errors: Vec<_> = report.errors().collect();
        // The generic pass's untyped finding only.
        assert_eq!(errors.len(), 1);
        assert_eq!(
            errors[0].path.as_deref(),
            Some(Path::new("skill/deploy.md"))
        );
        assert!(errors[0].message.contains("type"));
    }

    #[test]
    fn skl2_entry_point_must_be_a_file_not_a_directory() {
        let bundle = temp_bundle(&[]);
        fs::create_dir_all(bundle.path().join("skill/deploy/deploy.md")).unwrap();
        let findings = validate(bundle.path());
        assert_eq!(error_ids(&findings), vec![Some("SKL-2")]);
    }

    #[test]
    fn skl6_listing_files_inside_a_skill_dir_are_legitimate() {
        let bundle = temp_bundle(&[
            ("skill/deploy/deploy.md", VALID_SKILL),
            ("skill/deploy/index.md", "# skill index\n"),
        ]);
        assert_eq!(validate(bundle.path()), vec![]);
    }

    /// Symlinked entry points — file-form and directory-form — are not
    /// skills: reads never follow symlinks, whatever the link points at.
    #[cfg(unix)]
    #[test]
    fn symlinked_entry_points_are_not_skills() {
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("skill.md");
        std::fs::write(&secret, VALID_SKILL).unwrap();

        let bundle = temp_bundle(&[("skill/real.md", VALID_SKILL)]);
        std::os::unix::fs::symlink(&secret, bundle.path().join("skill/leak.md")).unwrap();
        std::fs::create_dir_all(bundle.path().join("skill/deploy")).unwrap();
        std::os::unix::fs::symlink(&secret, bundle.path().join("skill/deploy/deploy.md")).unwrap();

        let argosy = Argosy::open(bundle.path()).unwrap();
        let skills = Skill::list(&argosy).unwrap();
        let names: Vec<_> = skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["real"]);
        // The namespace pass stays out of symlink reporting entirely
        // (the structural layer owns it), so it reports nothing here.
        assert_eq!(validate(bundle.path()), vec![]);
    }

    /// A symlinked `skill/` namespace reads as absent everywhere:
    /// validation does not read through the link.
    #[cfg(unix)]
    #[test]
    fn symlinked_skill_namespace_reads_as_absent_in_validation() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(outside.path().join("notes")).unwrap();
        std::fs::write(outside.path().join("notes/stray.txt"), "x\n").unwrap();

        let bundle = temp_bundle(&[]);
        std::os::unix::fs::symlink(outside.path(), bundle.path().join("skill")).unwrap();

        assert!(validate(bundle.path()).is_empty());
        // The structural layer owns the symlinked-namespace finding (STR-7).
        let report = Argosy::validate(bundle.path());
        let ids: Vec<_> = report.errors().map(|f| f.id).collect();
        assert_eq!(ids, vec![Some("STR-7")]);
    }
}
