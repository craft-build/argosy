//! Provenance-based trust tiers: the OKF tier a concept *claims* in its
//! `verified` frontmatter is only ever relayed verbatim for the local
//! (writable) argosy. An imported argosy is shared content — it cannot
//! grade its own homework — so its concepts' effective tier is capped at
//! the tier the *importing* user configured for that import
//! ([`TrustPolicy`]), never at the book's self-claim. With no configured
//! cap every import reads as [`UNVERIFIED`].
//!
//! Integrity verification ([`crate::package::validate_integrity`]) is the
//! orthogonal tamper check: a checkout whose sidecar disagrees with its
//! contents is excluded from the context outright (see
//! [`crate::context::ProjectContext`]) rather than downgraded, while a
//! book that ships no sidecar at all is still eligible for a configured
//! cap — the sidecar proves the bytes match what was packaged, not that
//! anyone reviewed them.

use std::collections::HashMap;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::concept::Concept;

/// The tier reported for concepts with no `verified` frontmatter — and the
/// floor of every effective tier (OKF SEC-2's tier vocabulary).
pub const UNVERIFIED: &str = "unverified";

/// The OKF trust tiers, weakest to strongest (the ordering that makes
/// [`TrustPolicy::effective`] a `min`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrustTier {
    /// No review signal at all — the default for every import without a
    /// configured cap.
    #[default]
    Unverified,
    /// A machine check passed (lint, tests, provenance tooling).
    MachineConfirmed,
    /// A person read the content and vouched for it.
    HumanReviewed,
}

impl TrustTier {
    /// The frontmatter/config spelling of this tier.
    pub fn as_str(&self) -> &'static str {
        match self {
            TrustTier::Unverified => UNVERIFIED,
            TrustTier::MachineConfirmed => "machine-confirmed",
            TrustTier::HumanReviewed => "human-reviewed",
        }
    }

    /// Parses a tier from its frontmatter/config spelling; `None` for
    /// anything else (an unrecognized `verified` value carries no trust
    /// signal to relay).
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            UNVERIFIED => Some(TrustTier::Unverified),
            "machine-confirmed" => Some(TrustTier::MachineConfirmed),
            "human-reviewed" => Some(TrustTier::HumanReviewed),
            _ => None,
        }
    }
}

impl FromStr for TrustTier {
    type Err = crate::error::Error;

    fn from_str(text: &str) -> crate::error::Result<Self> {
        Self::parse(text).ok_or_else(|| crate::error::Error::Validation {
            reason: format!(
                "unknown trust tier `{text}` (expected one of: {UNVERIFIED}, \
                 machine-confirmed, human-reviewed)"
            ),
        })
    }
}

/// Which argosy a concept comes from, for [`TrustPolicy::effective`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The local, writable argosy: its own claims are relayed verbatim.
    Local,
    /// An imported, read-only argosy: claims are capped by configuration.
    Imported,
}

/// The importing user's trust decisions: one maximum tier per imported
/// argosy, keyed by manifest name (the `argosy://` URI spelling). Derived
/// from the user configuration's `trust` section; an import with no entry
/// is capped at [`TrustTier::Unverified`] whatever it claims.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustPolicy {
    caps: HashMap<String, TrustTier>,
}

impl TrustPolicy {
    /// The empty policy: every import is `unverified`, the local argosy is
    /// unaffected.
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds the policy from the user configuration's caps (manifest name
    /// → maximum tier for that import).
    pub fn from_caps(caps: HashMap<String, TrustTier>) -> Self {
        Self { caps }
    }

    /// The maximum tier an import of this manifest name may carry.
    pub fn cap(&self, argosy: &str) -> TrustTier {
        self.caps.get(argosy).copied().unwrap_or_default()
    }

    /// The effective tier for one concept's `verified` claim:
    ///
    /// - [`Origin::Local`]: the claim verbatim (or [`UNVERIFIED`] when
    ///   absent) — the local book is the user's own writing;
    /// - [`Origin::Imported`]: `min(claim, configured cap)`, where an
    ///   absent or unrecognized claim counts as [`TrustTier::Unverified`].
    pub fn effective(&self, origin: Origin, argosy: &str, claim: Option<&str>) -> String {
        match origin {
            Origin::Local => claim.unwrap_or(UNVERIFIED).trim().to_string(),
            Origin::Imported => {
                let claim = claim.and_then(TrustTier::parse).unwrap_or_default();
                let tier = claim.min(self.cap(argosy));
                tier.as_str().to_string()
            }
        }
    }

