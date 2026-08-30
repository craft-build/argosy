//! Trust surfacing: skill listing and precedence-resolved reads.

use super::*;

#[test]
fn list_skills_surfaces_origin_trust_tier_and_shadowing() {
    let mut rig = rig();
    // A local skill shadowing the imported one by name.
    let shadow: Concept = ("---\n\
         type: Skill\n\
         description: Local override of the shared audit.\n\
         ---\n\
         # Audit\n\nLocal steps.\n")
        .parse()
        .unwrap();
    rig.state
        .session(project())
        .unwrap()
        .context
        .local()
        .write_concept(
            Namespace::Skill,
            &"skill/shared-audit".parse().unwrap(),
            &shadow,
        )
        .unwrap();

    let report = rig
        .state
        .list_skills(ListSkillsParams { cwd: project() })
        .unwrap();
    // 2 fixture skills + imported shared-audit + the local shadow just written.
    assert_eq!(report.skills.len(), 4);
    let shared: Vec<_> = report
        .skills
        .iter()
        .filter(|s| s.name == "shared-audit")
        .collect();
    assert_eq!(shared.len(), 2, "both listings appear, shadowed flagged");
    let local = shared.iter().find(|s| s.argosy == "acme-billing").unwrap();
    assert!(!local.shadowed);
    assert_eq!(local.verified, "unverified", "no verified entry");
    let imported = shared.iter().find(|s| s.argosy == "acme-shared").unwrap();
    assert!(imported.shadowed, "local shadows the import");
    assert_eq!(imported.verified, "machine-confirmed");
    assert!(shared.iter().all(|s| !s.description.is_empty()));
}

#[test]
fn get_skill_resolves_by_precedence_and_errors_on_unknown() {
    let mut rig = rig();
    let out = rig.state.get_skill(GetSkillParams {
        cwd: project(),
        name: "shared-audit".to_string(),
    });
    // No local override yet: the import wins and carries its tier.
    let out = match out {
        Ok(out) => out,
        Err(e) => panic!("unexpected: {e}"),
    };
    assert_eq!(out.skill.argosy, "acme-shared");
    assert_eq!(out.skill.verified, "machine-confirmed");
    assert!(out.content.contains("verified: machine-confirmed"));

    rig.state
        .session(project())
        .unwrap()
        .context
        .local()
        .write_concept(
            Namespace::Skill,
            &"skill/shared-audit".parse().unwrap(),
            &("---\ntype: Skill\ndescription: local\n---\n# A\n"
                .parse::<Concept>()
                .unwrap()),
        )
        .unwrap();
    let out = rig
        .state
        .get_skill(GetSkillParams {
            cwd: project(),
            name: "shared-audit".to_string(),
        })
        .unwrap();
    assert_eq!(out.skill.argosy, "acme-billing", "local wins precedence");

    let err = rig
        .state
        .get_skill(GetSkillParams {
            cwd: project(),
            name: "nope".to_string(),
        })
        .unwrap_err();
    assert!(matches!(err, Error::ConceptNotFound { .. }), "got {err:?}");
}
