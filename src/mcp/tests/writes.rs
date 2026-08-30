//! Write tools: memory, rule, and document mutations and their index
//! reconciliation.

use super::*;

#[test]
fn write_and_read_memory_round_trip() {
    let mut rig = rig();
    let content = "---\ntype: Session Note\ndescription: learned\n---\n# N\n\nBody.\n";
    let out = rig
        .state
        .write_memory(WriteParams {
            cwd: project(),
            path: "memory/rust-internals".to_string(),
            content: content.to_string(),
        })
        .unwrap();
    assert_eq!(out.uri, "argosy://acme-billing/memory/rust-internals");
    assert_eq!(out.action, "created");
    assert_eq!(out.bytes, Some(content.len() as u64));
    assert!(out.indexed, "write reconciles the index: {out:?}");
    assert!(out.index_error.is_none());

    let read = rig
        .state
        .read_memory(ReadPathParams {
            cwd: project(),
            path: "memory/rust-internals".to_string(),
        })
        .unwrap();
    assert!(read.content.contains("# N"));

    // Writes land in the local argosy on disk.
    let local_root = rig
        .state
        .session(project())
        .unwrap()
        .context
        .local()
        .root()
        .to_path_buf();
    assert!(local_root.join("memory/rust-internals.md").is_file());
}

/// A memory write without a usable frontmatter `type` is auto-filled as
/// `type: Memory` instead of a MEM-1 rejection — what round-trips carries
/// the filled type.
#[test]
fn write_memory_auto_fills_missing_type() {
    let mut rig = rig();
    let out = rig
        .state
        .write_memory(WriteParams {
            cwd: project(),
            path: "memory/untyped-note".to_string(),
            content: "# Just prose\n\nNo frontmatter at all.\n".to_string(),
        })
        .unwrap();
    assert_eq!(out.action, "created");

    let read = rig
        .state
        .read_memory(ReadPathParams {
            cwd: project(),
            path: "memory/untyped-note".to_string(),
        })
        .unwrap();
    assert!(
        read.content.starts_with("---\ntype: Memory\n"),
        "auto-filled frontmatter, got {}",
        read.content
    );
    // `bytes` reports what landed on disk (the serialized concept with
    // the auto-filled type), not the submitted input's length.
    assert_eq!(
        out.bytes,
        Some(read.content.len() as u64),
        "bytes matches the on-disk concept"
    );
}

/// The staleness regression: a concept written through the MCP surface
/// is findable via search in the SAME session, no restart. A deleted
/// one disappears immediately.
#[test]
fn write_then_search_and_delete_then_search_are_immediately_visible() {
    let mut rig = rig();
    let content = "---\ntype: Session Note\ndescription: Zinc whisker relay failures.\n---\n\
         Zinc whiskers bridge relays after humid summers.\n";
    let out = rig
        .state
        .write_memory(WriteParams {
            cwd: project(),
            path: "memory/zinc-whiskers".to_string(),
            content: content.to_string(),
        })
        .unwrap();
    assert!(out.indexed, "the write reconciled the index");

    let report = rig
        .state
        .search(SearchParams {
            cwd: project(),
            query: "zinc whisker relay failures".to_string(),
            k: Some(10),
            namespaces: Some(vec!["memory".to_string()]),
            argosy: None,
            tags: None,
            r#type: None,
            language: None,
            category: None,
        })
        .unwrap();
    assert!(
        report
            .hits
            .iter()
            .any(|h| h.uri.ends_with("memory/zinc-whiskers")),
        "the fresh write is searchable now, got {:?}",
        report.hits
    );

    let out = rig
        .state
        .delete_memory(ReadPathParams {
            cwd: project(),
            path: "memory/zinc-whiskers".to_string(),
        })
        .unwrap();
    assert!(out.indexed);
    let report = rig
        .state
        .search(SearchParams {
            cwd: project(),
            query: "zinc whisker relay failures".to_string(),
            k: Some(10),
            namespaces: Some(vec!["memory".to_string()]),
            argosy: None,
            tags: None,
            r#type: None,
            language: None,
            category: None,
        })
        .unwrap();
    assert!(
        report
            .hits
            .iter()
            .all(|h| !h.uri.ends_with("memory/zinc-whiskers")),
        "the deletion is reflected now, got {:?}",
        report.hits
    );
}

/// An update must say `updated` — silent destruction is never silent.
#[test]
fn rewriting_an_existing_concept_reports_updated_not_created() {
    let mut rig = rig();
    let first = "---\ntype: Session Note\ndescription: one\n---\nOne.\n";
    let out = rig
        .state
        .write_memory(WriteParams {
            cwd: project(),
            path: "memory/overwrite-me".to_string(),
            content: first.to_string(),
        })
        .unwrap();
    assert_eq!(out.action, "created");

    let second = "---\ntype: Session Note\ndescription: two\n---\nTwo.\n";
    let out = rig
        .state
        .write_memory(WriteParams {
            cwd: project(),
            path: "memory/overwrite-me".to_string(),
            content: second.to_string(),
        })
        .unwrap();
    assert_eq!(out.action, "updated", "the prior version existed");
    assert!(out.indexed);
}

