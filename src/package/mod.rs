//! Distribution packaging, bundle integrity, and Craft YAML styleguide
//! import. [`package`] copies an [`Argosy`] to a directory or gzipped
//! tarball: `memory/` is excluded unconditionally, `.argosy/` only under
//! [`PackageOptions::include_index`]. Every copy emits a verified sidecar.
//! [`package`] operates on a [`crate::bundle::Argosy`].

mod archive;
mod import;
mod payload;

#[cfg(test)]
mod tests;

pub use archive::{bundle_content_hash, package, validate_integrity};
pub use import::{ImportReport, import_styleguide_yaml};

/// The integrity sidecar every package emits.
pub const INTEGRITY_FILENAME: &str = "argosy-integrity.txt";

/// How [`package`] materializes the distributable bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PackageFormat {
    /// A plain directory tree — the artifact you would commit to git.
    #[default]
    Directory,
    /// A gzipped tar archive (`.tar.gz`).
    TarGz,
}

/// Knobs for [`package`].
#[derive(Debug, Clone, Default)]
pub struct PackageOptions {
    /// Ship `.argosy/` as a precomputed index cache. Off by default: the
    /// index is derivative and rebuildable.
    pub include_index: bool,
    /// The materialization format.
    pub format: PackageFormat,
}

/// The outcome of a [`package`] run.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PackageReport {
    /// Manifest name of the packaged argosy (printed by the CLI).
    pub name: String,
    /// Manifest `argosy_version` of the packaged argosy.
    pub argosy_version: semver::Version,
    /// Bundle files copied (excluding the integrity sidecar).
    pub files_copied: usize,
    /// True iff a root `memory/` directory existed at the source and was
    /// excluded.
    pub memory_excluded: bool,
    /// Non-fatal observations; always carries a warning when
    /// [`PackageReport::memory_excluded`] is true.
    pub warnings: Vec<String>,
}
