//! Code-intelligence tools over the wire: the seven tools ported from
//! Craft, driven like any MCP client would.

use std::fs;
use std::process::Command;

use argosy::mcp::ArgosyMcpServer;
use tempfile::TempDir;

use super::common::*;

/// A small multi-language workspace: Rust + Python + a TODO.
fn workspace() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(
        dir.path().join("src/main.rs"),
        "fn main() {\n    helper();\n}\n\nfn helper() {\n    println!(\"hi\");\n}\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("src/app.py"),
        "def run():\n    # TODO: handle errors\n    return 1\n",
    )
    .unwrap();
    fs::write(dir.path().join("notes.txt"), "not source\n").unwrap();
    dir
}

type TestClient = rmcp::service::RunningService<rmcp::RoleClient, ()>;

async fn client() -> (TestClient, tokio::task::JoinHandle<()>) {
    let rig = rig();
    let (server_io, client_io) = tokio::io::duplex(8192);
    let server = ArgosyMcpServer::new(rig.state);
    let server_task = tokio::spawn(async move {
        use rmcp::ServiceExt;
        let running = server.serve(server_io).await.expect("server initializes");
        let _ = running.waiting().await;
    });
    use rmcp::ServiceExt;
    let client = ().serve(client_io).await.expect("initialize handshake");
    (client, server_task)
}

async fn call_ok(client: &TestClient, name: &str, args: serde_json::Value) -> serde_json::Value {
    let result = complete(client.call_tool_once(call(name, args)).await.unwrap());
    assert_ne!(result.is_error, Some(true), "`{name}` must succeed");
    structured(&result)
}

async fn call_err(client: &TestClient, name: &str, args: serde_json::Value) -> String {
    let result = complete(client.call_tool_once(call(name, args)).await.unwrap());
    assert_eq!(result.is_error, Some(true), "`{name}` must fail");
    match &result.content[0] {
        rmcp::model::ContentBlock::Text(text) => text.text.clone(),
        other => panic!("expected text error, got {other:?}"),
    }
}

