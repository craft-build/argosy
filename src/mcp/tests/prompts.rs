//! The advertised prompt workflows.

use super::*;

#[test]
fn prompt_definitions_list_exactly_dream_and_scan_with_llm_descriptions() {
    let prompts = prompt_definitions();
    let names: Vec<_> = prompts.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["dream", "scan"], "exactly the documented set");
    for prompt in &prompts {
        assert!(
            prompt.description.as_deref().is_some_and(|d| d.len() > 40),
            "prompt `{}` needs a real description",
            prompt.name
        );
        // Neither workflow takes arguments (craft's /dream is max_args 0).
        assert!(prompt.arguments.is_none());
    }
}

#[test]
fn get_prompt_result_returns_one_user_message_naming_the_workflow_tools() {
    let result = get_prompt_result("dream").expect("dream resolves");
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
    let result = get_prompt_result("scan").expect("scan resolves");
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
fn get_prompt_result_unknown_name_is_none() {
    assert!(get_prompt_result("nope").is_none());
}
