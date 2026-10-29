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

    // The bundle lands in the project's slot under the state dir, not the project tree.
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

/// A session idle past the TTL is dropped and reopened on next use.
#[test]
fn an_idle_project_session_expires_and_reopens() {
    let opens = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&opens);
    let local = fixture_copy("valid-acme-billing");
    let local_root = local.path().to_path_buf();
    // A zero TTL makes every cached session immediately stale.
    let mut state = McpState::new(Arc::new(move |_root| {
        opens.fetch_add(1, Ordering::SeqCst);
        let context = ProjectContext::open(&local_root, [])?;
        let mut index = Index::new(MockEmbedder::new(), MemStore::new());
        index.reconcile(&context)?;
        Ok(ProjectSession::new(context, index))
    }))
    .with_session_limits(std::time::Duration::ZERO, 8);

    state
        .list_skills(ListSkillsParams { cwd: project() })
        .unwrap();
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
    assert_eq!(
        seen.load(Ordering::SeqCst),
        2,
        "an expired session is reopened through the factory"
    );
}

/// The LRU cap drops the coldest root.
#[test]
fn the_project_session_cap_evicts_the_least_recently_used_root() {
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
    }))
    .with_session_limits(std::time::Duration::from_secs(3600), 2);

    let first = PathBuf::from("/first");
    let second = PathBuf::from("/second");
    let third = PathBuf::from("/third");
    for cwd in [&first, &second, &first] {
        state
            .list_skills(ListSkillsParams { cwd: cwd.clone() })
            .unwrap();
    }
    // `second` is the coldest entry; opening `third` evicts it.
    state
        .list_skills(ListSkillsParams { cwd: third.clone() })
        .unwrap();
    assert_eq!(seen.load(Ordering::SeqCst), 3, "three opens, no reopens");

    state
        .list_skills(ListSkillsParams { cwd: first.clone() })
        .unwrap();
    assert_eq!(seen.load(Ordering::SeqCst), 3, "`first` stayed cached");
    state
        .list_skills(ListSkillsParams {
            cwd: second.clone(),
        })
        .unwrap();
    assert_eq!(seen.load(Ordering::SeqCst), 4, "`second` reopened");
}
