//! The multi-argosy project context: one local (writable) argosy plus any
//! number of imported (read-only) ones, with qualified identity,
//! `argosy://` URIs, and precedence-ordered aggregate listings. Imports
//! are read-only by construction; duplicate names are rejected; v1 URIs
//! have no percent-encoding — ids outside `[A-Za-z0-9._-/]` error.

use std::path::{Path, PathBuf};

use snafu::ensure;

use crate::bundle::{Argosy, Namespace};
use crate::concept::{Concept, ConceptId};
use crate::error::{
    DuplicateArgosyNameSnafu, InvalidUriSnafu, NotAnArgosySnafu, Result, UnknownArgosySnafu,
    ValidationSnafu,
};
use crate::local::LocalArgosy;
use crate::skill::Skill;
use crate::styleguide::StyleguideRule;

/// The characters an `argosy://` URI body may contain (v1: no
/// percent-encoding, so this is the identity charset wholesale).
const URI_CHARSET: &str = "[A-Za-z0-9._-/]";

/// A concept's identity qualified by its argosy: two argosys both
/// defining `document/architecture.md` are two distinct [`QualifiedConceptId`]s.
/// The `id` is bundle-relative and includes the namespace prefix, matching
/// the codebase-wide [`ConceptId`] convention.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize)]
pub struct QualifiedConceptId {
    /// The owning argosy's manifest name (e.g. `acme-billing`).
    pub argosy: String,
    /// The namespace the concept lives in.
    pub namespace: Namespace,
    /// The concept's bundle-relative id, including the namespace prefix.
    pub id: ConceptId,
}

impl QualifiedConceptId {
    /// Formats this identity as an `argosy://<argosy>/<namespace>/<concept-id>`
    /// URI. The fields are public, so an unrepresentable value cannot be
    /// rejected here: it will not round-trip through
    /// [`QualifiedConceptId::from_uri`].
    pub fn to_uri(&self) -> String {
        format!("argosy://{}/{}", self.argosy, self.id)
    }

    /// Parses an `argosy://` URI strictly. Rejects (as
    /// [`crate::error::Error::InvalidUri`]): a scheme other than `argosy://`;
    /// fewer than namespace + one concept-id segment; characters outside
    /// `[A-Za-z0-9._-/]`; a first segment that is not a reserved namespace
    /// name; or an id [`ConceptId`] itself rejects.
    pub fn from_uri(uri: &str) -> Result<Self> {
        let bad = |reason: &str| {
            InvalidUriSnafu {
                uri: uri.to_string(),
                reason: reason.to_string(),
            }
            .fail()
        };
        let Some(body) = uri.strip_prefix("argosy://") else {
            return bad("the scheme must be `argosy://`");
        };
        ensure!(
            body.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/')),
            InvalidUriSnafu {
                uri: uri.to_string(),
                reason: format!(
                    "characters outside {URI_CHARSET} are not representable (v1 has no percent-encoding)"
                ),
            }
        );
        let Some((argosy, path)) = body.split_once('/') else {
            return bad("expected `argosy://<argosy>/<namespace>/<concept-id>`");
        };
        if argosy.is_empty() {
            return bad("the argosy name is empty");
        }
        let namespace_segment = path.split('/').next().unwrap_or_default();
        let namespace = Namespace::from_dir_name(namespace_segment);
        if !namespace.is_reserved() {
            return bad(&format!(
                "namespace `{namespace_segment}` is not one of the reserved names {} \
                 (custom namespaces are not addressable by URI: concept identity across \
                 them is producer-defined)",
                Namespace::RESERVED.join(", ")
            ));
        }
        if !path.contains('/') {
            return bad("expected a namespace segment and at least one concept-id segment");
        }
        if path.split('/').any(|s| s == ".") || path.ends_with(".md") {
            return bad(
                "non-canonical spelling: ids are extensionless, so `.` segments and a \
                 trailing `.md` are rejected rather than normalized",
            );
        }
        let id: ConceptId = path
            .parse::<ConceptId>()
            .map_err(|e: crate::error::Error| {
                InvalidUriSnafu {
                    uri: uri.to_string(),
                    reason: e.to_string(),
                }
                .build()
            })?;
        // `path` is `<namespace>/<...>` by construction above, so the id's
        // first segment already matches the namespace.
        Ok(QualifiedConceptId {
            argosy: argosy.to_string(),
            namespace,
            id,
        })
    }
}

