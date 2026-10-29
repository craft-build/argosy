//! Unit tests for project context: discovery, URIs, resolution, skills.

use super::*;

use std::fs;

use tempfile::TempDir;

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

/// A project plus its state layout: checkouts under
/// `<state>/projects/<slug>` (the local one named `default`), globals
/// under `<state>/global`.
fn project_layout(names: &[&str], globals: &[&str]) -> (TempDir, TempDir) {
    let project = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let project_dir = crate::pull::project_argosy_dir_at(state.path(), project.path());
    for name in names {
        let manifest = format!(
            "---\ntype: Argosy Manifest\nname: proj-{name}\nargosy_version: \"1.0.0\"\n---\n# proj-{name}\n"
        );
        write_file(&project_dir.join(name), "argosy.md", &manifest);
    }
    for name in globals {
        let manifest = format!(
            "---\ntype: Argosy Manifest\nname: global-{name}\nargosy_version: \"1.0.0\"\n---\n# global-{name}\n"
        );
        write_file(
            &state.path().join("global").join(name),
            "argosy.md",
            &manifest,
        );
    }
    (project, state)
}

#[test]
fn open_project_loads_default_children_and_globals_in_precedence_order() {
    let (project, state) = project_layout(&["default", "beta", "alpha"], &["extra"]);
    // Non-checkout noise that discovery must ignore.
    let project_dir = crate::pull::project_argosy_dir_at(state.path(), project.path());
    write_file(&project_dir, "index.db", "not a directory");
    write_file(&project_dir.join("notado"), "argosy.notmd", "x");

    let ctx = ProjectContext::open_project_with_state(project.path(), state.path()).unwrap();

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
    let (project, state) = project_layout(&["beta"], &[]);
    let err = ProjectContext::open_project_with_state(project.path(), state.path()).unwrap_err();
    assert!(
        err.to_string().contains("argosy init") && err.to_string().contains("default"),
        "unexpected error: {err}"
    );
}

#[test]
fn open_project_rejects_duplicate_manifest_names_across_tiers() {
    let (project, state) = project_layout(&["default"], &[]);
    write_file(
        &state.path().join("global/rogue"),
        "argosy.md",
        "---\ntype: Argosy Manifest\nname: proj-default\nargosy_version: \"1.0.0\"\n---\n# rogue\n",
    );
    let err = ProjectContext::open_project_with_state(project.path(), state.path()).unwrap_err();
    assert!(err.to_string().contains("proj-default"), "{err}");
}

