//! The global catalog: one read-only, machine- and human-readable index of
//! every project slot under the user's argosy state dir.
//!
//! Argosy state lives outside project trees (`<state>/projects/<slug>/…`;
//! see [`crate::pull`]), so with many repositories and worktrees it is hard
//! to see what exists, what an installation holds, and what has gone stale.
//! [`build`] scans the state dir on demand and returns a [`Catalog`]
//! describing each slot: its canonical project root (recorded when the slot
//! was created — see [`crate::pull::record_project_root`]), the local
//! (writable) bundle and read-only imports, the active global argosies,
//! per-namespace content counts, semantic-index status, duplicate concept
//! ids across the slot's active bundles, and whether the original project
//! directory still exists.
//!
//! The catalog is regenerated, never hand-edited: `argosy catalog --write`
//! materializes [`render_markdown`] at `<state>/README.md`, and the MCP
//! server serves the same text as the `argosy://catalog` resource. Because
//! that file lives at the state root — outside every bundle — packaging
//! never includes it.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::Serialize;
use snafu::ResultExt;

use crate::bundle::{Argosy, Namespace};
use crate::concept::ConceptId;
use crate::error::{IoSnafu, Result};
use crate::pull;
use crate::skill::Skill;
use crate::styleguide::StyleguideRule;

/// The catalog file name under the state root: `<state>/README.md`.
pub const CATALOG_FILE: &str = "README.md";

/// Tunables for [`build`]: the expected embedding-model identity (so index
/// status can flag a model mismatch) and the configured index file name.
#[derive(Debug, Clone, Default)]
pub struct CatalogOptions {
    /// The model identity the current configuration expects. `None` reports
    /// a store's recorded model without judging it.
    pub expected_model_id: Option<String>,
    /// The index file name (default: [`pull::INDEX_DB_NAME`]).
    pub index_db_name: Option<String>,
}

impl CatalogOptions {
    /// Derives options from loaded user configuration.
    pub fn from_config(config: &crate::config::Config) -> Self {
        Self {
            #[cfg(feature = "default-index")]
            expected_model_id: crate::index::tract::ModelSpec::from_name(config.model_name())
                .map(|spec| spec.model_id()),
            #[cfg(not(feature = "default-index"))]
            expected_model_id: None,
            index_db_name: Some(config.index_db_name().to_string()),
        }
    }
}

/// The generated global catalog.
#[derive(Debug, Clone, Serialize)]
pub struct Catalog {
    /// The state root the catalog describes.
    pub state_root: PathBuf,
    /// One entry per project slot, sorted by slug.
    pub projects: Vec<ProjectEntry>,
}

/// One project slot under `<state>/projects/`.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectEntry {
    /// The slot directory name, e.g. `craft-1a2b3c4d`.
    pub slug: String,
    /// The path-hash suffix of the slug (`1a2b3c4d`), or empty when the slug
    /// does not end in one.
    pub hash: String,
    /// The canonical project root recorded when the slot was created, if
    /// any (slots predating root recording have none).
    pub root: Option<PathBuf>,
    /// Whether [`ProjectEntry::root`] still exists on disk; `None` when no
    /// root was recorded.
    pub root_exists: Option<bool>,
    /// True iff a root was recorded and its directory is gone — the slot is
    /// a cleanup candidate.
    pub stale: bool,
    /// The writable local (`default`) bundle, when one is present.
    pub local: Option<BundleInfo>,
    /// The slot's read-only imported argosies, sorted by name.
    pub imports: Vec<BundleInfo>,
    /// The active global (read-only) argosies, shared by every project.
    pub globals: Vec<BundleInfo>,
    /// Content counts across the local bundle, imports, and globals.
    pub contents: NamespaceCounts,
    /// The semantic index status for this slot.
    pub index: IndexStatus,
    /// Concept ids defined by more than one active bundle in this slot.
    pub duplicate_concepts: Vec<DuplicateConcept>,
}

/// A single argosy bundle appearing in the catalog.
#[derive(Debug, Clone, Serialize)]
pub struct BundleInfo {
    /// The manifest name (the `argosy://` identity).
    pub name: String,
    /// The manifest description, when set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The bundle's root directory.
    pub root: PathBuf,
    /// True for the writable local bundle; false for a read-only import.
    pub writable: bool,
    /// The bundle's own content version.
    pub argosy_version: String,
    /// Per-namespace content counts for this bundle.
    pub contents: NamespaceCounts,
}

/// Per-namespace concept counts. Skills and styleguide rules are counted by
/// their contract-satisfying entries ([`Skill::list`],
/// [`StyleguideRule::list`]); documents and memories count concept files.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct NamespaceCounts {
    /// Concepts under `document/`.
    pub documents: usize,
    /// Concepts under `memory/`.
    pub memories: usize,
    /// Contract-satisfying entries under `skill/`.
    pub skills: usize,
    /// Contract-satisfying rules under `styleguide/`.
    pub styleguides: usize,
}

