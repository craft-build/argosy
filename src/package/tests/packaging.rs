//! Tests for `package`, integrity validation, and content hashing.

use super::*;

#[test]
fn package_tar_gz_refuses_an_existing_destination() {
    // Regression: Directory mode refused non-empty destinations, but a
    // second tar.gz run silently clobbered the first artifact (and
    // errored on Windows instead). Both formats refuse now.
    let dir = TempDir::new().unwrap();
    let root = fixture_argosy(&dir);
    let dest = dir.path().join("bundle.tar.gz");
    let argosy = Argosy::open(&root).unwrap();
    let opts = PackageOptions {
        include_index: false,
        format: PackageFormat::TarGz,
    };
    package(&argosy, &dest, &opts).unwrap();
    let before = fs::read(&dest).unwrap();

    let err = package(&argosy, &dest, &opts).unwrap_err();
    assert!(
        err.to_string().contains("refusing to overwrite"),
        "unexpected error: {err}"
    );
    assert_eq!(fs::read(&dest).unwrap(), before, "the artifact is intact");
}

#[test]
fn import_of_a_directory_with_no_yaml_reports_zero_files_seen() {
    // A wrong path spelling must not look like a clean no-op success:
    // the report records that no YAML files were even considered.
    let dir = TempDir::new().unwrap();
    let local = import_fixture(&dir);
    let yaml_dir = dir.path().join("not-yaml");
    fs::create_dir_all(&yaml_dir).unwrap();
    fs::write(yaml_dir.join("rules.yaml.bak"), "id: x").unwrap();

    let report = import_styleguide_yaml(&local, &yaml_dir).unwrap();
    assert_eq!(report.yaml_files_seen, 0);
    assert_eq!(report.written, 0);
    assert!(report.findings.is_empty());
}

#[test]
fn package_excludes_root_memory_and_index_but_keeps_lookalikes() {
    let dir = TempDir::new().unwrap();
    let root = fixture_argosy(&dir);
    let dest = dir.path().join("out");
    let argosy = Argosy::open(&root).unwrap();
    let report = package(&argosy, &dest, &PackageOptions::default()).unwrap();

    assert_eq!(report.name, "acme-billing");
    assert_eq!(report.argosy_version.to_string(), "0.3.1");
    assert_eq!(report.files_copied, 4);
    assert!(report.memory_excluded);
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("memory/") && w.contains("excluded")),
        "DIST-4 warning missing: {:?}",
        report.warnings
    );

    assert!(dest.join("argosy.md").is_file());
    assert!(dest.join("document/design.md").is_file());
    assert!(dest.join("document/memory-notes/notes.md").is_file());
    assert!(dest.join("custom/product/faq.md").is_file());
    assert!(!dest.join("memory").exists());
    assert!(!dest.join(".argosy").exists());
}

#[test]
fn no_memory_means_no_warning() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("fixture");
    write(&root, "argosy.md", MANIFEST);
    let dest = dir.path().join("out");
    let argosy = Argosy::open(&root).unwrap();
    let report = package(&argosy, &dest, &PackageOptions::default()).unwrap();
    assert!(!report.memory_excluded);
    assert!(report.warnings.is_empty());
}

#[test]
fn include_index_ships_the_argosy_cache_dir() {
    let dir = TempDir::new().unwrap();
    let root = fixture_argosy(&dir);
    let dest = dir.path().join("out");
    let argosy = Argosy::open(&root).unwrap();
    let opts = PackageOptions {
        include_index: true,
        format: PackageFormat::Directory,
    };
    let report = package(&argosy, &dest, &opts).unwrap();
    assert!(dest.join(".argosy/index.db").is_file());
    assert!(!dest.join("memory").exists());
    assert_eq!(report.files_copied, 5);
}

