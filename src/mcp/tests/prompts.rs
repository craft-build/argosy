//! The advertised prompt workflows.

use super::*;

#[test]
fn prompt_definitions_list_all_workflows_with_llm_descriptions() {
    let prompts = prompt_definitions();
    let names: Vec<_> = prompts.iter().map(|p| p.name.as_str()).collect();
    #[cfg(feature = "code-tools")]
    assert_eq!(
        names,
        ["dream", "scan", "review"],
        "exactly the documented set"
    );
    #[cfg(not(feature = "code-tools"))]
    assert_eq!(names, ["dream", "scan"], "exactly the documented set");
    for prompt in &prompts {
        assert!(
            prompt.description.as_deref().is_some_and(|d| d.len() > 40),
            "prompt `{}` needs a real description",
            prompt.name
        );
    }
    assert!(prompts[0].arguments.is_none());
    assert!(prompts[1].arguments.is_none());
    #[cfg(feature = "code-tools")]
    {
        let review_args = prompts[2]
            .arguments
            .as_ref()
            .expect("review takes arguments");
        assert_eq!(
            review_args
                .iter()
                .map(|argument| argument.name.as_str())
                .collect::<Vec<_>>(),
            ["cwd", "base", "commit", "focus"]
        );
        assert_eq!(review_args[0].required, Some(true));
    }
}

#[test]
fn get_prompt_result_returns_one_user_message_naming_the_workflow_tools() {
    let result = get_prompt_result("dream", None)
        .expect("valid prompt")
        .expect("dream resolves");
    assert_eq!(
        result.description.as_deref(),
        prompt_definitions()[0].description.as_deref()
    );
    assert_eq!(result.messages.len(), 1);
    let message = &result.messages[0];
    assert_eq!(message.role, rmcp::model::Role::User);
    match &message.content {
        rmcp::model::ContentBlock::Text(text) => {
            // Self-contained: every tool the workflow drives is named.
            for tool in ["search", "read_memory", "write_memory", "delete_memory"] {
                assert!(text.text.contains(tool), "dream prompt must name `{tool}`");
            }
            assert!(
                text.text.contains("no-op is a valid outcome"),
                "the summary/no-op rule survives"
            );
            assert!(
                text.text.contains("read-only"),
                "imported-argosys read-only rule present"
            );
        }
        other => panic!("expected text content, got {other:?}"),
    }
}

#[test]
fn get_prompt_result_scan_names_the_core_documents_and_write_tools() {
    let result = get_prompt_result("scan", None)
        .expect("valid prompt")
        .expect("scan resolves");
    assert_eq!(
        result.description.as_deref(),
        prompt_definitions()
            .into_iter()
            .find(|p| p.name == "scan")
            .expect("scan is listed")
            .description
            .as_deref()
    );
    assert_eq!(result.messages.len(), 1);
    let message = &result.messages[0];
    assert_eq!(message.role, rmcp::model::Role::User);
    match &message.content {
        rmcp::model::ContentBlock::Text(text) => {
            // Self-contained: every tool the workflow drives is named.
            for tool in ["search", "read_memory", "write_document", "delete_document"] {
                assert!(text.text.contains(tool), "scan prompt must name `{tool}`");
            }
            // The core document set the workflow promises.
            for path in [
                "document/summary",
                "document/architecture",
                "document/tech",
                "document/development",
            ] {
                assert!(text.text.contains(path), "scan prompt must name `{path}`");
            }
            assert!(
                text.text.contains("frontmatter"),
                "the DOC-1 frontmatter `type` requirement is taught"
            );
            assert!(
                text.text.contains("read-only"),
                "imported-argosys read-only rule present"
            );
        }
        other => panic!("expected text content, got {other:?}"),
    }
}

#[test]
#[cfg(feature = "code-tools")]
fn get_prompt_result_review_interpolates_scope_and_teaches_structured_process() {
    let arguments = serde_json::json!({
        "cwd": "/work/acme",
        "commit": "abc123",
        "focus": "authentication regressions",
    });
    let result = get_prompt_result("review", arguments.as_object())
        .expect("valid prompt")
        .expect("review resolves");
    assert_eq!(
        result.description.as_deref(),
        prompt_definitions()[2].description.as_deref()
    );
    let rmcp::model::ContentBlock::Text(text) = &result.messages[0].content else {
        panic!("expected text content");
    };
    for required in [
        "/work/acme",
        "abc123",
        "authentication regressions",
        "open_review",
        "review_diff",
        "search_rules",
        "report_finding",
        "review_findings",
        "concrete failure scenario",
        "request_changes",
    ] {
        assert!(
            text.text.contains(required),
            "review prompt must contain `{required}`"
        );
    }
}

#[test]
#[cfg(feature = "code-tools")]
fn get_prompt_result_review_validates_arguments() {
    assert!(get_prompt_result("review", None).is_err());
    let conflicting = serde_json::json!({
        "cwd": "/work/acme",
        "base": "main",
        "commit": "abc123",
    });
    assert!(get_prompt_result("review", conflicting.as_object()).is_err());
}

#[test]
fn get_prompt_result_unknown_name_is_none() {
    assert!(
        get_prompt_result("nope", None)
            .expect("unknown names are not argument errors")
            .is_none()
    );
}