#[test]
fn same_named_projects_get_isolated_state_dirs() {
    // The slug hashes the absolute path, so two `craft` dirs never collide.
    let state = TempDir::new().unwrap();
    let mut guards = Vec::new();
    let mut locals = Vec::new();
    for parent in ["one", "two"] {
        let project = TempDir::new().unwrap();
        let dir = project.path().join(parent).join("craft");
        fs::create_dir_all(&dir).unwrap();
        write_file(
            &crate::pull::project_argosy_dir_at(state.path(), &dir).join("default"),
            "argosy.md",
            &format!(
                "---\ntype: Argosy Manifest\nname: craft-{parent}\nargosy_version: \"1.0.0\"\n---\n# craft\n"
            ),
        );
        locals.push((dir, format!("craft-{parent}")));
        guards.push(project);
    }
    for (dir, name) in &locals {
        let ctx = ProjectContext::open_project_with_state(dir, state.path()).unwrap();
        assert_eq!(ctx.local().manifest().name(), name);
    }
    let slugs: Vec<String> = locals
        .iter()
        .map(|(dir, _)| crate::pull::project_slug(dir))
        .collect();
    assert_ne!(slugs[0], slugs[1], "same-named projects collide: {slugs:?}");
    for slug in &slugs {
        assert!(state.path().join("projects").join(slug).is_dir());
    }
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

// --- Integrity verification on open ---

/// Packages `source` (a fixture argosy) into a fresh directory copy that
/// carries a valid integrity sidecar — the shape a shared book cloned from
/// a packaged repo has.
fn packaged_copy(source: &Path) -> TempDir {
    let dest = TempDir::new().unwrap();
    let staged = dest.path().join("pkg");
    crate::package::package(
        &crate::bundle::Argosy::open(source).unwrap(),
        &staged,
        &crate::package::PackageOptions::default(),
    )
    .unwrap();
    let out = dest.path().join("book");
    fs::rename(&staged, &out).unwrap();
    dest
}

#[test]
fn an_import_with_a_valid_sidecar_opens_normally() {
    let local = make_argosy("local", &[]);
    let imp = make_argosy("imp", &[("document/guide.md", "Guide.\n")]);
    let pkg = packaged_copy(imp.path());

    let ctx = ProjectContext::open(local.path(), [pkg.path().join("book")]).unwrap();
    assert_eq!(ctx.imported().count(), 1);
    assert!(ctx.excluded_imports().is_empty());
    assert!(
        ctx.read_uri("argosy://imp/document/guide")
            .unwrap()
            .body()
            .contains("Guide")
    );
}

#[test]
fn an_import_with_git_metadata_reports_its_head_commit() {
    let local = make_argosy("local", &[]);
    let imp = make_argosy("imp", &[]);
    // Hand-rolled git metadata like a clone: HEAD at a loose branch ref.
    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    let git = imp.path().join(".git");
    fs::create_dir_all(git.join("refs/heads")).unwrap();
    fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    fs::write(git.join("refs/heads/main"), format!("{COMMIT}\n")).unwrap();

    let ctx = ProjectContext::open(local.path(), [imp.path().to_path_buf()]).unwrap();
    assert_eq!(ctx.import_commit("imp"), Some(COMMIT));
    assert_eq!(ctx.import_commit("local"), None);
    assert_eq!(ctx.import_commit("nope"), None);
    let plain = make_argosy("plain", &[]);
    let ctx = ProjectContext::open(local.path(), [plain.path().to_path_buf()]).unwrap();
    assert_eq!(ctx.import_commit("plain"), None);
}

#[test]
fn an_import_tampered_after_packaging_is_excluded_not_served() {
    let local = make_argosy("local", &[]);
    let imp = make_argosy("imp", &[("document/guide.md", "Guide.\n")]);
    let pkg = packaged_copy(imp.path());
    write_file(&pkg.path().join("book"), "document/guide.md", "Injected.\n");

    let ctx = ProjectContext::open(local.path(), [pkg.path().join("book")]).unwrap();
    assert_eq!(ctx.imported().count(), 0, "tampered import never serves");
    let excluded = ctx.excluded_imports();
    assert_eq!(excluded.len(), 1);
    assert_eq!(excluded[0].name, "book", "named by its checkout directory");
    assert!(
        excluded[0].reason.contains("integrity"),
        "reason carries the error chain: {}",
        excluded[0].reason
    );
    let err = ctx
        .resolve(&qid("imp", Namespace::Document, "document/guide"))
        .unwrap_err();
    assert!(matches!(err, Error::UnknownArgosy { .. }), "got {err:?}");
}

#[test]
fn a_tampered_import_does_not_take_healthy_imports_down_with_it() {
    let local = make_argosy("local", &[]);
    let healthy = make_argosy("healthy", &[("document/ok.md", "Fine.\n")]);
    let pkg = packaged_copy(healthy.path());
    let imp = make_argosy("imp", &[("document/guide.md", "Guide.\n")]);
    let tampered = packaged_copy(imp.path());
    write_file(
        &tampered.path().join("book"),
        "document/guide.md",
        "Injected.\n",
    );

    let ctx = ProjectContext::open(
        local.path(),
        [tampered.path().join("book"), pkg.path().join("book")],
    )
    .unwrap();
    let names: Vec<&str> = ctx.imported().map(|a| a.manifest().name()).collect();
    assert_eq!(names, vec!["healthy"]);
    assert_eq!(ctx.excluded_imports().len(), 1);
}

#[test]
fn an_import_without_a_sidecar_needs_no_verification() {
    // Nothing was ever packaged: no sidecar, so trust stays a config question.
    let local = make_argosy("local", &[]);
    let imp = make_argosy("imp", &[("document/guide.md", "Guide.\n")]);
    let ctx = ProjectContext::open(local.path(), [imp.path().to_path_buf()]).unwrap();
    assert_eq!(ctx.imported().count(), 1);
    assert!(ctx.excluded_imports().is_empty());
}

#[test]
fn open_project_with_state_reports_tampered_checkouts_as_excluded() {
    // Discovery must surface exclusions like the explicit-path route does.
    let (project, state) = project_layout(&["default"], &[]);
    let imp = make_argosy("imp", &[("document/guide.md", "Guide.\n")]);
    let pkg = packaged_copy(imp.path());
    let project_dir = crate::pull::project_argosy_dir_at(state.path(), project.path());
    fs::rename(pkg.path().join("book"), project_dir.join("shared")).unwrap();
    write_file(
        &project_dir.join("shared"),
        "document/guide.md",
        "Injected.\n",
    );

    let ctx = ProjectContext::open_project_with_state(project.path(), state.path()).unwrap();
    assert_eq!(ctx.imported().count(), 0);
    assert_eq!(ctx.excluded_imports().len(), 1);
    assert_eq!(ctx.excluded_imports()[0].name, "shared");
}

// --- Pull-time stamps: tampering after pull fails closed ---

/// Stamps `checkout` the way `argosy pull` would have: the SHA-256 of its
/// current sidecar, or `absent` when it ships none.
fn stamp(checkout: &Path) {
    let sidecar = checkout.join(crate::package::INTEGRITY_FILENAME);
    let body = if std::fs::symlink_metadata(&sidecar).is_ok_and(|m| m.file_type().is_file()) {
        crate::hash::sha256_hex(&fs::read(&sidecar).unwrap())
    } else {
        "absent".to_string()
    };
    fs::write(crate::pull::import_integrity_stamp(checkout), body).unwrap();
}

/// Repackages the argosy at `source` over `dest` (removed first) as a
/// fully self-consistent tree — the shape of an upstream rewrite.
///
/// The pull-time stamp lives *beside* `dest` and survives.
fn repackage_onto(source: &Path, dest: &Path) {
    fs::remove_dir_all(dest).unwrap();
    crate::package::package(
        &crate::bundle::Argosy::open(source).unwrap(),
        dest,
        &crate::package::PackageOptions::default(),
    )
    .unwrap();
}

#[test]
fn deleting_the_sidecar_after_a_stamped_pull_excludes_the_import() {
    // The regression the stamp exists for: without it, a vanished sidecar
    // made the book look never-packaged and it kept serving.
    let local = make_argosy("local", &[]);
    let imp = make_argosy("imp", &[("document/guide.md", "Guide.\n")]);
    let pkg = packaged_copy(imp.path());
    let book = pkg.path().join("book");
    stamp(&book);
    // "Malware": rewrite content *and* delete the sidecar.
    write_file(&book, "document/guide.md", "Injected.\n");
    fs::remove_file(book.join(crate::package::INTEGRITY_FILENAME)).unwrap();

    let ctx = ProjectContext::open(local.path(), [book]).unwrap();
    assert_eq!(ctx.imported().count(), 0, "tampered import never serves");
    let reason = &ctx.excluded_imports()[0].reason;
    assert!(
        reason.contains("sidecar is missing") && reason.contains("argosy pull"),
        "actionable reason, got {reason}"
    );
}

#[test]
fn rewriting_the_sidecar_after_a_stamped_pull_excludes_the_import() {
    // Even a *self-consistent* replacement sidecar fails: the stamp
    // recorded what was verified at pull time.
    let local = make_argosy("local", &[]);
    let imp = make_argosy("imp", &[("document/guide.md", "Guide.\n")]);
    let tampered = make_argosy("imp", &[("document/guide.md", "Injected.\n")]);
    let pkg = packaged_copy(imp.path());
    let book = pkg.path().join("book");
    stamp(&book);
    repackage_onto(tampered.path(), &book);

    let ctx = ProjectContext::open(local.path(), [book]).unwrap();
    assert_eq!(ctx.imported().count(), 0);
    let reason = &ctx.excluded_imports()[0].reason;
    assert!(
        reason.contains("sidecar changed"),
        "stamp mismatch named, got {reason}"
    );
}

#[test]
fn a_sidecar_appearing_on_an_unstamped_book_excludes_the_import() {
    let local = make_argosy("local", &[]);
    let imp = make_argosy("imp", &[("document/guide.md", "Guide.\n")]);
    let pkg = packaged_copy(imp.path());
    let book = pkg.path().join("book");
    fs::remove_file(book.join(crate::package::INTEGRITY_FILENAME)).unwrap();
    stamp(&book); // stamps `absent`
    repackage_onto(imp.path(), &book); // now a (consistent) sidecar exists

    let ctx = ProjectContext::open(local.path(), [book]).unwrap();
    assert_eq!(ctx.imported().count(), 0);
    assert!(
        ctx.excluded_imports()[0]
            .reason
            .contains("sidecar appeared"),
        "got {}",
        ctx.excluded_imports()[0].reason
    );
}

#[test]
fn a_stamped_unchanged_checkout_serves_normally() {
    let local = make_argosy("local", &[]);
    let imp = make_argosy("imp", &[("document/guide.md", "Guide.\n")]);
    let pkg = packaged_copy(imp.path());
    let book = pkg.path().join("book");
    stamp(&book);

    let ctx = ProjectContext::open(local.path(), [book]).unwrap();
    assert_eq!(ctx.imported().count(), 1);
    assert!(ctx.excluded_imports().is_empty());
    assert!(
        ctx.read_uri("argosy://imp/document/guide")
            .unwrap()
            .body()
            .contains("Guide")
    );
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

// --- Read-only imports with memory/ ---

#[test]
fn imported_argosy_containing_memory_is_tolerated_and_readable() {
    let local = make_argosy("local", &[]);
    let imp = make_argosy("imp", &[("memory/vendor-gotchas.md", "Their notes.\n")]);
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
    // there is no `local_mut`/`imported_mut`.
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
    assert!(
        ctx2.resolve(&qid("imp2", Namespace::Document, "document/real"))
            .is_ok()
    );
}