/// The degraded path (M1): when the embedding model is unavailable, a
/// write still succeeds on disk and the report says `indexed: false`
/// with an actionable error.
#[test]
fn write_with_a_failing_embedder_still_writes_and_reports_not_indexed() {
    use crate::index::Index;
    use crate::index::tests::MemStore;

    /// A provider whose embed always fails — the "model unavailable"
    /// double.
    struct FailingEmbedder;

    impl crate::index::EmbeddingProvider for FailingEmbedder {
        fn model_id(&self) -> &str {
            "failing@1"
        }
        fn dimensions(&self) -> usize {
            8
        }
        fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, crate::error::Error> {
            Err(Error::Index {
                reason: "embedding model unavailable (test double)".to_string(),
            })
        }
    }

    let local = fixture_copy("valid-acme-billing");
    let local_root = local.path().to_path_buf();
    let mut state: McpState<FailingEmbedder, MemStore> = McpState::new(Arc::new(move |_root| {
        let context = ProjectContext::open(&local_root, [])?;
        Ok(ProjectSession::new(
            context,
            Index::new(FailingEmbedder, MemStore::new()),
        ))
    }));

    let content = "---\ntype: Session Note\ndescription: offline write.\n---\nBody.\n";
    let out = state
        .write_memory(WriteParams {
            cwd: project(),
            path: "memory/offline-write".to_string(),
            content: content.to_string(),
        })
        .unwrap();

    assert_eq!(out.action, "created");
    assert!(!out.indexed, "reconcile could not embed");
    let note = out.index_error.expect("the failure is explained");
    assert!(note.contains("not yet indexed"), "got {note}");
    // The write itself stands on disk.
    assert!(local.path().join("memory/offline-write.md").is_file());
}

#[test]
fn write_memory_rejects_reserved_and_escape_paths() {
    let mut rig = rig();
    rig.state
        .write_memory(WriteParams {
            cwd: project(),
            path: "../escape".to_string(),
            content: "x".to_string(),
        })
        .unwrap_err();
    rig.state
        .write_memory(WriteParams {
            cwd: project(),
            path: "memory/index".to_string(),
            content: "x".to_string(),
        })
        .unwrap_err(); // index.md is a reserved filename
    rig.state
        .write_memory(WriteParams {
            cwd: project(),
            path: "memory/malformed".to_string(),
            content: "---\ntype: [oops\n---\nx".to_string(),
        })
        .unwrap_err();
}

#[test]
fn delete_memory_removes_the_concept() {
    let mut rig = rig();
    let out = rig
        .state
        .delete_memory(ReadPathParams {
            cwd: project(),
            path: "memory/gotchas".to_string(),
        })
        .unwrap();
    assert_eq!(out.action, "deleted");
    assert!(out.indexed);
    rig.state
        .read_memory(ReadPathParams {
            cwd: project(),
            path: "memory/gotchas".to_string(),
        })
        .unwrap_err();
    rig.state
        .delete_memory(ReadPathParams {
            cwd: project(),
            path: "memory/gotchas".to_string(),
        })
        .unwrap_err();
}

#[test]
fn write_and_delete_rule_with_contract_checks() {
    let mut rig = rig();
    let rule = "---\n\
         type: Styleguide Rule\n\
         description: Prefer sleep over polling.\n\
         language: rust\n\
         category: async\n\
         ---\n\
         ## Good\n\nawait.\n";
    let out = rig
        .state
        .write_rule(WriteParams {
            cwd: project(),
            path: "styleguide/rust/async/no-polling".to_string(),
            content: rule.to_string(),
        })
        .unwrap();
    assert_eq!(out.action, "created");
    assert!(out.indexed);

    // STG-3: a rule without a description is refused by the library.
    let err = rig
        .state
        .write_rule(WriteParams {
            cwd: project(),
            path: "styleguide/rust/async/no-retries".to_string(),
            content: "---\ntype: Styleguide Rule\n---\n# X\n".to_string(),
        })
        .unwrap_err();
    assert!(
        matches!(err, Error::NamespaceContractViolation { .. }),
        "got {err:?}"
    );

    let out = rig
        .state
        .delete_rule(ReadPathParams {
            cwd: project(),
            path: "styleguide/rust/async/no-polling".to_string(),
        })
        .unwrap();
    assert_eq!(out.action, "deleted");
    assert!(out.indexed);
}

