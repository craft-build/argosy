//! The multi-project session cache.

use super::*;

/// The cache contract: one open per project root, reused across calls,
/// a new open per distinct root.
#[test]
fn sessions_open_once_per_root_and_are_reused_across_calls() {
    let opens = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&opens);
    let local = fixture_copy("valid-acme-billing");
    let local_root = local.path().to_path_buf();
    let mut state = McpState::new(Arc::new(move |_root| {
        opens.fetch_add(1, Ordering::SeqCst);
        let context = ProjectContext::open(&local_root, [])?;
        let mut index = Index::new(MockEmbedder::new(), MemStore::new());
        index.reconcile(&context)?;
        Ok(ProjectSession::new(context, index))
    }));

    state
        .search(SearchParams {
            cwd: project(),
            query: "anything".to_string(),
            k: None,
            namespaces: None,
            argosy: None,
            tags: None,
            r#type: None,
            language: None,
            category: None,
        })
        .unwrap();
    state
        .list_skills(ListSkillsParams { cwd: project() })
        .unwrap();
    assert_eq!(
        seen.load(Ordering::SeqCst),
        1,
        "two calls on the same cwd share one open"
    );

    state
        .list_skills(ListSkillsParams {
            cwd: PathBuf::from("/elsewhere"),
        })
        .unwrap();
    assert_eq!(
        seen.load(Ordering::SeqCst),
        2,
        "a different cwd opens a new session"
    );
}

/// A cwd with no argosy is a tool-level error pointing at `argosy init` —
/// and it is never cached, so a later init is picked up by the very next
/// call.
#[test]
fn a_cwd_without_an_argosy_errors_and_is_not_cached() {
    let empty = tempfile::tempdir().unwrap();
    let state_tmp = tempfile::tempdir().unwrap();
    let empty_root = empty.path().to_path_buf();
    let state_root = state_tmp.path().to_path_buf();
    let factory_root = state_root.clone();
    let opens = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&opens);
    let mut state = McpState::new(Arc::new(move |root| {
        opens.fetch_add(1, Ordering::SeqCst);
        let context = ProjectContext::open_project_with_state(root, &factory_root)?;
        let mut index = Index::new(MockEmbedder::new(), MemStore::new());
        index.reconcile(&context)?;
        Ok(ProjectSession::new(context, index))
    }));

    let err = state
        .list_skills(ListSkillsParams {
            cwd: empty_root.clone(),
        })
        .unwrap_err();
    assert!(
        err.to_string().contains("argosy init") && err.to_string().contains("default"),
        "unexpected error: {err}"
    );

    // `argosy init` after the failure: the next call sees it (the
    // failure was not cached) — the bundle lands in the project's slot
    // under the state dir, not the project tree.
    let local = LocalArgosy::init(
        crate::pull::project_argosy_dir_at(&state_root, &empty_root)
            .join(crate::pull::LOCAL_ARGOSY_NAME),
        Some("fresh"),
        None,
    )
    .unwrap();
    drop(local);
    assert!(
        !empty_root.join(".argosy").exists(),
        "the project tree stays argosy-free"
    );
    let skills = state
        .list_skills(ListSkillsParams { cwd: empty_root })
        .unwrap();
    assert!(skills.skills.is_empty());
    assert_eq!(seen.load(Ordering::SeqCst), 2, "the failed open retried");
}