impl NamespaceCounts {
    /// Merges `other` into `self` (component-wise addition).
    fn add(&mut self, other: &NamespaceCounts) {
        self.documents += other.documents;
        self.memories += other.memories;
        self.skills += other.skills;
        self.styleguides += other.styleguides;
    }
}

/// A concept id defined by more than one active bundle in a project.
#[derive(Debug, Clone, Serialize)]
pub struct DuplicateConcept {
    /// The shared bundle-relative concept id (namespace prefix included).
    pub id: String,
    /// The manifest names of the bundles defining it, in precedence order.
    pub argosies: Vec<String>,
}

/// Semantic-index status for a project slot.
#[derive(Debug, Clone, Default, Serialize)]
pub struct IndexStatus {
    /// Whether an index database exists for the slot.
    pub present: bool,
    /// The index database path, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub db: Option<PathBuf>,
    /// The model identity recorded in the store, if readable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    /// The model identity the current configuration expects, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_model_id: Option<String>,
    /// The index database's last-modified time, as seconds since the Unix
    /// epoch (the closest thing to a "last indexed" time on disk).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_indexed_secs: Option<u64>,
    /// What the next `index build` would change, when it could be computed
    /// read-only (requires the `default-index` feature).
    #[cfg(feature = "default-index")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub staleness: Option<crate::index::StalenessReport>,
}

impl IndexStatus {
    /// A short human label for the index state.
    fn label(&self) -> String {
        if !self.present {
            return "absent".to_string();
        }
        #[cfg(feature = "default-index")]
        if let Some(stale) = &self.staleness {
            if stale.model_mismatch {
                return "stale (model changed — `index build` rebuilds)".to_string();
            }
            let drift = stale.added + stale.changed + stale.removed;
            if drift == 0 {
                return format!("current ({} unchanged)", stale.unchanged);
            }
            return format!(
                "stale (+{} ~{} -{}; {} unchanged)",
                stale.added, stale.changed, stale.removed, stale.unchanged
            );
        }
        "present".to_string()
    }
}

/// A scanned bundle plus the concept ids it defines, kept during build for
/// duplicate detection and not serialized.
struct ScannedBundle {
    info: BundleInfo,
    ids: Vec<ConceptId>,
}

impl ScannedBundle {
    fn name(&self) -> &str {
        &self.info.name
    }
}

/// Scans `state_root` and returns the catalog. Read-only: it opens bundles
/// and (with the `default-index` feature) index databases read-only, and
/// never writes. A malformed bundle or index is skipped rather than failing
/// the whole catalog.
pub fn build(state_root: &Path, options: &CatalogOptions) -> Result<Catalog> {
    let globals = scan_bundles(&state_root.join("global"), None);

    let mut projects = Vec::new();
    for slot in list_slots(state_root) {
        projects.push(build_entry(&slot, &globals, options));
    }
    Ok(Catalog {
        state_root: state_root.to_path_buf(),
        projects,
    })
}

/// Builds one project entry from its slot directory.
fn build_entry(slot: &Path, globals: &[ScannedBundle], options: &CatalogOptions) -> ProjectEntry {
    let slug = slot
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let root = pull::recorded_project_root(slot);
    let root_exists = root.as_ref().map(|p| p.is_dir());
    let stale = root_exists == Some(false);

    // With a recorded (canonical) root the slug's hash suffix is checked
    // against the root's actual path hash, so a project merely *named* like
    // `…-deadbeef` is not misread as carrying a hash. Without a sidecar the
    // textual shape is the best available guess.
    let hash = match &root {
        Some(root) => {
            let expected = &crate::hash::sha256_hex(root.as_os_str().as_encoded_bytes())[..8];
            slug.ends_with(&format!("-{expected}"))
                .then(|| expected.to_string())
                .unwrap_or_default()
        }
        None => split_slug(&slug).1.to_string(),
    };

    let local_scanned = scan_local(slot);
    let imports = scan_bundles(slot, Some(pull::LOCAL_ARGOSY_NAME));

    // The same bundle can be installed both as a slot import and globally
    // (e.g. a project pins a global bundle locally). It is one bundle in
    // two roles, not two shadowing bundles: count it once (imports take
    // precedence) and never report its own concepts as duplicates. The
    // `globals` list still shows it for display.
    let seen_bundles: Vec<(String, String)> = local_scanned
        .iter()
        .chain(imports.iter())
        .map(bundle_key)
        .collect();
    let active_globals: Vec<&ScannedBundle> = globals
        .iter()
        .filter(|g| !seen_bundles.contains(&bundle_key(g)))
        .collect();

    // Contents aggregate every bundle active for the project.
    let mut contents = NamespaceCounts::default();
    for bundle in local_scanned
        .iter()
        .chain(imports.iter())
        .chain(active_globals.iter().copied())
    {
        contents.add(&bundle.info.contents);
    }

    let duplicate_concepts = duplicates(
        local_scanned
            .iter()
            .chain(imports.iter())
            .chain(active_globals.iter().copied()),
    );

    let index = index_status(slot, &local_scanned, &imports, globals, options);

    ProjectEntry {
        slug,
        hash,
        root,
        root_exists,
        stale,
        local: local_scanned.map(|s| s.info),
        imports: imports.into_iter().map(|s| s.info).collect(),
        globals: globals.iter().map(|s| s.info.clone()).collect(),
        contents,
        index,
        duplicate_concepts,
    }
}