#[test]
fn write_and_delete_document_round_trip() {
    let mut rig = rig();
    let content =
        "---\ntype: Decision\ndescription: Cache responses.\n---\n# Decision\n\nWe cache.\n";
    let out = rig
        .state
        .write_document(WriteParams {
            cwd: project(),
            path: "document/decisions/2026-08-caching".to_string(),
            content: content.to_string(),
        })
        .unwrap();
    assert_eq!(
        out.uri,
        "argosy://acme-billing/document/decisions/2026-08-caching"
    );
    assert_eq!(out.action, "created");
    assert_eq!(out.bytes, Some(content.len() as u64));
    assert!(out.indexed, "write reconciles the index: {out:?}");
    assert!(out.index_error.is_none());

    let read = rig
        .state
        .read_resource("argosy://acme-billing/document/decisions/2026-08-caching")
        .unwrap();
    assert!(read.text.contains("# Decision"));

    // The edit path: rewriting reports `updated`, not `created`.
    let revised = "---\ntype: Decision\ndescription: Cache responses.\n---\n# Decision\n\nWe cache, revisited.\n";
    let out = rig
        .state
        .write_document(WriteParams {
            cwd: project(),
            path: "document/decisions/2026-08-caching".to_string(),
            content: revised.to_string(),
        })
        .unwrap();
    assert_eq!(out.action, "updated");
    assert!(out.indexed);

    let out = rig
        .state
        .delete_document(ReadPathParams {
            cwd: project(),
            path: "document/decisions/2026-08-caching".to_string(),
        })
        .unwrap();
    assert_eq!(out.action, "deleted");
    assert!(out.indexed);
    rig.state
        .read_resource("argosy://acme-billing/document/decisions/2026-08-caching")
        .unwrap_err();
    rig.state
        .delete_document(ReadPathParams {
            cwd: project(),
            path: "document/decisions/2026-08-caching".to_string(),
        })
        .unwrap_err();
}

/// The staleness regression, document flavor: a document written through
/// the MCP surface is findable via search in the SAME session, and a
/// deleted one disappears immediately.
#[test]
fn write_then_search_and_delete_then_search_documents_are_visible() {
    let mut rig = rig();
    let content = "---\ntype: Reference\ndescription: Zinc whisker relay failures.\n---\n\
         Zinc whiskers bridge relays after humid summers.\n";
    let out = rig
        .state
        .write_document(WriteParams {
            cwd: project(),
            path: "document/zinc-whiskers".to_string(),
            content: content.to_string(),
        })
        .unwrap();
    assert!(out.indexed, "the write reconciled the index");

    let report = rig
        .state
        .search(SearchParams {
            cwd: project(),
            query: "zinc whisker relay failures".to_string(),
            k: Some(10),
            namespaces: Some(vec!["document".to_string()]),
            argosy: None,
            tags: None,
            r#type: None,
            language: None,
            category: None,
        })
        .unwrap();
    assert!(
        report
            .hits
            .iter()
            .any(|h| h.uri.ends_with("document/zinc-whiskers")),
        "the fresh write is searchable now, got {:?}",
        report.hits
    );

    let out = rig
        .state
        .delete_document(ReadPathParams {
            cwd: project(),
            path: "document/zinc-whiskers".to_string(),
        })
        .unwrap();
    assert!(out.indexed);
    let report = rig
        .state
        .search(SearchParams {
            cwd: project(),
            query: "zinc whisker relay failures".to_string(),
            k: Some(10),
            namespaces: Some(vec!["document".to_string()]),
            argosy: None,
            tags: None,
            r#type: None,
            language: None,
            category: None,
        })
        .unwrap();
    assert!(
        report
            .hits
            .iter()
            .all(|h| !h.uri.ends_with("document/zinc-whiskers")),
        "the deletion is reflected now, got {:?}",
        report.hits
    );
}

#[test]
fn write_document_rejects_untyped_reserved_and_escape_paths() {
    let mut rig = rig();
    for (path, content) in [
        ("../escape", "# Just prose\n"),
        ("document/index", "---\ntype: Note\n---\nx\n"), // index.md is reserved
        ("document/malformed", "---\ntype: [oops\n---\nx\n"),
        ("document/untyped", "# Just prose\n"), // DOC-1: no frontmatter type
    ] {
        let err = rig
            .state
            .write_document(WriteParams {
                cwd: project(),
                path: path.to_string(),
                content: content.to_string(),
            })
            .unwrap_err();
        assert!(
            matches!(
                err,
                Error::Validation { .. }
                    | Error::NamespaceContractViolation { .. }
                    | Error::ReservedFilename
            ),
            "{path}: got {err:?}"
        );
    }
    // Nothing was written for any of them.
    let local_root = rig
        .state
        .session(project())
        .unwrap()
        .context
        .local()
        .root()
        .to_path_buf();
    assert!(!local_root.join("document/untyped.md").is_file());
    assert!(!local_root.join("document/index.md").is_file());
}