#[cfg(feature = "default-index")]
#[test]
fn include_index_packages_a_checkpointed_snapshot_of_a_live_database() {
    use crate::index::VectorStore;
    use crate::index::sqlite::SqliteVecStore;

    let dir = TempDir::new().unwrap();
    let root = fixture_argosy(&dir);
    // A live writer holds the index open with committed-but-uncheckpointed
    // WAL content — the state an MCP server leaves the store in. (The
    // bundle fixture's placeholder file is not a real database; start
    // from a fresh one the store creates.)
    fs::remove_file(root.join(".argosy/index.db")).unwrap();
    let mut store = SqliteVecStore::open(root.join(".argosy/index.db")).unwrap();
    store.set_model_id("mock-embedder@1").unwrap();
    assert!(root.join(".argosy/index.db-wal").exists());

    let dest = dir.path().join("out");
    let argosy = Argosy::open(&root).unwrap();
    let opts = PackageOptions {
        include_index: true,
        format: PackageFormat::Directory,
    };
    package(&argosy, &dest, &opts).unwrap();
    drop(store);

    // The sidecars are excluded...
    assert!(dest.join(".argosy/index.db").is_file());
    assert!(!dest.join(".argosy/index.db-wal").exists());
    assert!(!dest.join(".argosy/index.db-shm").exists());
    //...because the main file alone carries the checkpointed state.
    let copied = SqliteVecStore::open_read_only(dest.join(".argosy/index.db")).unwrap();
    assert_eq!(copied.model_id(), Some("mock-embedder@1"));
    validate_integrity(&dest).unwrap();
}

#[test]
fn package_refuses_a_destination_inside_the_source() {
    let dir = TempDir::new().unwrap();
    let root = fixture_argosy(&dir);
    let argosy = Argosy::open(&root).unwrap();
    let err = package(&argosy, &root.join("out"), &PackageOptions::default()).unwrap_err();
    assert!(
        err.to_string().contains("inside the source bundle"),
        "unexpected error: {err}"
    );
    assert!(!root.join("out").exists(), "nothing was written");
}

#[test]
fn include_index_packages_validate_their_sidecar() {
    let dir = TempDir::new().unwrap();
    let root = fixture_argosy(&dir);
    let dest = dir.path().join("out");
    let argosy = Argosy::open(&root).unwrap();
    let opts = PackageOptions {
        include_index: true,
        format: PackageFormat::Directory,
    };
    package(&argosy, &dest, &opts).unwrap();
    validate_integrity(&dest).unwrap();

    fs::remove_file(dest.join(".argosy/index.db")).unwrap();
    let err = validate_integrity(&dest).unwrap_err();
    assert!(matches!(err, Error::IntegrityMismatch { .. }), "{err:?}");
}

#[test]
fn package_refuses_a_nonempty_destination() {
    let dir = TempDir::new().unwrap();
    let root = fixture_argosy(&dir);
    let dest = dir.path().join("out");
    fs::create_dir_all(&dest).unwrap();
    fs::write(dest.join("stale.txt"), "leftover").unwrap();
    let argosy = Argosy::open(&root).unwrap();
    let err = package(&argosy, &dest, &PackageOptions::default()).unwrap_err();
    assert!(
        err.to_string().contains("not an empty directory"),
        "unexpected error: {err}"
    );
}

#[test]
fn packaged_directory_is_itself_a_conformant_argosy() {
    let dir = TempDir::new().unwrap();
    let root = fixture_argosy(&dir);
    write(
        &root,
        "styleguide/rust/naming/snake-case-vars.md",
        "---\ntype: Styleguide Rule\ndescription: Name variables snake_case.\nlanguage: rust\ncategory: naming\n---\n\nGuidance.\n",
    );
    let dest = dir.path().join("out");
    let argosy = Argosy::open(&root).unwrap();
    package(&argosy, &dest, &PackageOptions::default()).unwrap();

    let packaged = Argosy::open(&dest).unwrap();
    assert!(Argosy::validate(&dest).is_conformant());
    assert_eq!(error_findings(&packaged).len(), 0);
}

#[test]
fn integrity_sidecar_matches_recomputation() {
    let dir = TempDir::new().unwrap();
    let root = fixture_argosy(&dir);
    let dest = dir.path().join("out");
    let argosy = Argosy::open(&root).unwrap();
    package(&argosy, &dest, &PackageOptions::default()).unwrap();

    let sidecar = fs::read_to_string(dest.join(INTEGRITY_FILENAME)).unwrap();
    let lines: Vec<&str> = sidecar.lines().collect();
    assert_eq!(lines.len(), 4);
    assert!(
        lines[0].ends_with("  argosy.md"),
        "manifest first: {lines:?}"
    );
    // Spot-check one file against an independent recomputation.
    let (hash, rel) = lines[1].split_once("  ").unwrap();
    let bytes = fs::read(dest.join(rel)).unwrap();
    assert_eq!(hash, sha256_hex(&bytes));

    validate_integrity(&dest).unwrap();
}