/// The bundle-identity key used to detect one bundle installed in two
/// roles (a slot import and a global checkout): manifest identity is
/// name plus content version.
fn bundle_key(b: &ScannedBundle) -> (String, String) {
    (b.info.name.clone(), b.info.argosy_version.clone())
}

/// Concept ids shared by more than one bundle, sorted by id. Bundles are
/// visited in precedence order (local first), so each duplicate's `argosies`
/// list reads as the precedence chain. Bundles are identified by their root
/// directory, not manifest name: two distinct checkouts can share a manifest
/// name and are still separate bundles.
fn duplicates<'a>(bundles: impl Iterator<Item = &'a ScannedBundle>) -> Vec<DuplicateConcept> {
    let mut seen: Vec<(ConceptId, Vec<(PathBuf, String)>)> = Vec::new();
    for bundle in bundles {
        for id in &bundle.ids {
            let name = bundle.name().to_string();
            match seen.iter_mut().find(|(seen_id, _)| seen_id == id) {
                Some((_, entries)) => {
                    if !entries.iter().any(|(root, _)| root == &bundle.info.root) {
                        entries.push((bundle.info.root.clone(), name));
                    }
                }
                None => seen.push((id.clone(), vec![(bundle.info.root.clone(), name)])),
            }
        }
    }
    seen.retain(|(_, entries)| entries.len() > 1);
    seen.sort_by(|a, b| a.0.cmp(&b.0));
    seen.into_iter()
        .map(|(id, entries)| DuplicateConcept {
            id: id.to_string(),
            argosies: entries.into_iter().map(|(_, name)| name).collect(),
        })
        .collect()
}

/// Computes the index status for a slot. The store is opened read-only;
/// staleness additionally diffs the store against the active bundles and is
/// only available with the `default-index` feature.
fn index_status(
    slot: &Path,
    local: &Option<ScannedBundle>,
    imports: &[ScannedBundle],
    globals: &[ScannedBundle],
    options: &CatalogOptions,
) -> IndexStatus {
    let db = slot.join(
        options
            .index_db_name
            .as_deref()
            .unwrap_or(pull::INDEX_DB_NAME),
    );
    let present = db.is_file();
    let status = IndexStatus {
        present,
        db: present.then(|| db.clone()),
        expected_model_id: options.expected_model_id.clone(),
        last_indexed_secs: fs::metadata(&db)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs()),
        ..IndexStatus::default()
    };
    if !present {
        return status;
    }

    #[cfg(feature = "default-index")]
    {
        use crate::context::ProjectContext;
        use crate::index::sqlite::SqliteVecStore;
        use crate::index::{VectorStore, staleness_report};

        let mut status = status;
        if let Ok(store) = SqliteVecStore::open_read_only(&db) {
            status.model_id = store.model_id().map(str::to_string);
            // Staleness needs the active bundle set; skip when the local
            // bundle is missing (a broken slot is still reported, just
            // without a diff).
            let import_paths: Vec<PathBuf> = imports
                .iter()
                .chain(globals.iter())
                .map(|b| b.info.root.clone())
                .collect();
            if let Some(local) = local
                && let Ok(context) = ProjectContext::open(local.info.root.clone(), import_paths)
            {
                let expected = options
                    .expected_model_id
                    .clone()
                    .unwrap_or_else(|| store.model_id().unwrap_or_default().to_string());
                status.staleness = staleness_report(&context, &store, &expected).ok();
            }
        }
        status
    }
    #[cfg(not(feature = "default-index"))]
    {
        let _ = (local, imports, globals);
        status
    }
}

/// The slot directories under `<state>/projects/`, sorted by name. Missing
/// or unreadable `projects/` is "no projects", not an error.
fn list_slots(state_root: &Path) -> Vec<PathBuf> {
    let dir = state_root.join("projects");
    let Ok(mut entries) = fs::read_dir(&dir).map(|rd| rd.flatten().collect::<Vec<_>>()) else {
        return Vec::new();
    };
    entries.sort_by_key(fs::DirEntry::file_name);
    entries
        .into_iter()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect()
}

