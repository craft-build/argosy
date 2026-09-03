//! Tool-schema tests.

/// Imported argosys are read-only *structurally* — no
/// mutating tool may expose an `argosy` selector. If a future tool violates
/// this, the invariant breaks here, not in a runtime exploit.
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
        "open_review",
        "review_diff",
        "report_finding",
        "review_findings",
        "review_status",
    ] {
        assert!(names.contains(&expected), "missing tool `{expected}`");
    }
    #[cfg(feature = "code-tools")]
    let expected_total = 25;
    #[cfg(not(feature = "code-tools"))]
    let expected_total = 13;
    assert_eq!(
        tools.len(),
        expected_total,
        "exactly the documented tool set"
    );

    for tool in &tools {
        let props = tool.input_schema["properties"]
            .as_object()
            .cloned()
            .unwrap_or_default();
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
        // Every tool carries an LLM-facing description.
        assert!(
            tool.description.as_deref().is_some_and(|d| d.len() > 40),
            "tool `{}` needs a real description",
            tool.name
        );
    }
}
