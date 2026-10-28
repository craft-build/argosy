//! Invisible-Unicode stripping at index time (the RAG-poisoning defense):
//! zero-width/bidi/tag characters never reach the embedded text, the bytes
//! on disk never change, and the flag fires once — when the concept is
//! embedded, not on every reconcile.

use super::*;

#[test]
fn index_text_strips_and_counts_invisible_format_characters() {
    // Zero-width space in the description; bidi override + pop, an
    // invisible separator, and tag-spelled ASCII in the body.
    let poisoned: Concept = concat!(
        "---\ntype: Note\ndescription: Deploy the \u{200B}service.\n---\n",
        "Deploy \u{202E}steps\u{202C}. \u{2063}Follow \u{E0073}\u{E0069}.\n"
    )
    .parse()
    .unwrap();
    let (text, stripped) = index_text(&poisoned);
    assert_eq!(stripped, 6, "one per invisible character");
    assert!(
        !text.chars().any(is_invisible_format),
        "nothing invisible survives: {text:?}"
    );
    assert!(
        text.contains("Deploy steps. Follow"),
        "the visible text is intact: {text:?}"
    );

    // Honest text passes through untouched and uncounted.
    let clean: Concept = "---\ntype: Note\ndescription: Clean.\n---\nPlain.\n"
        .parse()
        .unwrap();
    let (text, stripped) = index_text(&clean);
    assert_eq!(stripped, 0);
    assert!(text.contains("Plain."));
}

#[test]
fn reconcile_flags_stripped_concepts_once_and_never_touches_disk() {
    let local = make_argosy(
        "local",
        &[(
            "document/poisoned.md",
            concat!(
                "---\ntype: Note\ndescription: The \u{200B}architecture.\n---\n",
                "The service \u{2063}architecture.\n"
            ),
        )],
    );
    let ctx = ProjectContext::open(local.path(), Vec::<std::path::PathBuf>::new()).unwrap();
    let mut index = Index::new(MockEmbedder::new(), MemStore::new());

    let report = index.reconcile(&ctx).unwrap();
    assert_eq!(report.warnings.len(), 1, "one line per poisoned concept");
    let warning = &report.warnings[0];
    assert!(
        warning.starts_with("argosy://local/document/poisoned: stripped 2 invisible"),
        "names the concept and the count: {warning}"
    );
    assert!(
        warning.contains("the on-disk file is unchanged"),
        "says what stripping does not touch: {warning}"
    );

    // The file on disk still carries the payload verbatim — it stays
    // inspectable (and sidecar-verifiable); only the indexed text changed.
    let on_disk = fs::read_to_string(local.path().join("document/poisoned.md")).unwrap();
    assert!(on_disk.contains('\u{200B}'), "bytes unchanged: {on_disk:?}");

    // A second reconcile is quiet: the store now holds the stripped
    // text's hash, so an unchanged poisoned book does not re-warn.
    let again = index.reconcile(&ctx).unwrap();
    assert!(again.warnings.is_empty(), "got {:?}", again.warnings);
    assert_eq!(again.unchanged, 1);
}