/// The bundle checkouts directly under `dir` (directories holding an
/// `argosy.md`), sorted by directory name. `exclude` skips one checkout name
/// (the local `default`, scanned separately with `writable = true`). Missing
/// or unreadable `dir` means "no bundles"; unopenable checkouts are skipped.
fn scan_bundles(dir: &Path, exclude: Option<&str>) -> Vec<ScannedBundle> {
    let Ok(mut entries) = fs::read_dir(dir).map(|rd| rd.flatten().collect::<Vec<_>>()) else {
        return Vec::new();
    };
    entries.sort_by_key(fs::DirEntry::file_name);
    let mut out = Vec::new();
    for entry in entries {
        let name = entry.file_name();
        if exclude.is_some_and(|exclude| name == exclude) {
            continue;
        }
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let path = entry.path();
        if !path.join("argosy.md").is_file() {
            continue;
        }
        if let Ok(argosy) = Argosy::open(&path) {
            out.push(scan_one(&argosy));
        }
    }
    out
}

/// Scans the local `default` bundle of `slot` as the writable local argosy.
/// `None` when the slot has no openable local bundle.
fn scan_local(slot: &Path) -> Option<ScannedBundle> {
    let path = slot.join(pull::LOCAL_ARGOSY_NAME);
    if !path.join("argosy.md").is_file() {
        return None;
    }
    let argosy = Argosy::open(&path).ok()?;
    let mut scanned = scan_one(&argosy);
    scanned.info.writable = true;
    Some(scanned)
}

/// Summarizes one opened bundle and collects its concept ids.
fn scan_one(argosy: &Argosy) -> ScannedBundle {
    let mut ids = Vec::new();
    for namespace in argosy.namespaces_present() {
        if let Ok(concepts) = argosy.concepts(&namespace) {
            ids.extend(concepts.into_iter().map(|(id, _)| id));
        }
    }
    ScannedBundle {
        info: BundleInfo {
            name: argosy.manifest().name().to_string(),
            description: argosy.manifest().description().map(str::to_string),
            root: argosy.root().to_path_buf(),
            writable: false,
            argosy_version: argosy.manifest().argosy_version().to_string(),
            contents: counts(argosy),
        },
        ids,
    }
}

/// Per-namespace counts for one bundle. A listing failure for a namespace
/// counts as zero rather than failing the catalog.
fn counts(argosy: &Argosy) -> NamespaceCounts {
    NamespaceCounts {
        documents: argosy
            .concepts(&Namespace::Document)
            .map(|v| v.len())
            .unwrap_or(0),
        memories: argosy
            .concepts(&Namespace::Memory)
            .map(|v| v.len())
            .unwrap_or(0),
        skills: Skill::list(argosy).map(|v| v.len()).unwrap_or(0),
        styleguides: StyleguideRule::list(argosy).map(|v| v.len()).unwrap_or(0),
    }
}

/// Splits a slot slug into its project-name and hash parts. The hash is the
/// final `-`-delimited component when it is exactly eight hex digits;
/// otherwise the whole slug is the name and the hash is empty.
fn split_slug(slug: &str) -> (&str, &str) {
    match slug.rsplit_once('-') {
        Some((name, hash)) if hash.len() == 8 && hash.chars().all(|c| c.is_ascii_hexdigit()) => {
            (name, hash)
        }
        _ => (slug, ""),
    }
}

/// The part of `path` after the `home` prefix, or `None` when `path` is not
/// under `home`. On Windows, where paths are case-insensitive, prefix
/// components are compared case-insensitively (`C:\Users\bob` vs
/// `c:\users\BOB`); elsewhere the comparison is exact.
fn strip_home_prefix(path: &Path, home: &Path) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let mut path_components = path.components();
        let mut home_components = home.components();
        loop {
            match (path_components.next(), home_components.next()) {
                (Some(p), Some(h)) => {
                    let same = p
                        .as_os_str()
                        .to_string_lossy()
                        .eq_ignore_ascii_case(&h.as_os_str().to_string_lossy());
                    if !same {
                        return None;
                    }
                }
                (None, Some(_)) => return None,
                _ => return Some(path_components.as_path().to_path_buf()),
            }
        }
    }
    #[cfg(not(windows))]
    path.strip_prefix(home).ok().map(Path::to_path_buf)
}

/// Rewrites every home-prefixed path in the catalog to `~/…`, so a shared or
/// pasted catalog does not leak the user's home directory.
pub fn redact_home(catalog: &mut Catalog, home: &Path) {
    // Try both the given spelling and its canonical form: recorded roots are
    // canonicalized (e.g. macOS `/var` -> `/private/var`), while the state
    // root and config-derived paths may not be.
    let mut homes = vec![home.to_path_buf()];
    if let Ok(canonical) = home.canonicalize()
        && canonical != home
    {
        homes.push(canonical);
    }
    let redact = |path: &mut PathBuf| {
        for home in &homes {
            if let Some(rest) = strip_home_prefix(path, home) {
                *path = if rest.as_os_str().is_empty() {
                    PathBuf::from("~")
                } else {
                    Path::new("~").join(rest)
                };
                return;
            }
        }
    };

    redact(&mut catalog.state_root);
    for project in &mut catalog.projects {
        if let Some(root) = &mut project.root {
            redact(root);
        }
        if let Some(local) = &mut project.local {
            redact(&mut local.root);
        }
        for bundle in project.imports.iter_mut().chain(project.globals.iter_mut()) {
            redact(&mut bundle.root);
        }
        if let Some(db) = &mut project.index.db {
            redact(db);
        }
    }
}