/// A borrowed reference to one of the context's argosys, distinguishing the
/// local one (write-bearing) from imported ones (read-only).
#[derive(Debug)]
pub enum ArgosyRef<'a> {
    /// The local, writable argosy.
    Local(&'a LocalArgosy),
    /// An imported, read-only argosy.
    Imported(&'a Argosy),
}

/// A skill in an aggregate listing, tagged with its origin argosy and
/// whether a higher-precedence argosy shadows it under the same name
/// (losers are annotated, never silently dropped).
#[derive(Debug, Clone)]
pub struct SkillListing {
    /// The manifest name of the argosy providing this skill.
    pub argosy: String,
    /// The skill itself.
    pub skill: Skill,
    /// True iff another argosy earlier in precedence order provides a skill
    /// with the same name; this listing would lose a `resolve_skill` lookup.
    pub shadowed: bool,
}

/// A styleguide rule in an aggregate listing, tagged with its origin argosy
/// (rules combine across argosys, never replace).
#[derive(Debug, Clone)]
pub struct RuleListing {
    /// The manifest name of the argosy providing this rule.
    pub argosy: String,
    /// The rule itself.
    pub rule: StyleguideRule,
}

/// The active argosys of a project: exactly one local (writable) argosy and
/// any number of imported (read-only) ones. The composition rules live here
/// as enforced behavior; see the module docs.
#[derive(Debug)]
pub struct ProjectContext {
    local: LocalArgosy,
    imported: Vec<Argosy>,
}

impl ProjectContext {
    /// Opens the local argosy plus every imported one, in registration order.
    /// Fails if any argosy fails to open or if two share a manifest name —
    /// erroring at open keeps identity-by-name unambiguous.
    pub fn open(
        local_path: impl AsRef<Path>,
        imported_paths: impl IntoIterator<Item = PathBuf>,
    ) -> Result<Self> {
        let local = LocalArgosy::open(local_path)?;
        let imported = imported_paths
            .into_iter()
            .map(Argosy::open)
            .collect::<Result<Vec<_>>>()?;

        let local_name = local.manifest().name();
        ensure!(
            !imported.iter().any(|a| a.manifest().name() == local_name),
            DuplicateArgosyNameSnafu {
                name: local_name.to_string()
            }
        );
        for (i, argosy) in imported.iter().enumerate() {
            let name = argosy.manifest().name();
            ensure!(
                !imported[..i].iter().any(|a| a.manifest().name() == name),
                DuplicateArgosyNameSnafu {
                    name: name.to_string()
                }
            );
        }
        Ok(Self { local, imported })
    }

    /// The local argosy with its full write surface. The only mutable
    /// accessor: imports are strictly read-only, including any `memory/`
    /// directory they happen to contain.
    pub fn local(&self) -> &LocalArgosy {
        &self.local
    }

    /// Opens the standard argosy set of a project: the local bundle at
    /// `<project>/.argosy/default`, other checkouts in `<project>/.argosy/`,
    /// then the global store ([`crate::pull::global_argosy_dir`]).
    /// Manifest-less directories and `index.db` are skipped; duplicates
    /// hard-fail; a missing `.argosy/default` points at `argosy init`.
    pub fn open_project(project_root: impl AsRef<Path>) -> Result<Self> {
        let globals = crate::pull::global_argosy_dir()?;
        Self::open_project_with_globals(project_root, &globals)
    }

    /// [`Self::open_project`] with an explicit globals tier (tests inject a
    /// tempdir instead of touching `~/.local/state`).
    pub(crate) fn open_project_with_globals(
        project_root: impl AsRef<Path>,
        globals_root: &Path,
    ) -> Result<Self> {
        let project_dir = project_root.as_ref().join(crate::pull::PROJECT_ARGOSY_DIR);
        // Everything under the project dir that is a bundle (a directory
        // holding `argosy.md`), in sorted order, minus the local checkout.
        let mut imported = Vec::new();
        collect_checkouts(
            &project_dir,
            Some(crate::pull::LOCAL_ARGOSY_NAME),
            &mut imported,
        );
        collect_checkouts(globals_root, None, &mut imported);

        let local = project_dir.join(crate::pull::LOCAL_ARGOSY_NAME);
        ensure!(
            local.join("argosy.md").is_file(),
            NotAnArgosySnafu {
                path: project_dir.clone(),
                reason: "no `.argosy/default` bundle for this project (run `argosy init`)"
            }
        );
        Self::open(local, imported)
    }

    /// The imported argosys, read-only, in registration (precedence) order.
    pub fn imported(&self) -> impl Iterator<Item = &Argosy> {
        self.imported.iter()
    }

    /// Finds an active argosy by manifest name — the local one first, then
    /// the imported ones in registration order. Names are unique (enforced
    /// at [`ProjectContext::open`]), so the search order cannot matter; it
    /// just short-circuits.
    pub fn argosy_named(&self, name: &str) -> Option<ArgosyRef<'_>> {
        if self.local.manifest().name() == name {
            return Some(ArgosyRef::Local(&self.local));
        }
        self.imported
            .iter()
            .find(|a| a.manifest().name() == name)
            .map(ArgosyRef::Imported)
    }

    /// Resolves a qualified id to the concept in that argosy: reads the
    /// file at the id's bundle-relative path. Errors:
    /// [`crate::error::Error::UnknownArgosy`] or
    /// [`crate::error::Error::ConceptNotFound`]. A defensive check refuses
    /// an id outside its namespace; symlinks are never followed.
    pub fn resolve(&self, qid: &QualifiedConceptId) -> Result<Concept> {
        let first = qid.id.as_str().split('/').next().unwrap_or_default();
        ensure!(
            first == qid.namespace.as_dir_name() && qid.id.as_str().contains('/'),
            ValidationSnafu {
                reason: format!(
                    "concept id `{}` is not under the `{}/` namespace",
                    qid.id,
                    qid.namespace.as_dir_name()
                ),
            }
        );
        let argosy = match self.argosy_named(&qid.argosy) {
            Some(ArgosyRef::Local(local)) => &**local,
            Some(ArgosyRef::Imported(argosy)) => argosy,
            None => {
                return UnknownArgosySnafu {
                    name: qid.argosy.clone(),
                }
                .fail();
            }
        };
        let not_found = || crate::error::ConceptNotFoundSnafu { id: qid.id.clone() }.fail();
        // A symlinked (or absent) namespace directory is treated as absent
        // crate-wide (`Argosy::namespace_dir`); honor that here.
        if argosy.namespace_dir(&qid.namespace).is_none() {
            return not_found();
        }
        let mut cursor = argosy.root().to_path_buf();
        let mut target = None;
        for component in qid.id.to_relative_path().components() {
            cursor.push(component);
            let Ok(meta) = std::fs::symlink_metadata(&cursor) else {
                return not_found();
            };
            if meta.file_type().is_symlink() {
                return not_found();
            }
            target = Some(meta);
        }
        if !target.is_some_and(|m| m.file_type().is_file()) {
            return not_found();
        }
        Concept::from_file(cursor)
    }

    /// Parses `uri` ([`QualifiedConceptId::from_uri`]) and resolves it
    /// ([`ProjectContext::resolve`]).
    pub fn read_uri(&self, uri: &str) -> Result<Concept> {
        let qid = QualifiedConceptId::from_uri(uri)?;
        self.resolve(&qid)
    }

    /// Every active argosy in precedence order (local first, then imported
    /// in registration order) paired with its manifest name.
    fn in_precedence_order(&self) -> impl Iterator<Item = (&str, &Argosy)> {
        std::iter::once(&*self.local)
            .chain(self.imported.iter())
            .map(|a| (a.manifest().name(), a))
    }

    /// Lists every skill across all active argosys in precedence order,
    /// each tagged with its origin argosy. Collisions annotate
    /// rather than drop: every skill whose `name` already appeared earlier
    /// in precedence order is flagged [`SkillListing::shadowed`].
    pub fn list_skills(&self) -> Result<Vec<SkillListing>> {
        let mut listings = Vec::new();
        for (name, argosy) in self.in_precedence_order() {
            for skill in Skill::list(argosy)? {
                listings.push(SkillListing {
                    argosy: name.to_string(),
                    skill,
                    shadowed: false,
                });
            }
        }
        let mut seen = std::collections::HashSet::new();
        for listing in &mut listings {
            if !seen.insert(listing.skill.name.clone()) {
                listing.shadowed = true;
            }
        }
        Ok(listings)
    }

    /// The highest-precedence skill of that name: the local argosy wins over
    /// every import, an earlier import over a later one — the first matching
    /// entry of [`ProjectContext::list_skills`]. Returned by value: a borrow
    /// is not expressible without a listing cache, and a cache would go
    /// stale on writes through [`ProjectContext::local`].
    pub fn resolve_skill(&self, name: &str) -> Result<Option<SkillListing>> {
        Ok(self
            .list_skills()?
            .into_iter()
            .find(|listing| listing.skill.name == name))
    }

    /// Lists styleguide rules across all active argosys in precedence order
    /// (combined, never replaced), each tagged with its origin argosy and
    /// narrowed by exact facet matches via [`StyleguideRule::filter`]
    /// (`None` = no constraint). These filesystem-level listings are the
    /// ground truth the semantic index must match.
    pub fn list_rules(
        &self,
        language: Option<&str>,
        category: Option<&str>,
    ) -> Result<Vec<RuleListing>> {
        let mut listings = Vec::new();
        for (name, argosy) in self.in_precedence_order() {
            for rule in StyleguideRule::filter(StyleguideRule::list(argosy)?, language, category) {
                listings.push(RuleListing {
                    argosy: name.to_string(),
                    rule,
                });
            }
        }
        Ok(listings)
    }
}