fn abs(dir: &TempDir, rel: &str) -> String {
    dir.path().join(rel).to_string_lossy().into_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn outline_file_and_directory() {
    let ws = workspace();
    let (client, server) = client().await;

    // File mode: symbol tree with kinds and line numbers.
    let out = call_ok(
        &client,
        "outline",
        serde_json::json!({"path": abs(&ws, "src/main.rs")}),
    )
    .await;
    assert_eq!(out["truncated"], false);
    assert!(
        out["text"].as_str().unwrap().contains("fn main"),
        "got {out}"
    );
    assert!(out["text"].as_str().unwrap().contains("helper"));

    // Directory mode: per-file trees with a skipped section.
    let out = call_ok(
        &client,
        "outline",
        serde_json::json!({"path": abs(&ws, "src")}),
    )
    .await;
    let text = out["text"].as_str().unwrap();
    assert!(text.contains("app.py"), "got {text}");
    assert!(text.contains("run"), "got {text}");
    assert!(text.contains("total: 2 files"), "got {text}");

    // Files mode: flat table with languages.
    let out = call_ok(
        &client,
        "outline",
        serde_json::json!({"path": abs(&ws, "src"), "files": true}),
    )
    .await;
    let text = out["text"].as_str().unwrap();
    assert!(text.contains("rust"), "got {text}");
    assert!(text.contains("python"), "got {text}");

    // Unsupported language is a report, not an error.
    let out = call_ok(
        &client,
        "outline",
        serde_json::json!({"path": abs(&ws, "notes.txt")}),
    )
    .await;
    assert!(out["text"].as_str().unwrap().contains("unsupported"));

    let err = call_err(
        &client,
        "outline",
        serde_json::json!({"path": abs(&ws, "nope.rs")}),
    )
    .await;
    assert!(err.contains("does not exist"), "{err}");

    drop(client);
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn zoom_symbol_range_and_ambiguity() {
    let ws = workspace();
    let (client, server) = client().await;

    let out = call_ok(
        &client,
        "zoom",
        serde_json::json!({
            "path": abs(&ws, "src/main.rs"),
            "symbol": "helper",
            "context_lines": 0,
        }),
    )
    .await;
    let text = out["text"].as_str().unwrap();
    assert!(text.contains("fn helper"), "got {text}");
    assert!(text.contains("5 |"), "numbered gutter, got {text}");

    let out = call_ok(
        &client,
        "zoom",
        serde_json::json!({
            "path": abs(&ws, "src/main.rs"),
            "start_line": 1,
            "end_line": 2,
        }),
    )
    .await;
    assert!(out["text"].as_str().unwrap().contains("fn main"));

    let err = call_err(
        &client,
        "zoom",
        serde_json::json!({"path": abs(&ws, "src/main.rs")}),
    )
    .await;
    assert!(err.contains("provide either"), "{err}");

    let err = call_err(
        &client,
        "zoom",
        serde_json::json!({
            "path": abs(&ws, "src/main.rs"),
            "symbol": "definitely_not_there",
        }),
    )
    .await;
    assert!(err.contains("not found"), "{err}");

    drop(client);
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn astgrep_search_diff_apply_and_guards() {
    let ws = workspace();
    let (client, server) = client().await;

    // Search mode.
    let out = call_ok(
        &client,
        "astgrep",
        serde_json::json!({
            "pattern": "println!($MSG)",
            "lang": "rust",
            "path": abs(&ws, "src"),
        }),
    )
    .await;
    assert_eq!(out["mode"], "search");
    assert_eq!(out["matches"], 1);
    assert!(
        out["text"].as_str().unwrap().contains("main.rs:6"),
        "got {out}"
    );

    // Dry-run: unified diff, file untouched.
    let before = fs::read_to_string(ws.path().join("src/main.rs")).unwrap();
    let out = call_ok(
        &client,
        "astgrep",
        serde_json::json!({
            "pattern": "println!($MSG)",
            "rewrite": "eprintln!($MSG)",
            "lang": "rust",
            "path": abs(&ws, "src"),
        }),
    )
    .await;
    assert_eq!(out["mode"], "diff");
    assert!(
        out["text"].as_str().unwrap().contains("+    eprintln!"),
        "got {out}"
    );
    assert_eq!(
        fs::read_to_string(ws.path().join("src/main.rs")).unwrap(),
        before
    );

    // Apply: writes and reports.
    let out = call_ok(
        &client,
        "astgrep",
        serde_json::json!({
            "pattern": "println!($MSG)",
            "rewrite": "eprintln!($MSG)",
            "lang": "rust",
            "path": abs(&ws, "src"),
            "apply": true,
        }),
    )
    .await;
    assert_eq!(out["mode"], "apply");
    assert_eq!(out["files_changed"], 1);
    assert_eq!(out["rolled_back"], false);
    assert!(
        fs::read_to_string(ws.path().join("src/main.rs"))
            .unwrap()
            .contains("eprintln!"),
        "the file was rewritten"
    );

    // Syntax-breaking rewrite is rolled back, file untouched.
    fs::write(
        ws.path().join("src/broken.rs"),
        "fn broken() {\n    println!(\"x\");\n}\n",
    )
    .unwrap();
    let before = fs::read_to_string(ws.path().join("src/broken.rs")).unwrap();
    let out = call_ok(
        &client,
        "astgrep",
        serde_json::json!({
            "pattern": "println!($MSG)",
            "rewrite": ")))) not rust",
            "lang": "rust",
            "path": abs(&ws, "src/broken.rs"),
            "apply": true,
        }),
    )
    .await;
    assert_eq!(out["rolled_back"], true);
    assert_eq!(
        fs::read_to_string(ws.path().join("src/broken.rs")).unwrap(),
        before
    );

    // Stale read: dry-run records the read, an external change then
    // blocks the apply.
    let out = call_ok(
        &client,
        "astgrep",
        serde_json::json!({
            "pattern": "println!($MSG)",
            "rewrite": "eprintln!($MSG)",
            "lang": "rust",
            "path": abs(&ws, "src/broken.rs"),
        }),
    )
    .await;
    assert_eq!(out["mode"], "diff");
    fs::write(
        ws.path().join("src/broken.rs"),
        "fn broken() {\n    println!(\"changed\");\n}\n",
    )
    .unwrap();
    // Stale read: dry-run records the read, an external change then makes
    // the apply skip that file. Skipping is reported inline (per-file), not
    // a whole-run error — files earlier in the walk may already be written.
    let out = call_ok(
        &client,
        "astgrep",
        serde_json::json!({
            "pattern": "println!($MSG)",
            "rewrite": "eprintln!($MSG)",
            "lang": "rust",
            "path": abs(&ws, "src/broken.rs"),
            "apply": true,
        }),
    )
    .await;
    assert_eq!(out["mode"], "apply");
    assert_eq!(out["files_changed"], 0);
    assert!(
        out["text"]
            .as_str()
            .unwrap()
            .contains("changed since last read"),
        "got {out}"
    );
    assert!(
        fs::read_to_string(ws.path().join("src/broken.rs"))
            .unwrap()
            .contains("changed"),
        "the stale file stays untouched"
    );

    // Unknown language is a tool error listing the supported set.
    let err = call_err(
        &client,
        "astgrep",
        serde_json::json!({"pattern": "x", "lang": "starlark"}),
    )
    .await;
    assert!(err.contains("unsupported language"), "{err}");

    drop(client);
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn conflicts_list_and_resolve() {
    let ws = workspace();
    fs::write(
        ws.path().join("merged.rs"),
        "top\n<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> feature\nbottom\n",
    )
    .unwrap();
    let (client, server) = client().await;

    let out = call_ok(
        &client,
        "conflicts",
        serde_json::json!({"path": abs(&ws, "merged.rs")}),
    )
    .await;
    let text = out["text"].as_str().unwrap();
    assert!(text.contains("merge conflicts in 1 file(s)"), "got {text}");
    assert!(text.contains("HEAD vs feature"));
    assert!(out["resolved"].is_null(), "listing has no resolved count");

    let out = call_ok(
        &client,
        "conflicts",
        serde_json::json!({
            "path": abs(&ws, "merged.rs"),
            "resolve": "@theirs",
        }),
    )
    .await;
    assert_eq!(out["resolved"], 1);
    assert_eq!(
        fs::read_to_string(ws.path().join("merged.rs")).unwrap(),
        "top\ntheirs\nbottom\n"
    );

    let err = call_err(
        &client,
        "conflicts",
        serde_json::json!({"resolve": "@nope"}),
    )
    .await;
    assert!(err.contains("unknown resolve choice"), "{err}");

    drop(client);
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn inspect_finds_todos() {
    let ws = workspace();
    let (client, server) = client().await;

    let out = call_ok(
        &client,
        "inspect",
        serde_json::json!({
            "sections": "todos",
            "scope": abs(&ws, "src/app.py"),
        }),
    )
    .await;
    let text = out["text"].as_str().unwrap();
    assert!(text.contains("(1 items)"), "got {text}");
    assert!(text.contains("handle errors"));

    drop(client);
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn callgraph_over_the_wire() {
    let ws = workspace();
    let (client, server) = client().await;

    let out = call_ok(
        &client,
        "callgraph",
        serde_json::json!({
            "op": "call_tree",
            "path": abs(&ws, "src/main.rs"),
            "symbol": "main",
        }),
    )
    .await;
    let text = out["text"].as_str().unwrap();
    assert!(text.contains("main (line 1)"), "got {text}");
    assert!(text.contains("helper"), "got {text}");

    let out = call_ok(
        &client,
        "callgraph",
        serde_json::json!({
            "op": "callers",
            "path": abs(&ws, "src/main.rs"),
            "symbol": "helper",
        }),
    )
    .await;
    assert!(
        out["text"]
            .as_str()
            .unwrap()
            .contains("callers of \"helper\""),
        "got {out}"
    );

    let out = call_ok(
        &client,
        "callgraph",
        serde_json::json!({
            "op": "impact",
            "path": abs(&ws, "src/main.rs"),
            "symbol": "helper",
        }),
    )
    .await;
    assert!(out["text"].as_str().unwrap().contains("main"), "got {out}");

    let err = call_err(
        &client,
        "callgraph",
        serde_json::json!({
            "op": "impact",
            "path": abs(&ws, "notes.txt"),
            "symbol": "x",
        }),
    )
    .await;
    assert!(err.contains("unsupported file type"), "{err}");

    drop(client);
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn repomap_renders_and_caches() {
    let ws = workspace();
    let (client, server) = client().await;

    let out = call_ok(
        &client,
        "repomap",
        serde_json::json!({
            "path": abs(&ws, "src"),
            "query": "show me the helper function",
        }),
    )
    .await;
    let text = out["text"].as_str().unwrap();
    assert!(text.contains("main.rs"), "got {text}");
    assert!(text.contains("helper"), "got {text}");

    // Same call again hits the per-root cache: identical output.
    let again = call_ok(
        &client,
        "repomap",
        serde_json::json!({
            "path": abs(&ws, "src"),
            "query": "show me the helper function",
        }),
    )
    .await;
    assert_eq!(out["text"], again["text"]);

    drop(client);
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn structured_review_findings_round_trip_over_mcp() {
    let ws = workspace();
    for args in [
        &["init", "--quiet"][..],
        &["config", "user.email", "review@example.com"][..],
        &["config", "user.name", "Review Test"][..],
        &["add", "."][..],
        &["commit", "--quiet", "-m", "initial"][..],
    ] {
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(ws.path())
                .args(args)
                .status()
                .unwrap()
                .success(),
            "git {args:?} failed"
        );
    }
    fs::write(
        ws.path().join("src/main.rs"),
        "fn main() {\n    changed();\n}\n\nfn changed() {}\n",
    )
    .unwrap();
    let (client, server) = client().await;

    let opened = call_ok(
        &client,
        "open_review",
        serde_json::json!({
            "cwd": ws.path(),
            "timeout_minutes": 1,
        }),
    )
    .await;
    let review_id = opened["review_id"].as_str().unwrap();
    assert_eq!(opened["changed_files"], serde_json::json!(["src/main.rs"]));
    let snapshot_files = call_ok(
        &client,
        "review_diff",
        serde_json::json!({"review_id": review_id}),
    )
    .await;
    assert_eq!(
        snapshot_files["changed_files"],
        serde_json::json!(["src/main.rs"])
    );
    assert!(snapshot_files["diff"].is_null());
    let snapshot = call_ok(
        &client,
        "review_diff",
        serde_json::json!({"review_id": review_id, "path": "src/main.rs"}),
    )
    .await;
    assert!(
        snapshot["diff"]
            .as_str()
            .unwrap()
            .contains("+    changed();"),
        "got {snapshot}"
    );
    let reported = call_ok(
        &client,
        "report_finding",
        serde_json::json!({
            "review_id": review_id,
            "title": "[P1] Preserve the helper contract",
            "body": "When the old helper is removed, main now calls an undefined replacement and the build fails. Restore the helper or update the call.",
            "priority": "P1",
            "confidence": 1.0,
            "path": "src/main.rs",
            "line_start": 2,
            "line_end": 2,
            "rule_uris": ["argosy://acme-billing/styleguide/rust/errors"],
            "suggestion": "Define changed or keep the existing helper."
        }),
    )
    .await;
    assert_eq!(reported["created"], true);

    let findings = call_ok(
        &client,
        "review_findings",
        serde_json::json!({"review_id": review_id, "priority": "P1"}),
    )
    .await;
    assert_eq!(findings["findings"].as_array().unwrap().len(), 1);
    let status = call_ok(
        &client,
        "review_status",
        serde_json::json!({"review_id": review_id}),
    )
    .await;
    assert_eq!(status["status"], "pending");
    assert_eq!(status["findings"].as_array().unwrap().len(), 1);

    drop(client);
    server.abort();
}
