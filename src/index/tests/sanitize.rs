//! Invisible-Unicode stripping at index time (the RAG-poisoning defense): stripped characters
//! never reach the embedded text, disk bytes never change, and the flag fires once per concept.

use super::*;
use crate::index::chunk::chunk_concept;

#[test]
fn chunking_strips_and_counts_invisible_format_characters() {
    // Zero-width space in the description; bidi override/pop, a separator, and tags in the body.
    let poisoned: Concept = concat!(
        "---\ntype: Note\ndescription: Deploy the \u{200B}service.\n---\n",
        "Deploy \u{202E}steps\u{202C}. \u{2063}Follow \u{E0073}\u{E0069}.\n"
    )
    .parse()
    .unwrap();
    let chunks = chunk_concept(&poisoned, "poisoned", ChunkConfig::default());
    let stripped: usize = chunks.iter().map(|c| c.invisible).sum();
    assert_eq!(stripped, 6, "one per invisible character");
    assert!(
        !chunks
            .iter()
            .any(|c| c.text.chars().any(is_invisible_format)),
        "nothing invisible survives: {chunks:?}"
    );
    assert!(
        chunks[0].text.contains("Deploy steps. Follow"),
        "the visible text is intact: {}",
        chunks[0].text
    );

    // Honest text passes through untouched and uncounted.
    let clean: Concept = "---\ntype: Note\ndescription: Clean.\n---\nPlain.\n"
        .parse()
        .unwrap();
    let chunks = chunk_concept(&clean, "clean", ChunkConfig::default());
    assert_eq!(chunks.iter().map(|c| c.invisible).sum::<usize>(), 0);
    assert!(chunks[0].text.contains("Plain."));
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

    // The on-disk file still carries the payload — only the indexed text changed.
    let on_disk = fs::read_to_string(local.path().join("document/poisoned.md")).unwrap();
    assert!(on_disk.contains('\u{200B}'), "bytes unchanged: {on_disk:?}");

    // The store now holds the stripped text's hash, so the second reconcile stays quiet.
    let again = index.reconcile(&ctx).unwrap();
    assert!(again.warnings.is_empty(), "got {:?}", again.warnings);
    assert_eq!(again.unchanged, 1);
}