#[test]
fn validate_integrity_flags_tampering() {
    let dir = TempDir::new().unwrap();
    let root = fixture_argosy(&dir);
    let dest = dir.path().join("out");
    let argosy = Argosy::open(&root).unwrap();
    package(&argosy, &dest, &PackageOptions::default()).unwrap();

    fs::write(dest.join("document/design.md"), "tampered").unwrap();
    let err = validate_integrity(&dest).unwrap_err();
    assert!(matches!(err, Error::IntegrityMismatch { .. }), "{err:?}");
}

#[test]
fn validate_integrity_flags_a_missing_file() {
    let dir = TempDir::new().unwrap();
    let root = fixture_argosy(&dir);
    let dest = dir.path().join("out");
    let argosy = Argosy::open(&root).unwrap();
    package(&argosy, &dest, &PackageOptions::default()).unwrap();

    fs::remove_file(dest.join("document/design.md")).unwrap();
    let err = validate_integrity(&dest).unwrap_err();
    assert!(matches!(err, Error::IntegrityMismatch { .. }), "{err:?}");
    assert!(err.to_string().contains("missing"), "{err:?}");
}

#[test]
fn validate_integrity_rejects_escaping_sidecar_paths() {
    let dir = TempDir::new().unwrap();
    let root = fixture_argosy(&dir);
    let dest = dir.path().join("out");
    let argosy = Argosy::open(&root).unwrap();
    package(&argosy, &dest, &PackageOptions::default()).unwrap();

    // An attacker-controlled sidecar must not make the validator hash
    // files outside the bundle (containment).
    fs::write(dest.join(INTEGRITY_FILENAME), "deadbeef  ../outside.txt\n").unwrap();
    let err = validate_integrity(&dest).unwrap_err();
    assert!(
        err.to_string().contains("escapes the bundle root"),
        "{err:?}"
    );

    // An empty path is equally malformed and must not surface as Io.
    fs::write(dest.join(INTEGRITY_FILENAME), "deadbeef  \n").unwrap();
    let err = validate_integrity(&dest).unwrap_err();
    assert!(matches!(err, Error::IntegrityMismatch { .. }), "{err:?}");
}

#[cfg(unix)]
#[test]
fn validate_integrity_refuses_symlinked_dirs_pointing_outside() {
    let dir = TempDir::new().unwrap();
    let root = fixture_argosy(&dir);
    let dest = dir.path().join("out");
    let argosy = Argosy::open(&root).unwrap();
    package(&argosy, &dest, &PackageOptions::default()).unwrap();

    // Every component is a Normal name, so the textual check passes — it
    // is the canonicalize in `read_bundle_file` that must catch the
    // symlinked intermediate directory.
    let outside = dir.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("secret.txt"), "do not read").unwrap();
    std::os::unix::fs::symlink(&outside, dest.join("linkdir")).unwrap();
    fs::write(
        dest.join(INTEGRITY_FILENAME),
        "deadbeef  linkdir/secret.txt\n",
    )
    .unwrap();

    let err = validate_integrity(&dest).unwrap_err();
    assert!(matches!(err, Error::SymlinkEscape { .. }), "{err:?}");
}

#[test]
fn bundle_content_hash_changes_with_content_and_is_stable_across_a_copy() {
    let dir = TempDir::new().unwrap();
    let root = fixture_argosy(&dir);
    let before = bundle_content_hash(&root).unwrap();

    fs::write(
        root.join("document/design.md"),
        "---\ntype: Document\ndescription: Design notes.\n---\n\n# Design v2\n",
    )
    .unwrap();
    let after = bundle_content_hash(&root).unwrap();
    assert_ne!(before, after);

    // Packaging excludes memory/ and.argosy/, so a packaged copy hashes
    // the same as its source — the hash covers distributable content.
    let dest = dir.path().join("out");
    let argosy = Argosy::open(&root).unwrap();
    package(&argosy, &dest, &PackageOptions::default()).unwrap();
    assert_eq!(bundle_content_hash(&dest).unwrap(), after);
}