/// Appends the bundle checkouts directly under `dir`: every subdirectory
/// holding `argosy.md`, in sorted order for a deterministic precedence, not
/// following symlinks (consistent with the bundle walk's policy). `skip`
/// excludes one checkout name (the local `default`, visited separately).
/// Missing or unreadable `dir` means "no argosies here", not an error.
fn collect_checkouts(dir: &Path, skip: Option<&str>, out: &mut Vec<PathBuf>) {
    let Ok(mut entries) = std::fs::read_dir(dir).map(|rd| rd.flatten().collect::<Vec<_>>()) else {
        return;
    };
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name();
        if skip.is_some_and(|skip| name == skip) {
            continue;
        }
        let Ok(ty) = entry.file_type() else { continue };
        if !ty.is_dir() {
            continue; // e.g. the derived `index.db` — never a checkout
        }
        let path = entry.path();
        if path.join("argosy.md").is_file() {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;
    use crate::error::Error;

    /// Writes a minimal openable argosy (manifest + the given files) into a
    /// fresh tempdir. `files` are `(bundle-relative path, file content)`.
    fn make_argosy(name: &str, files: &[(&str, &str)]) -> TempDir {
        let dir = TempDir::new().unwrap();
        let manifest = format!(
            "---\ntype: Argosy Manifest\nname: {name}\nargosy_version: \"0.3.1\"\n---\n# {name}\n"
        );
        write_file(dir.path(), "argosy.md", &manifest);
        for (rel, content) in files {
            write_file(dir.path(), rel, content);
        }
        dir
    }

    fn write_file(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn qid(argosy: &str, namespace: Namespace, id: &str) -> QualifiedConceptId {
        QualifiedConceptId {
            argosy: argosy.to_string(),
            namespace,
            id: id.parse().unwrap(),
        }
    }

    // --- open_project discovery ---

    /// A project layout: `.argosy/<names>` (the local one must be named
    /// `default`), plus a separate globals root with its own argosies.
    fn project_layout(names: &[&str], globals: &[&str]) -> (TempDir, TempDir) {
        let project = TempDir::new().unwrap();
        let project_dir = project.path().join(crate::pull::PROJECT_ARGOSY_DIR);
        for name in names {
            let manifest = format!(
                "---\ntype: Argosy Manifest\nname: proj-{name}\nargosy_version: \"1.0.0\"\n---\n# proj-{name}\n"
            );
            write_file(&project_dir.join(name), "argosy.md", &manifest);
        }
        let globals_root = TempDir::new().unwrap();
        for name in globals {
            let manifest = format!(
                "---\ntype: Argosy Manifest\nname: global-{name}\nargosy_version: \"1.0.0\"\n---\n# global-{name}\n"
            );
            write_file(&globals_root.path().join(name), "argosy.md", &manifest);
        }
        (project, globals_root)
    }

    #[test]
    fn open_project_loads_default_children_and_globals_in_precedence_order() {
        let (project, globals) = project_layout(&["default", "beta", "alpha"], &["extra"]);
        // Non-checkout noise that discovery must ignore.
        write_file(
            &project.path().join(crate::pull::PROJECT_ARGOSY_DIR),
            "index.db",
            "not a directory",
        );
        write_file(
            &project
                .path()
                .join(crate::pull::PROJECT_ARGOSY_DIR)
                .join("notado"),
            "argosy.notmd",
            "x",
        );

        let ctx =
            ProjectContext::open_project_with_globals(project.path(), globals.path()).unwrap();

        assert_eq!(ctx.local().manifest().name(), "proj-default");
        let imported: Vec<&str> = ctx.imported().map(|a| a.manifest().name()).collect();
        assert_eq!(
            imported,
            vec!["proj-alpha", "proj-beta", "global-extra"],
            "project checkouts sorted, globals last"
        );
    }

    #[test]
    fn open_project_without_default_points_at_init() {
        let (project, globals) = project_layout(&["beta"], &[]);
        let err =
            ProjectContext::open_project_with_globals(project.path(), globals.path()).unwrap_err();
        assert!(
            err.to_string().contains(".argosy/default") && err.to_string().contains("argosy init"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn open_project_rejects_duplicate_manifest_names_across_tiers() {
        let (project, globals) = project_layout(&["default"], &[]);
        // A global with the same manifest name as the local default.
        write_file(
            &globals.path().join("rogue"),
            "argosy.md",
            "---\ntype: Argosy Manifest\nname: proj-default\nargosy_version: \"1.0.0\"\n---\n# rogue\n",
        );
        let err =
            ProjectContext::open_project_with_globals(project.path(), globals.path()).unwrap_err();
        assert!(err.to_string().contains("proj-default"), "{err}");
    }

    const SKILL_DEPLOY: &str =
        "---\ntype: Skill\ndescription: Deploy the service.\n---\nDeploy steps.\n";

    fn rule_concept(name: &str) -> String {
        format!(
            "---\ntype: Styleguide Rule\ndescription: {name}.\nlanguage: rust\ncategory: naming\n---\n## Good\n```\nfoo\n```\n"
        )
    }

    // --- URI parse/format ---

    #[test]
    fn uri_round_trips_for_deeply_nested_concept_ids() {
        let original = qid(
            "acme-billing",
            Namespace::Document,
            "document/decisions/2026-05-caching",
        );
        assert_eq!(
            original.to_uri(),
            "argosy://acme-billing/document/decisions/2026-05-caching"
        );
        assert_eq!(
            QualifiedConceptId::from_uri(&original.to_uri()).unwrap(),
            original
        );
    }

    #[test]
    fn uri_rejects_wrong_scheme_missing_segments_and_bad_namespace() {
        for uri in [
            "http://acme-billing/document/x",
            "argosy:acme-billing/document/x",
            "argosy://acme-billing",
            "argosy://acme-billing/document",
            // `roadmap/` is a custom namespace: not URI-addressable.
            "argosy://acme-billing/roadmap/plans",
        ] {
            assert!(
                matches!(
                    QualifiedConceptId::from_uri(uri),
                    Err(Error::InvalidUri { .. })
                ),
                "expected InvalidUri for `{uri}`"
            );
        }
    }

    #[test]
    fn uri_rejects_out_of_charset_ids_instead_of_mangling_them() {
        for uri in [
            "argosy://acme-billing/document/foo%20bar",
            "argosy://acme-billing/document/has space",
            "argosy://acme-billing/document/café",
        ] {
            assert!(
                matches!(
                    QualifiedConceptId::from_uri(uri),
                    Err(Error::InvalidUri { .. })
                ),
                "expected InvalidUri for `{uri}`"
            );
        }
    }

    // --- Qualified identity ---

    #[test]
    fn same_concept_in_two_argosys_yields_distinct_qualified_ids() {
        let a = make_argosy(
            "alpha",
            &[("document/architecture.md", "Alpha architecture.\n")],
        );
        let b = make_argosy(
            "beta",
            &[("document/architecture.md", "Beta architecture.\n")],
        );
        let ctx = ProjectContext::open(a.path(), [b.path().to_path_buf()]).unwrap();

        let qa = qid("alpha", Namespace::Document, "document/architecture");
        let qb = qid("beta", Namespace::Document, "document/architecture");
        assert_ne!(qa, qb, "identity is qualified by argosy");

        assert!(ctx.resolve(&qa).unwrap().body().contains("Alpha"));
        assert!(ctx.resolve(&qb).unwrap().body().contains("Beta"));
    }

    #[test]
    fn open_rejects_duplicate_argosy_names_with_the_name_in_the_error() {
        let local = make_argosy("dup", &[]);
        let imp_dup = make_argosy("dup", &[]);
        let mine_a = make_argosy("mine", &[]);
        let mine_b = make_argosy("mine", &[]);

        // Local colliding with an import.
        let err = ProjectContext::open(local.path(), [imp_dup.path().to_path_buf()]).unwrap_err();
        assert!(
            matches!(err, Error::DuplicateArgosyName { ref name } if name == "dup"),
            "{err}"
        );
        assert!(err.to_string().contains("`dup`"));

        // Two imports colliding with each other.
        let err = ProjectContext::open(
            mine_a.path(),
            [mine_b.path().to_path_buf(), imp_dup.path().to_path_buf()],
        )
        .unwrap_err();
        assert!(
            matches!(err, Error::DuplicateArgosyName { ref name } if name == "mine"),
            "{err}"
        );
    }

    #[test]
    fn a_failing_import_path_fails_the_whole_context() {
        let local = make_argosy("local", &[]);
        let missing = local.path().join("does-not-exist");
        assert!(ProjectContext::open(local.path(), [missing]).is_err());
    }

    // --- Lookups ---

    #[test]
    fn resolve_unknown_argosy_names_it_in_the_error() {
        let local = make_argosy("local", &[]);
        let ctx = ProjectContext::open(local.path(), []).unwrap();
        let err = ctx
            .resolve(&qid("ghost", Namespace::Document, "document/x"))
            .unwrap_err();
        assert!(matches!(err, Error::UnknownArgosy { ref name } if name == "ghost"));
    }

    #[test]
    fn resolve_missing_concept_is_concept_not_found() {
        let local = make_argosy("local", &[]);
        let ctx = ProjectContext::open(local.path(), []).unwrap();
        let err = ctx
            .resolve(&qid("local", Namespace::Document, "document/nope"))
            .unwrap_err();
        assert!(matches!(err, Error::ConceptNotFound { .. }));
    }

    // --- Skill precedence ---

    #[test]
    fn skill_collision_keeps_both_and_flags_the_imported_one_shadowed() {
        let local = make_argosy("local", &[("skill/deploy.md", SKILL_DEPLOY)]);
        let imp = make_argosy("imp", &[("skill/deploy.md", SKILL_DEPLOY)]);
        let ctx = ProjectContext::open(local.path(), [imp.path().to_path_buf()]).unwrap();

        let skills = ctx.list_skills().unwrap();
        assert_eq!(skills.len(), 2, "losers are never dropped");
        assert_eq!(skills[0].argosy, "local");
        assert!(!skills[0].shadowed);
        assert_eq!(skills[1].argosy, "imp");
        assert!(skills[1].shadowed);

        let winner = ctx.resolve_skill("deploy").unwrap().unwrap();
        assert_eq!(winner.argosy, "local", "local wins over imported");
    }

    #[test]
    fn collision_between_two_imports_is_decided_by_registration_order() {
        let local = make_argosy("local", &[]);
        let first = make_argosy("first", &[("skill/deploy.md", SKILL_DEPLOY)]);
        let second = make_argosy("second", &[("skill/deploy.md", SKILL_DEPLOY)]);
        let ctx = ProjectContext::open(
            local.path(),
            [first.path().to_path_buf(), second.path().to_path_buf()],
        )
        .unwrap();

        let winner = ctx.resolve_skill("deploy").unwrap().unwrap();
        assert_eq!(winner.argosy, "first");

        let skills = ctx.list_skills().unwrap();
        assert!(skills.iter().any(|s| s.argosy == "second" && s.shadowed));
        assert!(ctx.resolve_skill("nonexistent").unwrap().is_none());
    }

    // --- Rules across argosys ---

    #[test]
    fn list_rules_combines_across_argosys_and_tags_origins() {
        let local = make_argosy(
            "local",
            &[(
                "styleguide/rust/naming/local-rule.md",
                &rule_concept("Local naming rule"),
            )],
        );
        let imp = make_argosy(
            "imp",
            &[(
                "styleguide/rust/naming/imported-rule.md",
                &rule_concept("Imported naming rule"),
            )],
        );
        let ctx = ProjectContext::open(local.path(), [imp.path().to_path_buf()]).unwrap();

        let rules = ctx.list_rules(Some("rust"), Some("naming")).unwrap();
        assert_eq!(rules.len(), 2, "combine, not replace");
        assert_eq!(rules[0].argosy, "local");
        assert_eq!(rules[1].argosy, "imp");

        assert!(ctx.list_rules(Some("python"), None).unwrap().is_empty());
        assert_eq!(ctx.list_rules(None, None).unwrap().len(), 2);
    }

    // --- Read-only imports with memory/, STR-9 ---

    #[test]
    fn imported_argosy_containing_memory_is_tolerated_and_readable() {
        let local = make_argosy("local", &[]);
        let imp = make_argosy("imp", &[("memory/vendor-gotchas.md", "Their notes.\n")]);
        // Tolerated at open even though imports are read-only.
        let ctx = ProjectContext::open(local.path(), [imp.path().to_path_buf()]).unwrap();
        let concept = ctx.read_uri("argosy://imp/memory/vendor-gotchas").unwrap();
        assert!(concept.body().contains("Their notes"));

        let resolved = ctx
            .resolve(&qid("imp", Namespace::Memory, "memory/vendor-gotchas"))
            .unwrap();
        assert!(resolved.body().contains("Their notes"));
    }

    // --- Write through the local argosy only ---

    #[test]
    fn write_through_local_then_resolve_reads_it_back() {
        let local = make_argosy("local", &[]);
        let imp = make_argosy("imp", &[]);
        let ctx = ProjectContext::open(local.path(), [imp.path().to_path_buf()]).unwrap();

        let concept = Concept::from_str("---\ntype: Note\n---\nA learnings note.\n").unwrap();
        let id: ConceptId = "memory/new-note".parse().unwrap();
        ctx.local().write_memory(&id, &concept).unwrap();

        let read = ctx.read_uri("argosy://local/memory/new-note").unwrap();
        assert!(read.body().contains("A learnings note"));

        // Compile-time: `local()` is the only write-bearing accessor —
        // there is no `local_mut`/`imported_mut`, and `ArgosyRef` splits
        // local from imported so callers see which is which.
        let _: &LocalArgosy = ctx.local();
        assert!(matches!(
            ctx.argosy_named("local").unwrap(),
            ArgosyRef::Local(_)
        ));
        assert!(matches!(
            ctx.argosy_named("imp").unwrap(),
            ArgosyRef::Imported(_)
        ));
    }

    #[test]
    fn imported_memory_read_fails_cleanly_for_ghost_files() {
        let local = make_argosy("local", &[]);
        let imp = make_argosy("imp", &[("memory/x.md", "note\n")]);
        let ctx = ProjectContext::open(local.path(), [imp.path().to_path_buf()]).unwrap();
        let err = ctx
            .resolve(&qid("imp", Namespace::Memory, "memory/ghost"))
            .unwrap_err();
        assert!(matches!(err, Error::ConceptNotFound { .. }));
    }

    // --- Parser edge cases (strict, no silent normalization) ---

    #[test]
    fn uri_rejects_malformed_bodies_and_noncanonical_spellings() {
        for uri in [
            "argosy:///document/x",        // empty argosy name
            "argosy://acme/document/",     // trailing slash → empty id segment
            "argosy://acme/document//x",   // doubled separator → empty segment
            "argosy://acme//document/x",   // empty namespace segment → custom(""),
            "argosy://acme/document/../x", // `..` rejected by ConceptId, remapped to InvalidUri
            "argosy://acme/document/./x",  // `.` segment: rejected, not normalized away
            "argosy://acme/document/x.md", // `.md` suffix: rejected, not stripped
        ] {
            assert!(
                matches!(
                    QualifiedConceptId::from_uri(uri),
                    Err(Error::InvalidUri { .. })
                ),
                "expected InvalidUri for `{uri}`"
            );
        }
    }

    // --- resolve defenses ---

    #[test]
    fn resolve_refuses_an_id_outside_its_declared_namespace() {
        let local = make_argosy("local", &[("document/x.md", "content\n")]);
        let ctx = ProjectContext::open(local.path(), []).unwrap();
        // Built by hand (public fields), bypassing from_uri's guarantees.
        let err = ctx
            .resolve(&qid("local", Namespace::Memory, "document/x"))
            .unwrap_err();
        assert!(matches!(err, Error::Validation { .. }), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn resolve_never_follows_symlinks_so_reads_cannot_escape_the_bundle() {
        use std::os::unix::fs::symlink;

        let outside = TempDir::new().unwrap();
        write_file(outside.path(), "leak.md", "secret\n");
        let local = make_argosy("local", &[]);

        // Imported argosy whose entire `document/` is a symlink outside.
        let imp = make_argosy("imp", &[]);
        symlink(outside.path(), imp.path().join("document")).unwrap();
        let ctx = ProjectContext::open(local.path(), [imp.path().to_path_buf()]).unwrap();
        let err = ctx
            .resolve(&qid("imp", Namespace::Document, "document/leak"))
            .unwrap_err();
        assert!(
            matches!(err, Error::ConceptNotFound { .. }),
            "symlinked namespace must read as absent, got {err}"
        );

        // Imported argosy with a symlinked *file* pointing outside.
        let imp2 = make_argosy("imp2", &[]);
        fs::create_dir_all(imp2.path().join("document")).unwrap();
        symlink(
            outside.path().join("leak.md"),
            imp2.path().join("document/link.md"),
        )
        .unwrap();
        write_file(imp2.path(), "document/real.md", "real\n");
        let ctx2 = ProjectContext::open(local.path(), [imp2.path().to_path_buf()]).unwrap();
        let err = ctx2
            .resolve(&qid("imp2", Namespace::Document, "document/link"))
            .unwrap_err();
        assert!(
            matches!(err, Error::ConceptNotFound { .. }),
            "symlinked file must read as absent, got {err}"
        );
        // A genuine file next to the symlink still resolves.
        assert!(
            ctx2.resolve(&qid("imp2", Namespace::Document, "document/real"))
                .is_ok()
        );
    }
}
