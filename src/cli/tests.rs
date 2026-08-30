//! CLI parse and dispatch tests.

use super::*;
use argosy::Namespace;

fn parse_query(argv: &[&str]) -> QueryArgs {
    let cli = Cli::try_parse_from(argv).expect("argv parses");
    let Command::Index(IndexArgs {
        verb: IndexVerb::Query(q),
        ..
    }) = cli.command
    else {
        panic!("expected `index query` argv, got a different command");
    };
    q
}

#[test]
fn query_flags_map_to_filter_fields() {
    let q = parse_query(&[
        "argosy",
        "index",
        "query",
        "oauth refresh tokens",
        "-k",
        "3",
        "--namespace",
        "skill",
        "--namespace",
        "skill",
        "--namespace",
        "styleguide",
        "--argosy",
        "acme-billing",
        "--language",
        "rust",
        "--category",
        "naming",
        "--tag",
        "auth",
        "--tag",
        "api",
        "--type",
        "Styleguide Rule",
    ]);
    let filter = build_filter(&q);
    assert_eq!(q.k, 3);
    assert_eq!(
        filter.namespaces,
        Some(vec![
            Namespace::Skill,
            Namespace::Skill,
            Namespace::Styleguide
        ])
    );
    assert_eq!(
        filter.argosies.as_deref(),
        Some(&["acme-billing".to_string()][..])
    );
    assert_eq!(filter.language.as_deref(), Some("rust"));
    assert_eq!(filter.category.as_deref(), Some("naming"));
    assert_eq!(
        filter.tags,
        Some(vec!["auth".to_string(), "api".to_string()])
    );
    assert_eq!(
        filter.concept_types,
        Some(vec!["Styleguide Rule".to_string()])
    );
}

#[test]
fn unscoped_query_leaves_every_filter_field_none() {
    let q = parse_query(&["argosy", "index", "query", "anything"]);
    assert_eq!(q.k, 5, "default k");
    let filter = build_filter(&q);
    // 1:1 flag mapping: no flags means no constraints anywhere.
    assert!(filter.namespaces.is_none());
    assert!(filter.argosies.is_none());
    assert!(filter.concept_types.is_none());
    assert!(filter.tags.is_none());
    assert!(filter.language.is_none());
    assert!(filter.category.is_none());
}

#[cfg(test)]
mod mcp_parse_tests {
    use super::*;

    #[test]
    fn mcp_parses_with_no_flags_and_stdio_is_the_only_transport() {
        let cli = Cli::try_parse_from(["argosy", "mcp"]).expect("argv parses");
        let Command::Mcp(_args) = cli.command else {
            panic!("expected mcp argv");
        };

        // The HTTP transport was removed (unauthenticated network exposure
        // of destructive tools): neither flag may parse at all.
        for flag in ["--transport", "--bind"] {
            assert!(
                Cli::try_parse_from(["argosy", "mcp", flag, "x"]).is_err(),
                "`{flag}` must not parse"
            );
        }
    }

    #[test]
    fn mcp_takes_no_argosy_selection_flags() {
        // Membership comes from `.argosy/` + global-store discovery, not from
        // a hand-maintained flag list: neither flag may parse at all.
        for flag in ["--project-root", "--import"] {
            assert!(
                Cli::try_parse_from(["argosy", "mcp", flag, "x"]).is_err(),
                "`{flag}` must not parse"
            );
        }
    }
}

#[cfg(test)]
mod agent_parse_tests {
    use super::*;

    #[test]
    fn agent_reviewer_parses_each_harness_and_force_is_opt_in() {
        for (argv, expected) in [
            (
                &["argosy", "agent", "reviewer", "opencode"][..],
                HarnessOpt::OpenCode,
            ),
            (
                &["argosy", "agent", "reviewer", "claude"][..],
                HarnessOpt::Claude,
            ),
            (
                &["argosy", "agent", "reviewer", "kiro-cli"][..],
                HarnessOpt::KiroCli,
            ),
        ] {
            let cli = Cli::try_parse_from(argv).expect("argv parses");
            let Command::Agent(AgentArgs {
                verb: AgentVerb::Reviewer(ReviewerArgs { harness, force }),
            }) = cli.command
            else {
                panic!("expected `agent reviewer` argv, got another command");
            };
            assert_eq!(harness, expected);
            assert!(!force, "force is opt-in");
        }

        let cli =
            Cli::try_parse_from(["argosy", "agent", "reviewer", "claude", "--force"]).unwrap();
        let Command::Agent(AgentArgs {
            verb: AgentVerb::Reviewer(ReviewerArgs { force, .. }),
        }) = cli.command
        else {
            panic!("expected `agent reviewer` argv, got another command");
        };
        assert!(force);
    }

    #[test]
    fn agent_reviewer_rejects_misspelled_harnesses() {
        // clap's default kebab-casing would spell OpenCode `open-code`;
        // the explicit value names keep the documented spellings only.
        for bad in ["cursor", "open-code", "kiro", "claude-code"] {
            assert!(
                Cli::try_parse_from(["argosy", "agent", "reviewer", bad]).is_err(),
                "`{bad}` must not parse"
            );
        }
    }
}