#[test]
fn bundle_content_hash_rejects_a_non_bundle() {
    let dir = TempDir::new().unwrap();
    let err = bundle_content_hash(dir.path()).unwrap_err();
    assert!(matches!(err, Error::NotAnArgosy { .. }), "{err:?}");
}

#[cfg(unix)]
#[test]
fn symlink_escaping_the_bundle_errors() {
    let dir = TempDir::new().unwrap();
    let root = fixture_argosy(&dir);
    let outside = dir.path().join("secret.txt");
    fs::write(&outside, "do not ship").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("document/leak.md")).unwrap();

    let dest = dir.path().join("out");
    let argosy = Argosy::open(&root).unwrap();
    let err = package(&argosy, &dest, &PackageOptions::default()).unwrap_err();
    assert!(matches!(err, Error::SymlinkEscape { .. }), "{err:?}");
}

#[cfg(unix)]
#[test]
fn symlink_aliasing_memory_or_the_index_is_rejected() {
    let dir = TempDir::new().unwrap();
    let root = fixture_argosy(&dir);
    std::os::unix::fs::symlink("../memory/gotchas.md", root.join("document/alias.md")).unwrap();

    let dest = dir.path().join("out");
    let argosy = Argosy::open(&root).unwrap();
    let err = package(&argosy, &dest, &PackageOptions::default()).unwrap_err();
    assert!(matches!(err, Error::SymlinkEscape { .. }), "{err:?}");

    fs::remove_file(root.join("document/alias.md")).unwrap();
    std::os::unix::fs::symlink("../.argosy/index.db", root.join("document/idx.md")).unwrap();
    let opts = PackageOptions {
        include_index: false,
        format: PackageFormat::Directory,
    };
    let err = package(&argosy, &dest, &opts).unwrap_err();
    assert!(matches!(err, Error::SymlinkEscape { .. }), "{err:?}");

    // include_index ships the cache, so aliasing into it is legitimate.
    let opts = PackageOptions {
        include_index: true,
        format: PackageFormat::Directory,
    };
    package(&argosy, &dir.path().join("out2"), &opts).unwrap();
}

#[cfg(unix)]
#[test]
fn in_bundle_symlink_is_materialized_as_contents() {
    let dir = TempDir::new().unwrap();
    let root = fixture_argosy(&dir);
    std::os::unix::fs::symlink("../argosy.md", root.join("document/manifest-copy.md")).unwrap();

    let dest = dir.path().join("out");
    let argosy = Argosy::open(&root).unwrap();
    package(&argosy, &dest, &PackageOptions::default()).unwrap();

    let copied = dest.join("document/manifest-copy.md");
    assert!(copied.is_file());
    assert!(!copied.symlink_metadata().unwrap().file_type().is_symlink());
    assert_eq!(
        fs::read(&copied).unwrap(),
        fs::read(dest.join("argosy.md")).unwrap()
    );
}

#[test]
fn targz_round_trips_into_a_conformant_argosy() {
    let dir = TempDir::new().unwrap();
    let root = fixture_argosy(&dir);
    let archive = dir.path().join("out.tar.gz");
    let argosy = Argosy::open(&root).unwrap();
    let opts = PackageOptions {
        include_index: false,
        format: PackageFormat::TarGz,
    };
    package(&argosy, &archive, &opts).unwrap();
    assert!(archive.is_file());

    let extracted = dir.path().join("extracted");
    fs::create_dir_all(&extracted).unwrap();
    let decoder = flate2::read::GzDecoder::new(fs::File::open(&archive).unwrap());
    tar::Archive::new(decoder).unpack(&extracted).unwrap();

    assert!(!extracted.join("memory").exists());
    assert!(extracted.join(INTEGRITY_FILENAME).is_file());
    Argosy::open(&extracted).unwrap();
    assert!(Argosy::validate(&extracted).is_conformant());
    validate_integrity(&extracted).unwrap();
}