    /// [`Self::effective`] over a concept's `verified` frontmatter.
    pub fn effective_for(&self, origin: Origin, argosy: &str, concept: &Concept) -> String {
        self.effective(origin, argosy, concept.get_str("verified"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(caps: &[(&str, TrustTier)]) -> TrustPolicy {
        TrustPolicy::from_caps(
            caps.iter()
                .map(|(name, tier)| (name.to_string(), *tier))
                .collect(),
        )
    }

    #[test]
    fn tiers_order_weakest_to_strongest() {
        assert!(TrustTier::Unverified < TrustTier::MachineConfirmed);
        assert!(TrustTier::MachineConfirmed < TrustTier::HumanReviewed);
    }

    #[test]
    fn tier_strings_round_trip_through_parse() {
        for tier in [
            TrustTier::Unverified,
            TrustTier::MachineConfirmed,
            TrustTier::HumanReviewed,
        ] {
            assert_eq!(TrustTier::parse(tier.as_str()), Some(tier));
            assert_eq!(tier.as_str().parse::<TrustTier>().unwrap(), tier);
        }
        // Whitespace-tolerant, casing-strict: an unknown spelling is not a
        // trust signal.
        assert_eq!(
            TrustTier::parse("  human-reviewed "),
            Some(TrustTier::HumanReviewed)
        );
        assert_eq!(TrustTier::parse("Human-Reviewed"), None);
        assert_eq!(TrustTier::parse("totally-legit"), None);
        let err = "totally-legit".parse::<TrustTier>().unwrap_err();
        assert!(err.to_string().contains("unknown trust tier"), "{err}");
    }

    #[test]
    fn local_claims_are_relayed_verbatim_regardless_of_caps() {
        let strict = TrustPolicy::new();
        let granting = policy(&[("local", TrustTier::Unverified)]);
        for p in [&strict, &granting] {
            assert_eq!(
                p.effective(Origin::Local, "local", Some("human-reviewed")),
                "human-reviewed"
            );
            assert_eq!(p.effective(Origin::Local, "local", None), UNVERIFIED);
            // An unconventional local claim is passed through untouched:
            // local frontmatter is the user's own hand.
            assert_eq!(
                p.effective(Origin::Local, "local", Some("audited 2026-01")),
                "audited 2026-01"
            );
        }
    }

    #[test]
    fn imported_claims_are_capped_by_the_configured_tier() {
        let p = policy(&[("acme-shared", TrustTier::MachineConfirmed)]);
        // Claim above the cap: capped.
        assert_eq!(
            p.effective(Origin::Imported, "acme-shared", Some("human-reviewed")),
            "machine-confirmed"
        );
        // Claim at the cap: stands.
        assert_eq!(
            p.effective(Origin::Imported, "acme-shared", Some("machine-confirmed")),
            "machine-confirmed"
        );
        // Claim below the cap: stands.
        let elevated = policy(&[("acme-shared", TrustTier::HumanReviewed)]);
        assert_eq!(
            elevated.effective(Origin::Imported, "acme-shared", Some("machine-confirmed")),
            "machine-confirmed"
        );
    }

    #[test]
    fn imports_without_a_configured_cap_are_unverified_whatever_they_claim() {
        let p = TrustPolicy::new();
        assert_eq!(
            p.effective(Origin::Imported, "acme-shared", Some("human-reviewed")),
            UNVERIFIED
        );
        assert_eq!(
            p.effective(Origin::Imported, "acme-shared", None),
            UNVERIFIED
        );
        // A cap for one import never leaks to another.
        let p = policy(&[("other", TrustTier::HumanReviewed)]);
        assert_eq!(
            p.effective(Origin::Imported, "acme-shared", Some("human-reviewed")),
            UNVERIFIED
        );
    }

    #[test]
    fn unrecognized_imported_claims_are_not_relayed() {
        let p = policy(&[("acme-shared", TrustTier::HumanReviewed)]);
        assert_eq!(
            p.effective(Origin::Imported, "acme-shared", Some("trust-me")),
            UNVERIFIED,
            "a self-invented tier string is not a signal"
        );
    }
}
