//! Validation findings and the report that carries them.

use std::fmt;
use std::path::PathBuf;

use serde::Serialize;

/// How serious a validation issue is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Worth surfacing; has no bearing on conformance.
    Info,
    /// A recommendation is unmet (e.g. missing `okf_version`).
    Warning,
    /// A hard requirement is violated; blocks conformance.
    Error,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Info => "INFO",
            Self::Warning => "WARNING",
            Self::Error => "ERROR",
        })
    }
}

/// One issue found while validating a bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    /// How serious the issue is (drives [`ValidationReport::is_conformant`]).
    pub severity: Severity,
    /// The requirement ID this finding evidences, when one maps to it.
    pub id: Option<&'static str>,
    /// Bundle-relative path concerned, when the finding is about a file.
    pub path: Option<PathBuf>,
    /// What is wrong, in prose.
    pub message: String,
}

impl Finding {
    pub(crate) fn new(
        severity: Severity,
        id: Option<&'static str>,
        path: Option<PathBuf>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity,
            id,
            path,
            message: message.into(),
        }
    }
}

/// The outcome of structurally validating a bundle: structural requirements,
/// generic `type` conformance under reserved namespaces, and the
/// `skill`/`styleguide` contracts. Deeper OKF conformance (link integrity,
/// listing contents) is out of scope.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ValidationReport {
    findings: Vec<Finding>,
}

impl ValidationReport {
    /// Builds a report directly from findings — how callers holding a
    /// `Vec<Finding>` ([`Argosy::validate_skills`],
    /// [`Argosy::validate_styleguide`]) produce the same report shape as
    /// [`Argosy::validate`] for JSON output and exit-code logic.
    pub fn from_findings(findings: Vec<Finding>) -> Self {
        Self { findings }
    }

    pub(crate) fn push(&mut self, finding: Finding) {
        self.findings.push(finding);
    }

    /// All findings, in the deterministic order they were produced.
    pub fn findings(&self) -> &[Finding] {
        &self.findings
    }

    /// True iff no [`Severity::Error`] findings were produced.
    pub fn is_conformant(&self) -> bool {
        !self.findings.iter().any(|f| f.severity == Severity::Error)
    }

    /// Findings with [`Severity::Error`].
    pub fn errors(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Error)
    }

    /// Findings with [`Severity::Warning`].
    pub fn warnings(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Warning)
    }
}

impl fmt::Display for ValidationReport {
    /// One line per finding: `[ERROR STR-4] path/to/file.md: message`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for finding in &self.findings {
            let id = finding.id.map(|id| format!(" {id}")).unwrap_or_default();
            let path = finding
                .path
                .as_ref()
                .map(|p| format!("{}: ", p.display()))
                .unwrap_or_default();
            writeln!(f, "[{}{id}] {path}{}", finding.severity, finding.message)?;
        }
        Ok(())
    }
}
