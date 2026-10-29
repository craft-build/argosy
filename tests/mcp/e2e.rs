//! End-to-end tests over an in-process duplex.

use std::sync::Arc;

use super::common::*;
use argosy::LocalArgosy;
use argosy::context::ProjectContext;
use argosy::index::Index;
use argosy::mcp::{ArgosyMcpServer, McpState, ProjectSession, SessionFactory};

#[tokio::test(flavor = "multi_thread")]
async fn end_to_end_over_in_process_duplex() {
    let rig = rig();
    let cwd = rig.cwd.clone();
    let (server_io, client_io) = tokio::io::duplex(8192);

    let server = ArgosyMcpServer::new(rig.state);
    let server_task = tokio::spawn(async move {
        use rmcp::ServiceExt;
        let running = server.serve(server_io).await.expect("server initializes");
        // `waiting()`, not `cancel()`, serves until the client drops the duplex.
        running.waiting().await
    });

    // `()` is rmcp's minimal client handler.
    use rmcp::ServiceExt;
    let client = ().serve(client_io).await.expect("initialize handshake");

    let skills = complete(
        client
            .call_tool_once(call("list_skills", serde_json::json!({"cwd": &cwd})))
            .await
            .unwrap(),
    );
    let skills = structured(&skills);
    let shared = skills["skills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "shared-audit")
        .expect("imported skill listed");
    assert_eq!(shared["argosy"], "acme-shared");
    assert_eq!(shared["verified"], "machine-confirmed");
    assert!(
        skills["skills"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["name"] == "reconcile-ledger" && s["verified"] == "unverified")
    );

    let search = complete(
        client
            .call_tool_once(call(
                "search",
                serde_json::json!({"cwd": &cwd, "query": "rate limit retries"}),
            ))
            .await
            .unwrap(),
    );
    let search = structured(&search);
    assert!(
        search["hits"].as_array().unwrap().iter().any(|h| h["uri"]
            .as_str()
            .unwrap()
            .starts_with("argosy://acme-billing/")),
        "qualified hits, got {search}"
    );
    // Hits are passage-level; the body lines are what a ranged read can expand.
    let hit = &search["hits"].as_array().unwrap()[0];
    assert!(
        hit["passage"]
            .as_str()
            .is_some_and(|p| !p.is_empty() && p.starts_with("Concept: ")),
        "hit carries its passage: {hit}"
    );
    assert!(
        hit["chunk_ordinal"].is_number(),
        "hit names its chunk: {hit}"
    );
    assert!(
        hit["start_line"].as_u64().unwrap_or(0) >= 1
            && hit["end_line"].as_u64().unwrap_or(0) >= hit["start_line"].as_u64().unwrap_or(1),
        "hit carries an exact line range: {hit}"
    );

    let content = "---\ntype: Session Note\ndescription: e2e\n---\n# E2E\n\nBody.\n";
    let written = complete(
        client
            .call_tool_once(call(
                "write_memory",
                serde_json::json!({
                    "cwd": &cwd,
                    "path": "memory/e2e-note",
                    "content": content,
                }),
            ))
            .await
            .unwrap(),
    );
    let written = structured(&written);
    assert_eq!(written["uri"], "argosy://acme-billing/memory/e2e-note");
    assert_eq!(written["action"], "created");
    assert_eq!(written["indexed"], true, "the write reconciled the index");

    let fresh = complete(
        client
            .call_tool_once(call(
                "search",
                serde_json::json!({
                    "cwd": &cwd,
                    "query": "e2e note body",
                    "namespaces": ["memory"]
                }),
            ))
            .await
            .unwrap(),
    );
    let fresh = structured(&fresh);
    assert!(
        fresh["hits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["uri"].as_str().unwrap().ends_with("memory/e2e-note")),
        "the fresh write is searchable in the same session, got {fresh}"
    );

    let read_back = complete(
        client
            .call_tool_once(call(
                "read_memory",
                serde_json::json!({"cwd": &cwd, "path": "memory/e2e-note"}),
            ))
            .await
            .unwrap(),
    );
    assert!(
        structured(&read_back)["content"]
            .as_str()
            .unwrap()
            .contains("# E2E")
    );

    // Resources only serve the cwd project, so imports need `read`'s `argosy` selector.
    let imported_read = complete(
        client
            .call_tool_once(call(
                "read",
                serde_json::json!({
                    "cwd": &cwd,
                    "path": "skill/shared-audit",
                    "argosy": "acme-shared",
                }),
            ))
            .await
            .unwrap(),
    );
    let imported_read = structured(&imported_read);
    assert_eq!(imported_read["kind"], "imported");
    assert_eq!(
        imported_read["uri"],
        "argosy://acme-shared/skill/shared-audit"
    );
    assert!(
        imported_read["content"]
            .as_str()
            .unwrap()
            .contains("Steps."),
        "imported content round-trips, got {imported_read}"
    );

    let unknown_argosy = complete(
        client
            .call_tool_once(call(
                "read",
                serde_json::json!({
                    "cwd": &cwd,
                    "path": "skill/shared-audit",
                    "argosy": "not-active",
                }),
            ))
            .await
            .unwrap(),
    );
    assert_eq!(
        unknown_argosy.is_error,
        Some(true),
        "unknown argosy is a tool error"
    );

    let doc = "---\ntype: Decision\ndescription: e2e doc\n---\n# E2E Doc\n\nWe decide.\n";
    let written = complete(
        client
            .call_tool_once(call(
                "write_document",
                serde_json::json!({
                    "cwd": &cwd,
                    "path": "document/e2e-decision",
                    "content": doc,
                }),
            ))
            .await
            .unwrap(),
    );
    let written = structured(&written);
    assert_eq!(
        written["uri"],
        "argosy://acme-billing/document/e2e-decision"
    );
    assert_eq!(written["action"], "created");
    assert_eq!(written["indexed"], true, "the write reconciled the index");

    let fresh_doc = complete(
        client
            .call_tool_once(call(
                "search",
                serde_json::json!({
                    "cwd": &cwd,
                    "query": "e2e decision doc",
                    "namespaces": ["document"]
                }),
            ))
            .await
            .unwrap(),
    );
    let fresh_doc = structured(&fresh_doc);
    assert!(
        fresh_doc["hits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["uri"]
                .as_str()
                .unwrap()
                .ends_with("document/e2e-decision")),
        "the fresh document is searchable in the same session, got {fresh_doc}"
    );

    let deleted = complete(
        client
            .call_tool_once(call(
                "delete_document",
                serde_json::json!({"cwd": &cwd, "path": "document/e2e-decision"}),
            ))
            .await
            .unwrap(),
    );
    assert_eq!(structured(&deleted)["action"], "deleted");

    let promoted = complete(
        client
            .call_tool_once(call(
                "promote",
                serde_json::json!({
                    "cwd": &cwd,
                    "source_path": "memory/e2e-note",
                    "target": "document",
                    "new_path": "document/e2e-promoted",
                }),
            ))
            .await
            .unwrap(),
    );
    let promoted = structured(&promoted);
    assert_eq!(promoted["target"], "document");
    assert_eq!(
        promoted["new_uri"],
        "argosy://acme-billing/document/e2e-promoted"
    );

    let resource = client
        .read_resource(rmcp::model::ReadResourceRequestParams::new(
            "argosy://acme-billing/document/e2e-promoted",
        ))
        .await
        .unwrap();
    let text = match &resource.contents[0] {
        rmcp::model::ResourceContents::TextResourceContents { text, .. } => text,
        other => panic!("expected text contents, got {other:?}"),
    };
    assert!(text.contains("type: Session Note"), "frontmatter survives");

    let argosys = client
        .read_resource(rmcp::model::ReadResourceRequestParams::new(
            argosy::mcp::ARGOSYS_URI,
        ))
        .await
        .unwrap();
    let text = match &argosys.contents[0] {
        rmcp::model::ResourceContents::TextResourceContents { text, .. } => text,
        other => panic!("expected text contents, got {other:?}"),
    };
    assert!(
        text.contains("acme-shared"),
        "imported argosy listed: {text}"
    );

    // Unknown resource: protocol-level resource-not-found, not empty content.
    let missing = client
        .read_resource(rmcp::model::ReadResourceRequestParams::new(
            "argosy://acme-billing/memory/nope",
        ))
        .await;
    assert!(missing.is_err(), "unknown concept must be an error");

    // Tool-level failure stays a tool error: unknown tool arguments.
    let bad = complete(
        client
            .call_tool_once(call(
                "write_memory",
                serde_json::json!({"cwd": &cwd, "path": "../escape"}),
            ))
            .await
            .unwrap(),
    );
    assert_eq!(bad.is_error, Some(true), "bad write is a tool error");

    let prompts = client.list_prompts(None).await.unwrap();
    let names: Vec<_> = prompts.prompts.iter().map(|p| p.name.as_str()).collect();
    #[cfg(feature = "code-tools")]
    assert_eq!(
        names,
        ["dream", "scan", "review"],
        "exactly the advertised set"
    );
    #[cfg(not(feature = "code-tools"))]
    assert_eq!(names, ["dream", "scan"], "exactly the advertised set");
    for prompt in &prompts.prompts {
        assert!(
            prompt.description.as_deref().is_some_and(|d| !d.is_empty()),
            "{} carries a description",
            prompt.name
        );
    }

    let dream = client
        .get_prompt(rmcp::model::GetPromptRequestParams::new("dream"))
        .await
        .unwrap();
    assert_eq!(dream.messages.len(), 1);
    assert_eq!(dream.messages[0].role, rmcp::model::Role::User);
    match &dream.messages[0].content {
        rmcp::model::ContentBlock::Text(text) => {
            // Self-contained workflow: the tools it drives are all named.
            for tool in ["search", "read_memory", "write_memory", "delete_memory"] {
                assert!(text.text.contains(tool), "dream names `{tool}`");
            }
            assert!(text.text.contains("no-op is a valid outcome"));
        }
        other => panic!("expected text content, got {other:?}"),
    }

    let scan = client
        .get_prompt(rmcp::model::GetPromptRequestParams::new("scan"))
        .await
        .unwrap();
    assert_eq!(scan.messages.len(), 1);
    assert_eq!(scan.messages[0].role, rmcp::model::Role::User);
    match &scan.messages[0].content {
        rmcp::model::ContentBlock::Text(text) => {
            for tool in ["search", "read_memory", "write_document", "delete_document"] {
                assert!(text.text.contains(tool), "scan names `{tool}`");
            }
            for path in ["document/summary", "document/architecture", "document/tech"] {
                assert!(text.text.contains(path), "scan names `{path}`");
            }
        }
        other => panic!("expected text content, got {other:?}"),
    }

    #[cfg(feature = "code-tools")]
    {
        let review = client
            .get_prompt(
                rmcp::model::GetPromptRequestParams::new("review").with_arguments(
                    serde_json::json!({
                        "cwd": cwd,
                        "base": "main",
                        "focus": "authorization",
                    })
                    .as_object()
                    .unwrap()
                    .clone(),
                ),
            )
            .await
            .unwrap();
        match &review.messages[0].content {
            rmcp::model::ContentBlock::Text(text) => {
                for required in [
                    "start_review",
                    "review_diff",
                    "search_rules",
                    "report_finding",
                    "review_findings",
                    "authorization",
                ] {
                    assert!(text.text.contains(required), "review names `{required}`");
                }
            }
            other => panic!("expected text content, got {other:?}"),
        }
        let missing_review_args = client
            .get_prompt(rmcp::model::GetPromptRequestParams::new("review"))
            .await;
        assert!(
            missing_review_args.is_err(),
            "required review arguments are enforced"
        );
    }

    // Unknown prompt name: protocol-level method-not-found, not empty content.
    let missing_prompt = client
        .get_prompt(rmcp::model::GetPromptRequestParams::new("nope"))
        .await;
    assert!(missing_prompt.is_err(), "unknown prompt must be an error");

    drop(client);
    server_task.abort();
}

/// `ttlMs` and `cacheScope` are required on list/read results under protocol
/// `2026-07-28`: strict clients reject the response when they are absent.
///
/// Static capability listings are public and long-lived; resource listings and
/// reads are private and never fresh from cache.
#[tokio::test(flavor = "multi_thread")]
async fn list_and_read_results_carry_sep2549_cache_hints() {
    let rig = rig();
    let (server_io, client_io) = tokio::io::duplex(8192);
    let server_task = tokio::spawn(async move {
        use rmcp::ServiceExt;
        let running = ArgosyMcpServer::new(rig.state)
            .serve(server_io)
            .await
            .expect("server initializes");
        let _ = running.waiting().await;
    });
    use rmcp::ServiceExt;
    let client = ().serve(client_io).await.expect("initialize handshake");

    let tools = client.list_tools(None).await.unwrap();
    assert_eq!(tools.ttl_ms, Some(3_600_000), "tools list: public, 1h");
    assert_eq!(tools.cache_scope, Some(rmcp::model::CacheScope::Public));

    let prompts = client.list_prompts(None).await.unwrap();
    assert_eq!(prompts.ttl_ms, Some(3_600_000));
    assert_eq!(prompts.cache_scope, Some(rmcp::model::CacheScope::Public));

    let resources = client.list_resources(None).await.unwrap();
    assert_eq!(resources.ttl_ms, Some(0), "resource list: never fresh");
    assert_eq!(
        resources.cache_scope,
        Some(rmcp::model::CacheScope::Private)
    );

    let read = client
        .read_resource(rmcp::model::ReadResourceRequestParams::new(
            argosy::mcp::ARGOSYS_URI,
        ))
        .await
        .unwrap();
    assert_eq!(read.ttl_ms, Some(0), "resource read: never fresh");
    assert_eq!(read.cache_scope, Some(rmcp::model::CacheScope::Private));

    drop(client);
    server_task.abort();
}

/// The server needs no project at startup: each call's `cwd` lazily opens an isolated session.
#[tokio::test(flavor = "multi_thread")]
async fn tools_select_their_project_by_cwd() {
    // Real discovery path with an injected state root, so the user's store cannot leak in.
    let state = tempfile::tempdir().unwrap();
    let state_root = state.path().to_path_buf();
    let factory: SessionFactory<FakeEmbedder, MemVec> = Arc::new(move |root| {
        let context = ProjectContext::open_project_with_state(root, &state_root)?;
        let mut index = Index::new(FakeEmbedder, MemVec::default());
        index.reconcile(&context)?;
        Ok(ProjectSession::new(context, index))
    });
    let alpha = tempfile::tempdir().unwrap();
    let beta = tempfile::tempdir().unwrap();
    LocalArgosy::init(
        argosy::pull::project_argosy_dir_at(state.path(), alpha.path()).join("default"),
        Some("alpha"),
        None,
    )
    .unwrap();
    LocalArgosy::init(
        argosy::pull::project_argosy_dir_at(state.path(), beta.path()).join("default"),
        Some("beta"),
        None,
    )
    .unwrap();
    assert!(
        !alpha.path().join(".argosy").exists() && !beta.path().join(".argosy").exists(),
        "the project trees stay argosy-free"
    );

    let (server_io, client_io) = tokio::io::duplex(8192);
    let server_task = tokio::spawn(async move {
        use rmcp::ServiceExt;
        let running = ArgosyMcpServer::new(McpState::new(factory))
            .serve(server_io)
            .await
            .expect("server initializes without any project");
        let _ = running.waiting().await;
    });
    use rmcp::ServiceExt;
    let client = ().serve(client_io).await.expect("initialize handshake");

    for (root, name) in [(alpha.path(), "alpha"), (beta.path(), "beta")] {
        let written = complete(
            client
                .call_tool_once(call(
                    "write_memory",
                    serde_json::json!({
                        "cwd": root,
                        "path": "memory/who-am-i",
                        "content": format!(
                            "---\ntype: Session Note\ndescription: {name}\n---\n# {name}\n"
                        ),
                    }),
                ))
                .await
                .unwrap(),
        );
        assert_eq!(
            structured(&written)["uri"],
            format!("argosy://{name}/memory/who-am-i")
        );
    }

    for (root, name, other) in [
        (alpha.path(), "alpha", "beta"),
        (beta.path(), "beta", "alpha"),
    ] {
        let skills = complete(
            client
                .call_tool_once(call(
                    "search",
                    serde_json::json!({
                        "cwd": root,
                        "query": "who am i",
                        "namespaces": ["memory"],
                        "k": 10,
                    }),
                ))
                .await
                .unwrap(),
        );
        let skills = structured(&skills);
        let uris: Vec<&str> = skills["hits"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|h| h["uri"].as_str())
            .collect();
        assert!(
            uris.iter()
                .any(|u| u.starts_with(&format!("argosy://{name}/"))),
            "{name} sees its own write, got {uris:?}"
        );
        assert!(
            !uris
                .iter()
                .any(|u| u.starts_with(&format!("argosy://{other}/"))),
            "{name} must not see {other}'s write, got {uris:?}"
        );
    }

    // A cwd with no argosy is a tool error, not a protocol error.
    let nowhere = tempfile::tempdir().unwrap();
    let result = complete(
        client
            .call_tool_once(call(
                "list_skills",
                serde_json::json!({"cwd": nowhere.path()}),
            ))
            .await
            .unwrap(),
    );
    assert_eq!(
        result.is_error,
        Some(true),
        "argosy-less cwd is a tool error"
    );
    match &result.content[0] {
        rmcp::model::ContentBlock::Text(text) => {
            assert!(text.text.contains("argosy init"), "got {}", text.text);
        }
        other => panic!("expected text error, got {other:?}"),
    }

    drop(client);
    server_task.abort();
}

/// `list_resource_templates` advertises the concept URI template with
/// the static listings' SEP-2549 cache hints.
#[tokio::test(flavor = "multi_thread")]
async fn resource_templates_advertise_the_concept_uri_template() {
    let rig = rig();
    let (server_io, client_io) = tokio::io::duplex(8192);
    let server_task = tokio::spawn(async move {
        use rmcp::ServiceExt;
        let running = ArgosyMcpServer::new(rig.state)
            .serve(server_io)
            .await
            .expect("server initializes");
        let _ = running.waiting().await;
    });
    use rmcp::ServiceExt;
    let client = ().serve(client_io).await.expect("initialize handshake");

    let templates = client.list_resource_templates(None).await.unwrap();
    assert_eq!(
        templates.ttl_ms,
        Some(3_600_000),
        "template list: static, public, 1h"
    );
    assert_eq!(templates.cache_scope, Some(rmcp::model::CacheScope::Public));
    assert_eq!(templates.resource_templates.len(), 1);
    let template = &templates.resource_templates[0];
    assert_eq!(
        template.uri_template,
        argosy::mcp::CONCEPT_TEMPLATE_URI,
        "the advertised template is the concept URI shape"
    );
    assert_eq!(template.name, "argosy-concept");
    assert_eq!(template.mime_type.as_deref(), Some("text/markdown"));
    assert!(
        template
            .description
            .as_deref()
            .is_some_and(|d| !d.is_empty()),
        "the template explains its variables"
    );

    // A URI expanded from the template reads like any concept resource.
    let read = client
        .read_resource(rmcp::model::ReadResourceRequestParams::new(
            "argosy://acme-billing/memory/gotchas",
        ))
        .await
        .unwrap();
    match &read.contents[0] {
        rmcp::model::ResourceContents::TextResourceContents {
            mime_type, text, ..
        } => {
            assert_eq!(mime_type.as_deref(), Some("text/markdown"));
            assert!(text.contains("# Gotchas"), "got {text}");
        }
        other => panic!("expected text contents, got {other:?}"),
    }

    drop(client);
    server_task.abort();
}

/// A restricted toolset shrinks the listing, keeps the enabled tools
/// working, and makes disabled tools unroutable like unknown names.
#[tokio::test(flavor = "multi_thread")]
async fn toolset_restriction_filters_listings_and_rejects_disabled_tools() {
    let rig = rig();
    let cwd = rig.cwd.clone();
    let (server_io, client_io) = tokio::io::duplex(8192);
    let server = ArgosyMcpServer::new(rig.state)
        .with_toolsets(argosy::mcp::Toolsets::from_names(&["knowledge".to_string()]).unwrap());
    let server_task = tokio::spawn(async move {
        use rmcp::ServiceExt;
        let running = server.serve(server_io).await.expect("server initializes");
        let _ = running.waiting().await;
    });
    use rmcp::ServiceExt;
    let client = ().serve(client_io).await.expect("initialize handshake");

    let tools = client.list_all_tools().await.unwrap();
    let names: Vec<String> = tools.iter().map(|t| t.name.to_string()).collect();
    assert!(
        names.contains(&"search".to_string()),
        "enabled set: {names:?}"
    );
    #[cfg(feature = "code-tools")]
    for absent in ["outline", "repomap", "start_review", "review_findings"] {
        assert!(!names.contains(&absent.to_string()), "listing: {names:?}");
    }

    // Knowledge tools keep working.
    let skills = complete(
        client
            .call_tool_once(call("list_skills", serde_json::json!({"cwd": &cwd})))
            .await
            .unwrap(),
    );
    assert_ne!(skills.is_error, Some(true), "knowledge tools still serve");

    // A disabled toolset is unroutable: a protocol error, like an unknown name.
    #[cfg(feature = "code-tools")]
    {
        let disabled = client
            .call_tool_once(call("start_review", serde_json::json!({"cwd": &cwd})))
            .await;
        assert!(disabled.is_err(), "disabled toolset rejected as unknown");
        let unknown = client
            .call_tool_once(call("not-a-tool", serde_json::json!({"cwd": &cwd})))
            .await;
        assert!(unknown.is_err(), "unknown tool rejected");
    }

    drop(client);
    server_task.abort();
}
