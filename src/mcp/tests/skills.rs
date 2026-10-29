//! Trust surfacing: skill listing and precedence-resolved reads.

use super::*;
use crate::trust::TrustTier;

/// A rig whose user configuration grants `acme-shared` exactly the tier
/// its fixture skill claims.
fn trusted_rig() -> Rig {
    rig_with_trust(crate::trust::TrustPolicy::from_caps(
        [("acme-shared".to_string(), TrustTier::MachineConfirmed)]
            .into_iter()
            .collect(),
    ))
}

#[test]
fn list_skills_surfaces_origin_trust_tier_and_shadowing() {
    let mut rig = trusted_rig();
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
    // 2 fixture skills + imported shared-audit + the local shadow.
    assert_eq!(report.skills.len(), 4);
    let shared: Vec<_> = report
        .skills
        .iter()
        .filter(|s| s.name == "shared-audit")
        .collect();
    assert_eq!(shared.len(), 2, "both listings appear, shadowed flagged");
    let local = shared.iter().find(|s| s.argosy == "acme-billing").unwrap();
    assert!(!local.shadowed);
    assert_eq!(local.kind, "local");
    assert_eq!(local.verified, "unverified", "no verified entry");
    let imported = shared.iter().find(|s| s.argosy == "acme-shared").unwrap();
    assert!(imported.shadowed, "local shadows the import");
    assert_eq!(imported.kind, "imported");
    assert_eq!(imported.verified, "machine-confirmed");
    assert!(shared.iter().all(|s| !s.description.is_empty()));
}

#[test]
fn get_skill_resolves_by_precedence_and_errors_on_unknown() {
    let mut rig = trusted_rig();
    let out = rig.state.get_skill(GetSkillParams {
        cwd: project(),
        name: "shared-audit".to_string(),
    });
    // No local override yet: the import wins.
    let out = match out {
        Ok(out) => out,
        Err(e) => panic!("unexpected: {e}"),
    };
    assert_eq!(out.skill.argosy, "acme-shared");
    assert_eq!(out.skill.kind, "imported");
    assert_eq!(out.skill.verified, "machine-confirmed");
    // The body arrives in untrusted-content delimiters — data to bracket, not to follow.
    assert!(
        out.content.starts_with(
            "<untrusted-content argosy=\"acme-shared\" \
             uri=\"argosy://acme-shared/skill/shared-audit\" \
             trust=\"machine-confirmed\">"
        ),
        "got {}",
        out.content
    );
    assert!(out.content.ends_with("</untrusted-content>"));
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

// --- Provenance-capped tiers (imported claims are never self-serving) ---

#[test]
fn imported_tier_defaults_to_unverified_without_configured_trust() {
    // No `trust` configuration, so the `machine-confirmed` self-claim must not relay.
    let mut rig = rig();
    let report = rig
        .state
        .list_skills(ListSkillsParams { cwd: project() })
        .unwrap();
    let imported = report
        .skills
        .iter()
        .find(|s| s.argosy == "acme-shared" && s.name == "shared-audit")
        .unwrap();
    assert_eq!(imported.verified, "unverified");
}

#[test]
fn configured_cap_above_the_claim_leaves_the_claim_standing() {
    let mut rig = rig_with_trust(crate::trust::TrustPolicy::from_caps(
        [("acme-shared".to_string(), TrustTier::HumanReviewed)]
            .into_iter()
            .collect(),
    ));
    let out = rig
        .state
        .get_skill(GetSkillParams {
            cwd: project(),
            name: "shared-audit".to_string(),
        })
        .unwrap();
    assert_eq!(out.skill.verified, "machine-confirmed");
}

#[test]
fn configured_cap_below_the_claim_caps_the_reported_tier() {
    let mut rig = rig_with_trust(crate::trust::TrustPolicy::from_caps(
        [("acme-shared".to_string(), TrustTier::Unverified)]
            .into_iter()
            .collect(),
    ));
    let out = rig
        .state
        .get_skill(GetSkillParams {
            cwd: project(),
            name: "shared-audit".to_string(),
        })
        .unwrap();
    assert_eq!(out.skill.verified, "unverified");
}

#[test]
fn local_claims_are_never_capped_by_the_import_policy() {
    // Caps apply to imports only — the local book is the user's own hand.
    let mut rig = rig_with_trust(crate::trust::TrustPolicy::from_caps(
        [("acme-billing".to_string(), TrustTier::Unverified)]
            .into_iter()
            .collect(),
    ));
    let claiming: Concept = ("---\n\
         type: Skill\n\
         description: Locally vouched deploy runbook.\n\
         verified: human-reviewed\n\
         ---\n\
         # Deploy\n\nLocal steps.\n")
        .parse()
        .unwrap();
    rig.state
        .session(project())
        .unwrap()
        .context
        .local()
        .write_concept(
            Namespace::Skill,
            &"skill/local-runbook".parse().unwrap(),
            &claiming,
        )
        .unwrap();

    let report = rig
        .state
        .list_skills(ListSkillsParams { cwd: project() })
        .unwrap();
    let local = report
        .skills
        .iter()
        .find(|s| s.argosy == "acme-billing" && s.name == "local-runbook")
        .unwrap();
    assert_eq!(local.verified, "human-reviewed");
}