/// Writes [`render_markdown`] to `<state_root>/README.md`, returning the
/// path written. The file lives outside every bundle, so packaging (which
/// only ever sees bundle contents) never includes it.
pub fn write(state_root: &Path, catalog: &Catalog) -> Result<PathBuf> {
    let path = state_root.join(CATALOG_FILE);
    fs::create_dir_all(state_root).context(IoSnafu {
        path: state_root.to_path_buf(),
    })?;
    fs::write(&path, render_markdown(catalog)).context(IoSnafu { path: path.clone() })?;
    Ok(path)
}

/// Renders the catalog as the human-readable markdown stored at
/// `<state>/README.md` (and served over MCP as `argosy://catalog`).
pub fn render_markdown(catalog: &Catalog) -> String {
    let mut out = String::from("# Argosy catalog\n\n");
    out.push_str("<!-- Generated by `argosy catalog --write`; edits are overwritten. -->\n\n");
    out.push_str(&format!(
        "- State root: `{}`\n",
        catalog.state_root.display()
    ));
    out.push_str(&format!("- Projects: {}\n", catalog.projects.len()));
    if catalog.projects.is_empty() {
        out.push_str("\nNo projects registered under this state root.\n");
        return out;
    }

    for project in &catalog.projects {
        out.push('\n');
        out.push_str(&format!("## {}\n\n", project.slug));
        out.push_str(&format!(
            "- Project root: {}\n",
            match &project.root {
                Some(root) => format!("`{}`", root.display()),
                None => "<not recorded>".to_string(),
            }
        ));
        if project.root.is_some() {
            out.push_str(&format!(
                "- Project directory exists: {}\n",
                match project.root_exists {
                    Some(true) => "yes",
                    Some(false) => "no (stale slot)",
                    None => "unknown",
                }
            ));
        }
        out.push_str(&format!(
            "- Local bundle: {}\n",
            match &project.local {
                Some(local) => match &local.description {
                    Some(description) => {
                        format!("{} (writable) — {}", local.name, description)
                    }
                    None => format!("{} (writable)", local.name),
                },
                None => "<missing>".to_string(),
            }
        ));
        render_bundle_list(&mut out, "Imports", &project.imports);
        render_bundle_list(&mut out, "Global imports", &project.globals);

        out.push_str("- Contents:\n");
        out.push_str(&format!("  - documents: {}\n", project.contents.documents));
        out.push_str(&format!("  - memories: {}\n", project.contents.memories));
        out.push_str(&format!("  - skills: {}\n", project.contents.skills));
        out.push_str(&format!(
            "  - styleguides: {}\n",
            project.contents.styleguides
        ));

        out.push_str(&format!("- Index: {}\n", project.index.label()));
        if let Some(model) = &project.index.model_id {
            out.push_str(&format!("  - model: {model}\n"));
        }
        if let (Some(actual), Some(expected)) =
            (&project.index.model_id, &project.index.expected_model_id)
            && actual != expected
        {
            out.push_str(&format!("  - expected model: {expected}\n"));
        }
        if let Some(secs) = project.index.last_indexed_secs {
            out.push_str(&format!("  - last indexed (epoch seconds): {secs}\n"));
        }

        if !project.duplicate_concepts.is_empty() {
            out.push_str("- Duplicate concept ids (defined by more than one bundle):\n");
            for duplicate in &project.duplicate_concepts {
                out.push_str(&format!(
                    "  - {} ({})\n",
                    duplicate.id,
                    duplicate.argosies.join(", ")
                ));
            }
        }
        if project.stale {
            out.push_str(
                "- Stale: the recorded project directory no longer exists; the slot can be \
                 deleted to reclaim space.\n",
            );
        }
    }
    out
}

