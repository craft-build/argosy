//! Tool-schema tests.

/// Imported argosys are read-only *structurally*: no mutating tool may expose an `argosy` selector.
#[test]
fn write_tools_have_no_argosy_selector_in_their_schemas() {
    let tools = argosy::mcp::tool_definitions();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    for expected in [
        "search",
        "list_skills",
        "get_skill",
        "search_rules",
        "read_memory",
        "read",
        "write_memory",
        "delete_memory",
        "write_rule",
        "delete_rule",
        "write_document",
        "delete_document",
        "promote",
    ] {
        assert!(names.contains(&expected), "missing tool `{expected}`");
    }
    #[cfg(feature = "code-tools")]
    for expected in [
        "outline",
        "zoom",
        "astgrep",
        "conflicts",
        "inspect",
        "callgraph",
        "repomap",
        "start_review",
        "review_diff",
        "report_finding",
        "review_findings",
    ] {
        assert!(names.contains(&expected), "missing tool `{expected}`");
    }
    #[cfg(feature = "decision")]
    assert!(names.contains(&"ask"), "missing tool `ask`");
    #[cfg(all(feature = "code-tools", feature = "decision"))]
    let expected_total = 25;
    #[cfg(all(feature = "code-tools", not(feature = "decision")))]
    let expected_total = 24;
    #[cfg(all(not(feature = "code-tools"), feature = "decision"))]
    let expected_total = 14;
    #[cfg(all(not(feature = "code-tools"), not(feature = "decision")))]
    let expected_total = 13;
    assert_eq!(
        tools.len(),
        expected_total,
        "exactly the documented tool set"
    );
    assert!(!names.contains(&"open_review"), "no browser review tool");
    assert!(
        !names.contains(&"review_status"),
        "no redundant review status tool"
    );

    for tool in &tools {
        let props = tool.input_schema["properties"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        if tool.name == "start_review" {
            assert!(props.contains_key("cwd"));
            assert!(props.contains_key("base"));
            assert!(props.contains_key("commit"));
            assert!(!props.contains_key("port"));
            assert!(!props.contains_key("timeout_minutes"));
        }
        if [
            "write_memory",
            "delete_memory",
            "write_rule",
            "delete_rule",
            "write_document",
            "delete_document",
            "promote",
        ]
        .contains(&tool.name.as_ref())
        {
            assert!(
                !props.contains_key("argosy"),
                "mutating tool `{}` must not take an argosy selector",
                tool.name
            );
        } else if matches!(tool.name.as_ref(), "search" | "read") {
            assert!(
                props.contains_key("argosy"),
                "{} scopes by argosy",
                tool.name
            );
        }
        assert!(
            tool.description.as_deref().is_some_and(|d| d.len() > 40),
            "tool `{}` needs a real description",
            tool.name
        );
    }
}

/// Toolsets partition the advertised surface: every compiled-in tool has a
/// set, and each set's filtered definitions are exactly that set's tools.
#[test]
fn toolsets_partition_the_advertised_tools() {
    use argosy::mcp::{Toolsets, tool_definitions_for, toolset_of};

    let all = argosy::mcp::tool_definitions();
    for tool in &all {
        assert!(
            toolset_of(tool.name.as_ref()).is_some(),
            "tool `{}` belongs to no toolset",
            tool.name
        );
    }

    let knowledge =
        tool_definitions_for(&Toolsets::from_names(&["knowledge".to_string()]).unwrap());
    let names: Vec<String> = knowledge.iter().map(|t| t.name.to_string()).collect();
    for expected in [
        "search",
        "list_skills",
        "get_skill",
        "search_rules",
        "read_memory",
        "read",
        "write_memory",
        "delete_memory",
        "write_rule",
        "delete_rule",
        "write_document",
        "delete_document",
        "promote",
    ] {
        assert!(
            names.contains(&expected.to_string()),
            "knowledge set: {names:?}"
        );
    }
    #[cfg(feature = "decision")]
    assert!(
        names.contains(&"ask".to_string()),
        "knowledge set: {names:?}"
    );
    #[cfg(feature = "code-tools")]
    for absent in [
        "outline",
        "zoom",
        "astgrep",
        "conflicts",
        "inspect",
        "callgraph",
        "repomap",
        "start_review",
        "review_diff",
        "report_finding",
        "review_findings",
    ] {
        assert!(
            !names.contains(&absent.to_string()),
            "knowledge set: {names:?}"
        );
    }

    // Exact sizes per feature combination: the sets never overlap and
    // together cover the whole surface.
    #[cfg(all(feature = "code-tools", feature = "decision"))]
    let (knowledge_n, review_n, code_n, all_n) = (14, 4, 7, 25);
    #[cfg(all(feature = "code-tools", not(feature = "decision")))]
    let (knowledge_n, review_n, code_n, all_n) = (13, 4, 7, 24);
    #[cfg(all(not(feature = "code-tools"), feature = "decision"))]
    let (knowledge_n, all_n) = (14, 14);
    #[cfg(all(not(feature = "code-tools"), not(feature = "decision")))]
    let (knowledge_n, all_n) = (13, 13);
    assert_eq!(knowledge.len(), knowledge_n, "knowledge set: {names:?}");
    assert_eq!(all.len(), all_n);
    #[cfg(feature = "code-tools")]
    {
        let review = tool_definitions_for(&Toolsets::from_names(&["review".to_string()]).unwrap());
        assert_eq!(review.len(), review_n);
        let code = tool_definitions_for(&Toolsets::from_names(&["code".to_string()]).unwrap());
        assert_eq!(code.len(), code_n);
        // Selecting every set is the unfiltered listing.
        assert_eq!(
            tool_definitions_for(&Toolsets::all()).len(),
            all.len(),
            "all toolsets = unfiltered definitions"
        );
    }
}

/// Toolset selection rejects empty input, empty names, and unknown
/// spellings, naming the valid ones.
#[test]
fn toolset_selection_rejects_unknown_and_empty_names() {
    use argosy::mcp::Toolsets;

    let err = Toolsets::from_names(&["knowledge".to_string(), "nope".to_string()]).unwrap_err();
    assert!(
        err.to_string().contains("unknown toolset `nope`"),
        "got {err}"
    );
    let err = Toolsets::from_names(&[]).unwrap_err();
    assert!(
        err.to_string().contains("at least one toolset"),
        "got {err}"
    );
    let err = Toolsets::from_names(&["".to_string()]).unwrap_err();
    assert!(err.to_string().contains("unknown toolset"), "got {err}");
    // Case-sensitive: `Knowledge` is not a spelling.
    let err = Toolsets::from_names(&["Knowledge".to_string()]).unwrap_err();
    assert!(
        err.to_string().contains("unknown toolset `Knowledge`"),
        "got {err}"
    );
    // Repeats collapse; the names come back in canonical order.
    let sets = Toolsets::from_names(&[
        "code".to_string(),
        "knowledge".to_string(),
        "code".to_string(),
    ])
    .unwrap();
    assert_eq!(sets.names(), vec!["knowledge", "code"]);
    assert!(!sets.is_all());
    assert!(Toolsets::all().is_all());
}

/// Every tool declares an object `outputSchema` with properties for its
/// `structuredContent`.
#[test]
fn every_tool_declares_an_object_output_schema() {
    for tool in argosy::mcp::tool_definitions() {
        let schema = tool
            .output_schema
            .as_ref()
            .unwrap_or_else(|| panic!("tool `{}` declares no output schema", tool.name));
        assert_eq!(
            schema.get("type").and_then(serde_json::Value::as_str),
            Some("object"),
            "tool `{}`: output schema must be an object",
            tool.name
        );
        assert!(
            schema
                .get("properties")
                .and_then(serde_json::Value::as_object)
                .is_some_and(|p| !p.is_empty()),
            "tool `{}`: output schema lists no properties",
            tool.name
        );
    }
}