/// Renders one bundle list (`Imports` / `Global imports`).
fn render_bundle_list(out: &mut String, label: &str, bundles: &[BundleInfo]) {
    if bundles.is_empty() {
        out.push_str(&format!("- {label}: <none>\n"));
        return;
    }
    out.push_str(&format!("- {label}:\n"));
    for bundle in bundles {
        let role = if bundle.writable {
            "writable"
        } else {
            "read-only"
        };
        let counts = &bundle.contents;
        out.push_str(&format!(
            "  - {} ({role}; {} document(s), {} memor(ies), {} skill(s), {} styleguide(s))\n",
            bundle.name, counts.documents, counts.memories, counts.skills, counts.styleguides
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Namespace;
    use crate::local::LocalArgosy;
    use tempfile::TempDir;

    /// Writes a bundle with the given manifest name and counts.
    fn bundle(root: &Path, name: &str, documents: usize, skills: usize) -> LocalArgosy {
        fs::create_dir_all(root).unwrap();
        let argosy = LocalArgosy::init(root, Some(name), Some("test bundle")).unwrap();
        for i in 0..documents {
            let concept: crate::Concept =
                format!("---\ntype: Reference\ndescription: doc {i}\n---\n# Doc {i}\n")
                    .parse()
                    .unwrap();
            argosy
                .write_concept(
                    Namespace::Document,
                    &format!("document/doc-{i}").parse().unwrap(),
                    &concept,
                )
                .unwrap();
        }
        for i in 0..skills {
            let concept: crate::Concept =
                format!("---\ntype: Skill\ndescription: skill {i}\n---\n# Skill {i}\n")
                    .parse()
                    .unwrap();
            argosy
                .write_concept(
                    Namespace::Skill,
                    &format!("skill/skill-{i}").parse().unwrap(),
                    &concept,
                )
                .unwrap();
        }
        argosy
    }

    fn options() -> CatalogOptions {
        CatalogOptions::default()
    }

    #[test]
    fn build_reports_local_imports_globals_and_counts() {
        let scratch = TempDir::new().unwrap();
        let state = scratch.path().join("argosy");
        let project_root = scratch.path().join("myproj");
        fs::create_dir_all(&project_root).unwrap();

        let slot = pull::record_project_root_at(&state, &project_root).unwrap();
        bundle(&slot.join("default"), "myproj", 2, 1);
        bundle(&slot.join("shared"), "shared-knowledge", 1, 0);
        bundle(&state.join("global/stylish"), "stylish", 0, 0);

        let catalog = build(&state, &options()).unwrap();
        assert_eq!(catalog.projects.len(), 1);
        let entry = &catalog.projects[0];
        assert_eq!(entry.local.as_ref().unwrap().name, "myproj");
        assert!(entry.local.as_ref().unwrap().writable);
        assert_eq!(entry.imports.len(), 1);
        assert_eq!(entry.imports[0].name, "shared-knowledge");
        assert!(!entry.imports[0].writable);
        assert_eq!(entry.globals.len(), 1);
        assert_eq!(entry.globals[0].name, "stylish");
        // 2 local + 1 imported + 0 global documents; 1 local skill.
        assert_eq!(entry.contents.documents, 3);
        assert_eq!(entry.contents.skills, 1);
        assert!(entry.root.as_ref().unwrap().ends_with("myproj"));
        assert_eq!(entry.root_exists, Some(true));
        assert!(!entry.stale);
        assert_eq!(entry.hash.len(), 8);
    }

    #[test]
    fn stale_slot_is_flagged_when_project_directory_is_gone() {
        let scratch = TempDir::new().unwrap();
        let state = scratch.path().join("argosy");
        let gone = scratch.path().join("gone-project");
        fs::create_dir_all(&gone).unwrap();
        let slot = pull::record_project_root_at(&state, &gone).unwrap();
        bundle(&slot.join("default"), "gone-project", 1, 0);
        fs::remove_dir_all(&gone).unwrap();

        let catalog = build(&state, &options()).unwrap();
        let entry = &catalog.projects[0];
        assert_eq!(entry.root_exists, Some(false));
        assert!(entry.stale);
    }

    #[test]
    fn slot_without_recorded_root_reports_unknown() {
        let scratch = TempDir::new().unwrap();
        let state = scratch.path().join("argosy");
        let slot = state.join("projects/legacy-1234abcd");
        bundle(&slot.join("default"), "legacy", 0, 0);

        let catalog = build(&state, &options()).unwrap();
        let entry = &catalog.projects[0];
        assert!(entry.root.is_none());
        assert_eq!(entry.root_exists, None);
        assert!(!entry.stale);
    }

    #[test]
    fn duplicate_concept_ids_across_bundles_are_reported() {
        let scratch = TempDir::new().unwrap();
        let state = scratch.path().join("argosy");
        let project_root = scratch.path().join("dupes");
        fs::create_dir_all(&project_root).unwrap();
        let slot = pull::record_project_root_at(&state, &project_root).unwrap();
        let local = bundle(&slot.join("default"), "dupes", 0, 0);
        let shared = bundle(&slot.join("shared"), "shared", 0, 0);
        let concept: crate::Concept =
            "---\ntype: Reference\ndescription: shared rule\n---\n# Shared\n"
                .parse()
                .unwrap();
        for argosy in [&local, &shared] {
            argosy
                .write_concept(
                    Namespace::Document,
                    &"document/shared".parse().unwrap(),
                    &concept,
                )
                .unwrap();
        }

        let catalog = build(&state, &options()).unwrap();
        let dupes = &catalog.projects[0].duplicate_concepts;
        assert_eq!(dupes.len(), 1);
        assert_eq!(dupes[0].id, "document/shared");
        assert_eq!(dupes[0].argosies, vec!["dupes", "shared"]);
    }

    #[test]
    fn duplicate_ids_are_keyed_by_checkout_not_manifest_name() {
        // Two distinct checkouts can share a manifest name; a duplicate they
        // both define must not be silently dropped by name-based dedupe.
        let scratch = TempDir::new().unwrap();
        let state = scratch.path().join("argosy");
        let project_root = scratch.path().join("namedupe");
        fs::create_dir_all(&project_root).unwrap();
        let slot = pull::record_project_root_at(&state, &project_root).unwrap();
        let a = bundle(&slot.join("shared-a"), "shared", 0, 0);
        let b = bundle(&slot.join("shared-b"), "shared", 0, 0);
        let concept: crate::Concept = "---\ntype: Reference\ndescription: x\n---\n# X\n"
            .parse()
            .unwrap();
        for argosy in [&a, &b] {
            argosy
                .write_concept(
                    Namespace::Document,
                    &"document/x".parse().unwrap(),
                    &concept,
                )
                .unwrap();
        }

        let catalog = build(&state, &options()).unwrap();
        let dupes = &catalog.projects[0].duplicate_concepts;
        assert_eq!(dupes.len(), 1, "{dupes:?}");
        assert_eq!(dupes[0].id, "document/x");
        assert_eq!(dupes[0].argosies, vec!["shared", "shared"]);
    }

    #[test]
    fn a_bundle_installed_as_both_import_and_global_is_counted_once() {
        let scratch = TempDir::new().unwrap();
        let state = scratch.path().join("argosy");
        let project_root = scratch.path().join("pinproj");
        fs::create_dir_all(&project_root).unwrap();
        let slot = pull::record_project_root_at(&state, &project_root).unwrap();
        bundle(&slot.join("default"), "pinproj", 0, 0);
        // Same manifest name and version, two roles: a project-local import
        // and the global checkout.
        bundle(&slot.join("shared"), "shared", 2, 0);
        bundle(&state.join("global/team"), "shared", 2, 0);

        let catalog = build(&state, &options()).unwrap();
        let entry = &catalog.projects[0];
        // The global is still listed for display…
        assert_eq!(entry.imports.len(), 1);
        assert_eq!(entry.globals.len(), 1);
        // …but its concepts are counted once and never reported as
        // duplicates of the import (2 shared documents, not 4).
        assert_eq!(entry.contents.documents, 2);
        assert!(
            entry.duplicate_concepts.is_empty(),
            "{:?}",
            entry.duplicate_concepts
        );
    }

    #[test]
    fn hash_is_checked_against_the_recorded_root_not_the_slug_text() {
        let scratch = TempDir::new().unwrap();
        let state = scratch.path().join("argosy");
        // A project legitimately named like `name-hash`: the slug's trailing
        // hex component is part of the name, not a hash suffix.
        let project_root = scratch.path().join("deadbeef-cafe1234");
        fs::create_dir_all(&project_root).unwrap();
        let slot = pull::record_project_root_at(&state, &project_root).unwrap();
        bundle(&slot.join("default"), "deadbeef-cafe1234", 0, 0);

        let catalog = build(&state, &options()).unwrap();
        let entry = &catalog.projects[0];
        // The slug is `deadbeef-cafe1234-<hash>`; `cafe1234` is part of the
        // name, so the hash is the root's actual path hash, never
        // `cafe1234`.
        assert_ne!(entry.hash, "cafe1234");
        assert_eq!(entry.hash.len(), 8);

        // A host-written slot whose sidecar points elsewhere carries the
        // exact hash of the recorded root, regardless of the slug's shape.
        let other = scratch.path().join("real-project");
        fs::create_dir_all(&other).unwrap();
        let slug = pull::project_slug(&other);
        let manual = state.join("projects").join(&slug);
        fs::create_dir_all(&manual).unwrap();
        // The sidecar must carry the canonical spelling, as
        // record_project_root_at would write it.
        let canonical_other = other.canonicalize().unwrap();
        fs::write(
            manual.join(pull::SLOT_ROOT_FILE),
            canonical_other.to_string_lossy().as_bytes(),
        )
        .unwrap();
        let expected =
            &crate::hash::sha256_hex(canonical_other.as_os_str().as_encoded_bytes())[..8];
        let catalog = build(&state, &options()).unwrap();
        let entry = &catalog.projects[1];
        assert_eq!(entry.slug, slug);
        assert_eq!(entry.hash, expected);

        // And a host-written slot whose slug simply does not end in the
        // recorded root's hash reports no hash at all rather than a
        // guess.
        let mismatch = state.join("projects/hexy-deadbeef");
        fs::create_dir_all(&mismatch).unwrap();
        fs::write(
            mismatch.join(pull::SLOT_ROOT_FILE),
            canonical_other.to_string_lossy().as_bytes(),
        )
        .unwrap();
        let catalog = build(&state, &options()).unwrap();
        // Sorted: `deadbeef-…`, `hexy-deadbeef`, `real-project-…`.
        let entry = &catalog.projects[1];
        assert_eq!(entry.slug, "hexy-deadbeef");
        assert_eq!(entry.hash, "");
        let entry = &catalog.projects[2];
        assert_eq!(entry.hash, expected);
    }

    #[test]
    fn sidecar_less_slots_fall_back_to_the_textual_hash_heuristic() {
        let scratch = TempDir::new().unwrap();
        let state = scratch.path().join("argosy");
        let slot = state.join("projects/legacy-1234abcd");
        bundle(&slot.join("default"), "legacy", 0, 0);

        let catalog = build(&state, &options()).unwrap();
        let entry = &catalog.projects[0];
        assert_eq!(entry.hash, "1234abcd");
    }

    #[test]
    fn broken_local_bundle_is_reported_as_missing() {
        // A corrupt `default` (argosy.md present but unopenable) is skipped
        // from the catalog rather than failing the whole build.
        let scratch = TempDir::new().unwrap();
        let state = scratch.path().join("argosy");
        let project_root = scratch.path().join("brokenproj");
        fs::create_dir_all(&project_root).unwrap();
        let slot = pull::record_project_root_at(&state, &project_root).unwrap();
        fs::create_dir_all(slot.join("default")).unwrap();
        fs::write(slot.join("default/argosy.md"), "not a mapping: [").unwrap();

        let catalog = build(&state, &options()).unwrap();
        let entry = &catalog.projects[0];
        assert!(entry.local.is_none());
    }

    #[test]
    #[cfg(unix)]
    fn redaction_handles_a_canonicalized_spelling_of_home() {
        use std::os::unix::fs::symlink;

        let scratch = TempDir::new().unwrap();
        let real_home = scratch.path().join("home-real");
        fs::create_dir_all(real_home.join("repo")).unwrap();
        let alias = scratch.path().join("home-alias");
        symlink(&real_home, &alias).unwrap();
        let project_root = real_home.join("repo/myproj");
        fs::create_dir_all(&project_root).unwrap();
        // recorded_project_root_at canonicalizes the project root, so the
        // recorded spelling is the real one while redaction is asked for the
        // alias — exactly the two-spelling case redact_home must bridge.
        let state = alias.join("state/argosy");
        let slot = pull::record_project_root_at(&state, &project_root).unwrap();
        bundle(&slot.join("default"), "myproj", 1, 0);

        let mut catalog = build(&state, &options()).unwrap();
        redact_home(&mut catalog, &alias);
        let markdown = render_markdown(&catalog);
        assert!(
            markdown.contains("Project root: `~/repo/myproj`"),
            "{markdown}"
        );
        assert!(
            !markdown.contains(&real_home.display().to_string()),
            "{markdown}"
        );
    }

    #[test]
    fn markdown_renders_the_documented_shape_and_redacts_home() {
        let scratch = TempDir::new().unwrap();
        let state = scratch.path().join("home/state/argosy");
        let project_root = scratch.path().join("home/repo/myproj");
        fs::create_dir_all(&project_root).unwrap();
        let slot = pull::record_project_root_at(&state, &project_root).unwrap();
        bundle(&slot.join("default"), "myproj", 3, 1);

        let mut catalog = build(&state, &options()).unwrap();
        redact_home(&mut catalog, &scratch.path().join("home"));
        let markdown = render_markdown(&catalog);
        assert!(markdown.contains("# Argosy catalog"));
        assert!(
            markdown.contains("Project root: `~/repo/myproj`"),
            "{markdown}"
        );
        assert!(
            markdown.contains("- Local bundle: myproj (writable)"),
            "{markdown}"
        );
        assert!(markdown.contains("documents: 3"), "{markdown}");
        assert!(markdown.contains("skills: 1"), "{markdown}");
        assert!(markdown.contains("- Index: absent"), "{markdown}");
        // State root is redacted too (no absolute home path leaks).
        assert!(
            !markdown.contains(&scratch.path().display().to_string()),
            "{markdown}"
        );
    }

    #[test]
    fn write_materializes_readme_at_the_state_root() {
        let scratch = TempDir::new().unwrap();
        let state = scratch.path().join("argosy");
        fs::create_dir_all(&state).unwrap();
        let catalog = build(&state, &options()).unwrap();
        let path = write(&state, &catalog).unwrap();
        assert_eq!(path, state.join(CATALOG_FILE));
        assert!(path.is_file());
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .contains("# Argosy catalog")
        );
    }

    #[test]
    fn split_slug_only_strips_a_trailing_hex_hash() {
        assert_eq!(split_slug("craft-1a2b3c4d"), ("craft", "1a2b3c4d"));
        assert_eq!(
            split_slug("my-project-1a2b3c4d"),
            ("my-project", "1a2b3c4d")
        );
        assert_eq!(split_slug("plain"), ("plain", ""));
        assert_eq!(
            split_slug("not-a-hash-zzzzzzzz"),
            ("not-a-hash-zzzzzzzz", "")
        );
    }
}
