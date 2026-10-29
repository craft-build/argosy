//! Pulling external argosies into a project or the global store.
//!
//! Argosy data lives under the user's state dir ([`state_dir`]), never in
//! the project tree: agents working the tree would just read the markdown
//! directly, bypassing the argosy MCP tools.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use snafu::{IntoError, OptionExt, ResultExt, ensure};

use serde::Serialize;

use crate::bundle::Argosy;
use crate::error::{IoSnafu, NotAnArgosySnafu, Result, ValidationSnafu};

/// The checkout name of a project's local (writable) bundle: `<state>/projects/<slug>/default`.
pub const LOCAL_ARGOSY_NAME: &str = "default";

/// The derived index file inside a project's state directory: `<state>/projects/<slug>/index.db`.
pub const INDEX_DB_NAME: &str = "index.db";

/// The sidecar in a project's state-dir slot recording its canonical project
/// root, so the catalog can map slots back to projects and flag stale ones.
pub const SLOT_ROOT_FILE: &str = ".project-root";

/// The sibling stamp `argosy pull` records beside every checkout: the SHA-256
/// of the sidecar it verified, or `absent` when the book shipped none.
///
/// It lives outside the cloned tree, so tampering with the checkout — the
/// sidecar rewritten or deleted included — is caught at the next open.
pub const IMPORT_INTEGRITY_SUFFIX: &str = ".import-integrity";

/// The stamp body for a checkout that shipped no integrity sidecar.
const STAMP_NO_SIDECAR: &str = "absent";

/// The user's argosy state root that every argosy path derives from:
/// `$XDG_STATE_HOME/argosy`, `~/.local/state/argosy`, or on Windows
/// `%USERPROFILE%\AppData\Local\argosy`.
pub fn state_dir() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(|home| home_state_base(PathBuf::from(home)))
        })
        .with_context(|| ValidationSnafu {
            reason: "cannot locate the argosy state directory: set XDG_STATE_HOME or HOME"
                .to_string(),
        })?;
    Ok(base.join("argosy"))
}

/// The per-user state base under `home`: `~/.local/state` on Unix,
/// `<home>\AppData\Local` on Windows.
fn home_state_base(home: PathBuf) -> PathBuf {
    if cfg!(windows) {
        home.join("AppData").join("Local")
    } else {
        home.join(".local").join("state")
    }
}

/// The directory of user-installed argosies shared by every project: `<state>/global`.
pub fn global_argosy_dir() -> Result<PathBuf> {
    Ok(state_dir()?.join("global"))
}

/// The state-dir slug of a project root: its final directory component plus
/// the first 8 hex digits of the SHA-256 of its absolute path, so
/// same-named projects never share storage.
///
/// The root is canonicalized first, so different spellings map to one slug.
pub fn project_slug(project_root: impl AsRef<Path>) -> String {
    let root = project_root.as_ref();
    let canonical = root
        .canonicalize()
        .or_else(|_| std::path::absolute(root))
        .unwrap_or_else(|_| root.to_path_buf());
    let name = canonical
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "project".to_string());
    let digest = crate::hash::sha256_hex(canonical.as_os_str().as_encoded_bytes());
    // The digest is lowercase ASCII hex, so slicing the first 8 bytes is safe.
    format!("{name}-{}", &digest[..8])
}

/// The directory holding a project's argosy state (local `default` bundle,
/// pulled checkouts, derived `index.db`), never in the project tree.
pub fn project_argosy_dir(project_root: impl AsRef<Path>) -> Result<PathBuf> {
    Ok(project_argosy_dir_at(&state_dir()?, project_root))
}

/// [`project_argosy_dir`] against an explicit state root (tests and hosts
/// inject a tempdir).
pub fn project_argosy_dir_at(state_root: &Path, project_root: impl AsRef<Path>) -> PathBuf {
    state_root.join("projects").join(project_slug(project_root))
}

/// The canonical spelling of `project_root`: its resolved path when the
/// directory exists, else its absolutized spelling.
///
/// The fallback keeps a deleted project's slot stable.
pub fn canonical_project_root(project_root: impl AsRef<Path>) -> PathBuf {
    let root = project_root.as_ref();
    root.canonicalize()
        .or_else(|_| std::path::absolute(root))
        .unwrap_or_else(|_| root.to_path_buf())
}

/// Records `project_root`'s canonical path in its state-dir slot, creating
/// the slot directory if needed, and returns the slot path.
pub fn record_project_root_at(
    state_root: &Path,
    project_root: impl AsRef<Path>,
) -> Result<PathBuf> {
    let slot = project_argosy_dir_at(state_root, project_root.as_ref());
    fs::create_dir_all(&slot).context(IoSnafu { path: slot.clone() })?;
    let sidecar = slot.join(SLOT_ROOT_FILE);
    let canonical = canonical_project_root(project_root);
    fs::write(&sidecar, canonical.to_string_lossy().as_bytes())
        .context(IoSnafu { path: sidecar })?;
    Ok(slot)
}

/// [`record_project_root_at`] against the resolved [`state_dir`].
pub fn record_project_root(project_root: impl AsRef<Path>) -> Result<PathBuf> {
    record_project_root_at(&state_dir()?, project_root)
}

/// The project root recorded for `slot` by [`record_project_root_at`], if any.
pub fn recorded_project_root(slot: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(slot.join(SLOT_ROOT_FILE)).ok()?;
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
}

/// Maps a failed `git` process spawn. A missing binary is a setup problem, not
/// an I/O problem on the destination.
fn git_spawn_error(dest: &Path, err: std::io::Error) -> crate::error::Error {
    if err.kind() == std::io::ErrorKind::NotFound {
        return ValidationSnafu {
            reason: "`git` was not found on PATH; `argosy pull` shells out to \
                     `git clone`, so install git (or fix PATH) and retry"
                .to_string(),
        }
        .build();
    }
    IoSnafu {
        path: dest.to_path_buf(),
    }
    .into_error(err)
}

/// Clones the argosy at `url` into `<root>/<name>` via `git clone`, opens it,
/// and verifies its integrity sidecar ([`crate::package::INTEGRITY_FILENAME`])
/// before accepting the checkout.
///
/// All-or-nothing: a failed, non-bundle, or integrity-failing clone leaves no
/// checkout behind.
pub fn clone_as_checkout(url: &str, root: &Path, name: &str) -> Result<Argosy> {
    ensure_checkout_target(url, root, name)?;
    let dest = root.join(name);
    clone_url_to(url, &dest)?;
    accept_checkout(&dest)
}

/// [`clone_as_checkout`], but checking out `commit` (detached) instead of the
/// remote's default-branch HEAD: the reproduction path for a pinned import.
///
/// All-or-nothing. When `commit` is not reachable in the fresh clone (it was
/// force-pushed away upstream), the error says so rather than falling back
/// to whatever the remote now serves.
pub fn clone_as_checkout_at(url: &str, root: &Path, name: &str, commit: &str) -> Result<Argosy> {
    ensure_checkout_target(url, root, name)?;
    ensure_valid_commit_id(commit)?;
    let dest = root.join(name);
    clone_url_to(url, &dest)?;
    // Detach before acceptance, so what is opened, verified, and stamped is
    // the pinned tree, not the default branch.
    if let Err(error) = git_in_dir(&dest, &["checkout", "--quiet", "--detach", commit]) {
        let _ = fs::remove_dir_all(&dest);
        return match error {
            crate::error::Error::Validation { reason } => ValidationSnafu {
                reason: format!(
                    "pinned commit `{commit}` could not be checked out from `{url}` \
                     (it may have been force-pushed away upstream): {reason}"
                ),
            }
            .fail(),
            other => Err(other),
        };
    }
    accept_checkout(&dest)
}

/// The shared guards of every checkout-creating path: a URL git can take as
/// a URL (never an option), a name that becomes a safe, non-colliding
/// directory beside the others in the store, and no checkout there yet.
fn ensure_checkout_target(url: &str, root: &Path, name: &str) -> Result<()> {
    ensure!(
        !url.is_empty() && !url.starts_with('-'),
        ValidationSnafu {
            reason: format!(
                "invalid clone url `{url}`: pass a git URL or remote name, not an option"
            )
        }
    );
    ensure!(
        crate::bundle::is_safe_bundle_name(name),
        ValidationSnafu {
            reason: format!(
                "invalid checkout name `{name}`: only [A-Za-z0-9._-] are allowed (it becomes the `argosy://` URI spelling)"
            )
        }
    );
    ensure!(
        name != LOCAL_ARGOSY_NAME,
        ValidationSnafu {
            reason: format!(
                "checkout name `{LOCAL_ARGOSY_NAME}` is reserved for the project-local bundle (create it with `argosy init`, never `argosy pull`)"
            )
        }
    );
    ensure!(
        name != crate::lockfile::LOCKFILE_NAME,
        ValidationSnafu {
            reason: format!(
                "checkout name `{}` is reserved for the import lockfile",
                crate::lockfile::LOCKFILE_NAME
            )
        }
    );
    let dest = root.join(name);
    ensure!(
        !dest.exists(),
        ValidationSnafu {
            reason: format!(
                "checkout `{}` already exists; refusing to overwrite it",
                dest.display()
            )
        }
    );
    Ok(())
}

/// True iff `commit` is a 40- or 64-hex-digit object id, so it can never be
/// parsed by git as an option or a path.
fn ensure_valid_commit_id(commit: &str) -> Result<()> {
    let well_formed =
        (commit.len() == 40 || commit.len() == 64) && commit.bytes().all(|b| b.is_ascii_hexdigit());
    ensure!(
        well_formed,
        ValidationSnafu {
            reason: format!(
                "invalid commit id `{commit}`: expected a 40- or 64-hex-digit object id"
            )
        }
    );
    Ok(())
}

/// Runs `git` inside `dir`, mapping a non-zero exit to a validation error
/// that carries git's stderr.
fn git_in_dir(dir: &Path, args: &[&str]) -> Result<std::process::Output> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|err| git_spawn_error(dir, err))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return ValidationSnafu {
            reason: format!(
                "`git {}` failed ({}): {}",
                args.join(" "),
                output.status,
                stderr.trim()
            ),
        }
        .fail();
    }
    Ok(output)
}

/// Clones `url` into `dest`, removing `dest` on failure.
fn clone_url_to(url: &str, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest.parent().unwrap_or_else(|| Path::new("."))).context(IoSnafu {
        path: dest.to_path_buf(),
    })?;
    let output = Command::new("git")
        .args(["clone", "--quiet", url])
        .arg(dest)
        .output()
        .map_err(|err| git_spawn_error(dest, err))?;
    if !output.status.success() {
        let _ = fs::remove_dir_all(dest);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return ValidationSnafu {
            reason: format!(
                "`git clone {url}` failed ({status}): {stderr}",
                status = output.status,
                stderr = stderr.trim()
            ),
        }
        .fail();
    }
    Ok(())
}

/// Opens a fresh clone as an accepted checkout: it must be an argosy, its
/// sidecar (when present) must verify, and the pull-time integrity stamp is
/// written. Any failure removes the clone.
fn accept_checkout(dest: &Path) -> Result<Argosy> {
    match Argosy::open(dest) {
        Ok(argosy) => {
            // A missing sidecar is fine: never packaged, nothing to verify.
            if sidecar_is_regular_file(dest)
                && let Err(error) = crate::package::validate_integrity(dest)
            {
                let _ = fs::remove_dir_all(dest);
                return Err(error);
            }
            // The stamp is what makes a *later* tamper with the cloned tree
            // detectable.
            if let Err(error) = write_import_integrity_stamp(dest) {
                let _ = fs::remove_dir_all(dest);
                return Err(error);
            }
            Ok(argosy)
        }
        Err(error) => {
            let _ = fs::remove_dir_all(dest);
            NotAnArgosySnafu {
                path: dest.to_path_buf(),
                reason: format!("the clone is not an argosy: {error:#}"),
            }
            .fail()
        }
    }
}

/// Whether an accepted checkout carries a verified sidecar: `"verified"` or
/// `"no sidecar"`.
pub fn integrity_status(checkout: &Path) -> &'static str {
    if sidecar_is_regular_file(checkout) {
        "verified"
    } else {
        "no sidecar"
    }
}

/// The values [`integrity_status`] can return; also the allowed `integrity`
/// spellings in an import lockfile entry.
pub fn integrity_status_strings() -> &'static [&'static str] {
    &["verified", "no sidecar"]
}

/// The pull-time stamp path beside a checkout: `<parent>/<name>.import-integrity`.
pub fn import_integrity_stamp(checkout: &Path) -> PathBuf {
    let name = checkout
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "checkout".to_string());
    checkout.with_file_name(format!("{name}{IMPORT_INTEGRITY_SUFFIX}"))
}

/// Whether the integrity sidecar is present as a regular file.
///
/// Symlinks count as absent: a symlinked sidecar is in attacker-reachable
/// space and must never be followed.
fn sidecar_is_regular_file(checkout: &Path) -> bool {
    std::fs::symlink_metadata(checkout.join(crate::package::INTEGRITY_FILENAME))
        .is_ok_and(|m| m.file_type().is_file())
}

/// The stamp body for a freshly pulled `checkout`: the SHA-256 of its
/// verified sidecar, or [`STAMP_NO_SIDECAR`] when it shipped none.
fn stamp_body(checkout: &Path) -> Result<String> {
    if !sidecar_is_regular_file(checkout) {
        return Ok(STAMP_NO_SIDECAR.to_string());
    }
    let path = checkout.join(crate::package::INTEGRITY_FILENAME);
    let bytes = fs::read(&path).context(IoSnafu { path })?;
    Ok(crate::hash::sha256_hex(&bytes))
}

/// Writes the pull-time integrity stamp beside `checkout`. Called only
/// after the checkout was accepted.
fn write_import_integrity_stamp(checkout: &Path) -> Result<()> {
    let stamp = import_integrity_stamp(checkout);
    fs::write(&stamp, stamp_body(checkout)?).context(IoSnafu { path: stamp })
}

/// Verifies a checkout against its pull-time stamp ([`IMPORT_INTEGRITY_SUFFIX`]);
/// `Err` means the checkout must be excluded from the active set — never served.
///
/// Unstamped checkouts follow the standing rule: a present sidecar is verified,
/// an absent one means nothing was ever packaged.
pub fn verify_import_integrity(checkout: &Path) -> std::result::Result<(), String> {
    let repull = "remove the checkout and `argosy pull` it again";
    let stamp = fs::read_to_string(import_integrity_stamp(checkout))
        .ok()
        .map(|s| s.trim().to_string());
    let sidecar_path = checkout.join(crate::package::INTEGRITY_FILENAME);
    match (stamp.as_deref(), sidecar_is_regular_file(checkout)) {
        (None, false) => Ok(()),
        (None, true) => crate::package::validate_integrity(checkout)
            .map(|_| ())
            .map_err(|e| format!("{e:#}")),
        (Some(STAMP_NO_SIDECAR), false) => Ok(()),
        (Some(STAMP_NO_SIDECAR), true) => Err(format!(
            "an integrity sidecar appeared after `argosy pull` (the book was pulled without \
             one); {repull}"
        )),
        (Some(expected), true) => {
            let bytes = fs::read(&sidecar_path)
                .map_err(|e| format!("cannot read the integrity sidecar: {e}"))?;
            if crate::hash::sha256_hex(&bytes) != expected {
                return Err(format!(
                    "the integrity sidecar changed since `argosy pull` (rewritten or \
                     re-committed upstream); {repull}"
                ));
            }
            crate::package::validate_integrity(checkout)
                .map(|_| ())
                .map_err(|e| format!("{e:#}"))
        }
        (Some(_), false) => Err(format!(
            "the integrity sidecar is missing — it was verified when the book was pulled, so \
             the checkout changed underneath it; {repull}"
        )),
    }
}

/// The commit a checkout's git metadata resolves to, if any — the provenance
/// hits and reads report for imported content, and `None` is not an error.
///
/// Read from git files, not a subprocess, so opening imports stays cheap.
pub fn checkout_head_commit(checkout: &Path) -> Option<String> {
    fn is_commit_id(text: &str) -> bool {
        let text = text.trim();
        (text.len() == 40 || text.len() == 64) && text.bytes().all(|b| b.is_ascii_hexdigit())
    }

    let git_dir = git_dir_of(checkout)?;
    let head = fs::read_to_string(git_dir.join("HEAD"))
        .ok()?
        .trim()
        .to_string();
    // Detached HEAD names the commit directly.
    if is_commit_id(&head) {
        return Some(head);
    }
    let ref_name = head.strip_prefix("ref: ")?.trim();
    if let Ok(loose) = fs::read_to_string(git_dir.join(ref_name))
        && is_commit_id(&loose)
    {
        return Some(loose.trim().to_string());
    }
    // `^`-prefixed peel lines fail the shape check and drop out.
    let packed = fs::read_to_string(git_dir.join("packed-refs")).ok()?;
    packed.lines().find_map(|line| {
        let (hash, name) = line.split_once(' ')?;
        (name.trim() == ref_name && is_commit_id(hash)).then(|| hash.to_string())
    })
}

// ---------------------------------------------------------------------------
// Pinned updates and removal
//

/// The namespaces whose changes steer agents directly; a pin moves only
/// after the importer has reviewed the diff of these.
pub const REVIEW_NAMESPACES: [&str; 2] = ["skill", "styleguide"];

/// File changes by area: the two review namespaces, and everything else,
/// for the summary `update` prints before asking to move the pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct ChangeCounts {
    /// Files added, removed, or changed under `skill/`.
    pub skill: usize,
    /// Files added, removed, or changed under `styleguide/`.
    pub styleguide: usize,
    /// Files changed anywhere else (manifest, documents, sidecar).
    pub other: usize,
}

impl ChangeCounts {
    /// Changes across the whole book.
    pub fn total(&self) -> usize {
        self.skill + self.styleguide + self.other
    }
}

/// What preparing an update found: the change between the checkout's pinned
/// commit and what its tracked ref resolves to now, with the diff an
/// importer must review.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UpdatePlan {
    /// The checkout name being updated.
    pub name: String,
    /// The URL the candidate was cloned from.
    pub url: String,
    /// The ref the update followed to find `to_commit`.
    pub reference: String,
    /// The checkout's current commit (`None` when it carries no git
    /// metadata — a plain tree).
    pub from_commit: Option<String>,
    /// The commit the tracked ref resolves to now.
    pub to_commit: String,
    /// Whether `from_commit` already is `to_commit` (nothing to apply).
    pub up_to_date: bool,
    /// Changed-file counts across the whole book.
    pub counts: ChangeCounts,
    /// Unified diff of `skill/` and `styleguide/` between the current tree
    /// and the candidate, with `## …` notes for namespaces that appear on
    /// only one side; empty when nothing changed.
    pub review_diff: String,
}

/// The outcome of [`prepare_update`].
#[derive(Debug)]
pub enum PreparedUpdate {
    /// The tracked ref already resolves to the checkout's commit; the plan
    /// is filled in but there is nothing to apply.
    UpToDate(UpdatePlan),
    /// A verified candidate sits beside the live checkout. Show
    /// [`Update::plan`], get the importer's confirmation, then
    /// [`Update::apply`]. Dropping the `Update` without applying it
    /// deletes the candidate and changes nothing.
    Ready(Update),
}

/// A prepared update holding its verified candidate checkout. The live
/// checkout is never touched until [`Update::apply`] succeeds.
#[derive(Debug)]
pub struct Update {
    root: PathBuf,
    name: String,
    candidate: PathBuf,
    plan: UpdatePlan,
}

impl Drop for Update {
    fn drop(&mut self) {
        // A no-op once `apply` renamed the candidate into place.
        discard_checkout(&self.candidate);
    }
}

/// Best-effort removal of a scratch checkout: its directory and the
/// integrity stamp written beside it.
fn discard_checkout(path: &Path) {
    let _ = fs::remove_dir_all(path);
    let _ = fs::remove_file(import_integrity_stamp(path));
}

impl Update {
    /// The prepared plan: what changed, and the diff to review.
    pub fn plan(&self) -> &UpdatePlan {
        &self.plan
    }

    /// Moves the pin: the live checkout is set aside, the verified
    /// candidate takes its place, and the pull-time integrity stamp is
    /// rewritten for the new tree. Any failure restores the previous
    /// checkout; the candidate is always cleaned up.
    ///
    /// # Errors
    ///
    /// [`Error::Io`](crate::error::Error) when a swap step fails (the
    /// previous checkout is restored), or the integrity-stamp write fails.
    pub fn apply(self) -> Result<UpdatePlan> {
        let current = self.root.join(&self.name);
        let backup = self
            .root
            .join(format!("{}.replaced-{}", self.name, std::process::id()));
        // A stale backup from an interrupted run must not block the rename.
        let _ = fs::remove_dir_all(&backup);
        fs::rename(&current, &backup).context(IoSnafu {
            path: current.clone(),
        })?;
        if let Err(error) = fs::rename(&self.candidate, &current) {
            let failure = IoSnafu {
                path: self.candidate.clone(),
            }
            .into_error(error);
            if fs::rename(&backup, &current).is_ok() {
                return Err(failure);
            }
            return ValidationSnafu {
                reason: format!(
                    "swapping the updated checkout in failed and the previous checkout \
                     could not be restored; both `{}` and `{}` remain on disk — inspect \
                     them before retrying",
                    backup.display(),
                    current.display()
                ),
            }
            .fail();
        }
        if let Err(error) = write_import_integrity_stamp(&current) {
            // The candidate is in place but unstamped; restore the old tree.
            let _ = fs::remove_dir_all(&current);
            let _ = fs::rename(&backup, &current);
            return Err(error);
        }
        let _ = fs::remove_dir_all(&backup);
        // The plan cannot move out of a Drop-holding self.
        Ok(self.plan.clone())
    }
}

/// Prepares an update of the checkout `name` in store `root`: resolves the
/// tracked `reference` at `url`, clones the candidate beside the live
/// checkout, verifies it exactly as `pull` would, and diffs the two trees.
/// Nothing on the live checkout changes until [`Update::apply`].
///
/// # Errors
///
/// [`Error::Validation`](crate::error::Error) for an unsafe name or URL, an
/// unresolvable ref, or a candidate that fails the pull-time checks (not an
/// argosy, integrity mismatch); [`Error::Io`](crate::error::Error) for
/// filesystem failures.
pub fn prepare_update(
    root: &Path,
    name: &str,
    url: &str,
    reference: &str,
) -> Result<PreparedUpdate> {
    ensure!(
        !url.is_empty() && !url.starts_with('-'),
        ValidationSnafu {
            reason: format!("invalid url `{url}`: pass a git URL or remote name, not an option"),
        }
    );
    ensure!(
        !reference.is_empty() && !reference.starts_with('-'),
        ValidationSnafu {
            reason: format!(
                "invalid ref `{reference}`: pass a branch, tag, or `HEAD`, not an option"
            ),
        }
    );
    ensure!(
        crate::bundle::is_safe_bundle_name(name) && name != LOCAL_ARGOSY_NAME,
        ValidationSnafu {
            reason: format!("invalid checkout name `{name}`")
        }
    );
    let current = root.join(name);
    ensure!(
        crate::bundle::is_real_dir(&current),
        ValidationSnafu {
            reason: format!("no checkout `{}` in `{}` to update", name, root.display())
        }
    );
    let to_commit = resolve_remote_commit(url, reference)?;
    let from_commit = checkout_head_commit(&current);
    let up_to_date = from_commit.as_deref() == Some(to_commit.as_str());
    let plan_of = |counts: ChangeCounts, review_diff: String| UpdatePlan {
        name: name.to_string(),
        url: url.to_string(),
        reference: reference.to_string(),
        from_commit: from_commit.clone(),
        to_commit: to_commit.clone(),
        up_to_date,
        counts,
        review_diff,
    };
    if up_to_date {
        return Ok(PreparedUpdate::UpToDate(plan_of(
            ChangeCounts::default(),
            String::new(),
        )));
    }

    let candidate = unique_sibling(root, name, ".update-")?;
    let cleanup_candidate = candidate.clone();
    let prepared = (|| -> Result<Update> {
        clone_url_to(url, &candidate)?;
        let pin = to_commit.as_str();
        if let Err(error) = git_in_dir(&candidate, &["checkout", "--quiet", "--detach", pin]) {
            return Err(match error {
                crate::error::Error::Validation { reason } => ValidationSnafu {
                    reason: format!(
                        "commit `{pin}` could not be checked out from `{url}` (it may have \
                         been force-pushed away upstream): {reason}"
                    ),
                }
                .build(),
                other => other,
            });
        }
        accept_checkout(&candidate)?;
        let counts = change_counts(&current, &candidate)?;
        let review_diff = review_diff(root, name, &candidate)?;
        Ok(Update {
            root: root.to_path_buf(),
            name: name.to_string(),
            candidate,
            plan: plan_of(counts, review_diff),
        })
    })();
    match prepared {
        Ok(update) => Ok(PreparedUpdate::Ready(update)),
        Err(error) => {
            discard_checkout(&cleanup_candidate);
            Err(error)
        }
    }
}

/// A fresh, unclaimed sibling path of `root/<name>`: `<name><suffix><pid>`
/// (plus a counter on collision), for update candidates and swap backups.
fn unique_sibling(root: &Path, name: &str, suffix: &str) -> Result<PathBuf> {
    for counter in 0.. {
        let candidate = root.join(format!("{name}{suffix}{}", std::process::id() + counter));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    unreachable!("an unclaimed sibling is found within u32 iterations")
}

/// Resolves `reference` at `url` to a commit id via `git ls-remote`,
/// preferring `refs/heads/<reference>` over `refs/tags/<reference>` when a
/// name exists as both, and never returning anything but a verified
/// hex object id.
///
/// # Errors
///
/// [`Error::Validation`](crate::error::Error) when the ref cannot be
/// resolved or resolves to something that is not an object id.
pub fn resolve_remote_commit(url: &str, reference: &str) -> Result<String> {
    ensure!(
        !url.is_empty() && !url.starts_with('-'),
        ValidationSnafu {
            reason: format!("invalid url `{url}`: pass a git URL or remote name, not an option"),
        }
    );
    ensure!(
        !reference.is_empty() && !reference.starts_with('-'),
        ValidationSnafu {
            reason: format!(
                "invalid ref `{reference}`: pass a branch, tag, or `HEAD`, not an option"
            ),
        }
    );
    let output = Command::new("git")
        .args(["ls-remote", url, reference])
        .output()
        .map_err(|err| git_spawn_error(root_cwd(), err))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return ValidationSnafu {
            reason: format!(
                "`git ls-remote {url} {reference}` failed ({}): {}",
                output.status,
                stderr.trim()
            ),
        }
        .fail();
    }
    // Lines are `<sha>\t<refname>`; `ls-remote` patterns match the ref tail,
    // so both refs/heads/x and refs/tags/x come back for `x`.
    let lines = String::from_utf8_lossy(&output.stdout);
    let mut heads: Option<String> = None;
    let mut tags: Option<String> = None;
    let mut exact: Option<String> = None;
    let mut any: Option<String> = None;
    for line in lines.lines() {
        let Some((sha, refname)) = line.split_once('\t') else {
            continue;
        };
        let (sha, refname) = (sha.trim(), refname.trim());
        if !(sha.len() == 40 || sha.len() == 64) || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        if refname == format!("refs/heads/{reference}") {
            heads = Some(sha.to_string());
        } else if refname == format!("refs/tags/{reference}") {
            tags = Some(sha.to_string());
        } else if refname == reference {
            exact = exact.or(Some(sha.to_string()));
        }
        any = any.or(Some(sha.to_string()));
    }
    heads.or(tags).or(exact).or(any).ok_or_else(|| {
        ValidationSnafu {
            reason: format!("`{reference}` does not resolve to a commit at `{url}`"),
        }
        .build()
    })
}

/// The default branch `url`'s HEAD points at (`"main"`, `"master"`), or
/// `"HEAD"` when the remote does not say.
///
/// # Errors
///
/// [`Error::Validation`](crate::error::Error) when `git ls-remote --symref`
/// fails or git is missing.
pub fn default_remote_branch(url: &str) -> Result<String> {
    ensure!(
        !url.is_empty() && !url.starts_with('-'),
        ValidationSnafu {
            reason: format!("invalid url `{url}`: pass a git URL or remote name, not an option"),
        }
    );
    let output = Command::new("git")
        .args(["ls-remote", "--symref", url, "HEAD"])
        .output()
        .map_err(|err| git_spawn_error(root_cwd(), err))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return ValidationSnafu {
            reason: format!(
                "`git ls-remote --symref {url} HEAD` failed ({}): {}",
                output.status,
                stderr.trim()
            ),
        }
        .fail();
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    // First line: `ref: refs/heads/main\tHEAD`.
    for line in stdout.lines() {
        if let Some(target) = line.trim().strip_prefix("ref:") {
            let target = target.split('\t').next().unwrap_or(target).trim();
            if let Some(branch) = target.strip_prefix("refs/heads/") {
                return Ok(branch.to_string());
            }
        }
    }
    Ok("HEAD".to_string())
}

/// A placeholder cwd for git spawn errors that have no natural directory.
fn root_cwd() -> &'static Path {
    Path::new("/")
}

/// The `origin` URL and tracked branch of an existing checkout, read from
/// git's own files so a checkout pulled before lockfiles existed can be
/// adopted into one. `None` when the checkout has no git metadata or no
/// `origin` remote; the ref falls back to `HEAD` for detached checkouts.
pub fn checkout_remote(checkout: &Path) -> Option<(String, String)> {
    let git_dir = git_dir_of(checkout)?;
    let config = fs::read_to_string(git_dir.join("config")).ok()?;

    let mut url: Option<String> = None;
    let mut merge_of: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut section = (String::new(), String::new());
    for line in config.lines() {
        let line = line.trim();
        if let Some(header) = line.strip_prefix('[') {
            let header = header.split(']').next().unwrap_or(header);
            // `[remote "origin"]` / `[branch "main"]` / `[core]`
            let mut parts = header.split_whitespace();
            section = match (parts.next(), parts.next()) {
                (Some(kind), Some(labeled)) => {
                    let label = labeled.trim_matches('"');
                    (kind.to_string(), label.to_string())
                }
                (Some(kind), None) => (kind.to_string(), String::new()),
                _ => (String::new(), String::new()),
            };
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let (key, value) = (key.trim(), value.trim());
            // Strip trailing `;`/`#` comments the way git config allows.
            let value = value
                .split([';', '#'])
                .next()
                .unwrap_or(value)
                .trim()
                .trim_matches('"');
            match section {
                (ref kind, ref label) if kind == "remote" && label == "origin" => {
                    if key == "url" && url.is_none() {
                        url = Some(value.to_string());
                    }
                }
                (ref kind, _) if kind == "branch" && key == "merge" => {
                    let branch = section.1.clone();
                    merge_of.insert(branch, value.to_string());
                }
                _ => {}
            }
        }
    }
    let url = url?;
    // The ref to follow: the branch HEAD sits on, when it tracks one.
    let head = fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    if let Some(ref_name) = head.strip_prefix("ref: ") {
        let branch = ref_name.trim().strip_prefix("refs/heads/")?;
        let reference = merge_of
            .get(branch)
            .and_then(|merge| merge.strip_prefix("refs/heads/"))
            .unwrap_or(branch);
        return Some((url, reference.to_string()));
    }
    Some((url, "HEAD".to_string()))
}

/// The `.git` directory of `checkout` (following one `gitdir:` indirection),
/// or `None` when it has no git metadata.
fn git_dir_of(checkout: &Path) -> Option<PathBuf> {
    let git = checkout.join(".git");
    let metadata = fs::symlink_metadata(&git).ok()?;
    if metadata.file_type().is_dir() {
        Some(git)
    } else {
        let pointer = fs::read_to_string(&git).ok()?;
        let target = pointer.strip_prefix("gitdir:")?.trim();
        Some(checkout.join(target))
    }
}

/// Deletes the checkout `name` from store `root` together with its
/// integrity stamp, returning the removed path. Works on broken checkouts
/// too — removing a tampered import to re-pull it is the documented repair
/// path, so no argosy.md is required.
///
/// # Errors
///
/// [`Error::Validation`](crate::error::Error) when `name` is unsafe or
/// reserved, or no such real directory exists.
pub fn remove_checkout(root: &Path, name: &str) -> Result<PathBuf> {
    ensure!(
        crate::bundle::is_safe_bundle_name(name),
        ValidationSnafu {
            reason: format!("invalid checkout name `{name}`")
        }
    );
    ensure!(
        name != LOCAL_ARGOSY_NAME,
        ValidationSnafu {
            reason: format!(
                "`{LOCAL_ARGOSY_NAME}` is the project-local bundle, not an import; \
                 delete its directory by hand if that is really intended"
            )
        }
    );
    ensure!(
        name != crate::lockfile::LOCKFILE_NAME,
        ValidationSnafu {
            reason: format!(
                "`{}` is the import lockfile, not a checkout",
                crate::lockfile::LOCKFILE_NAME
            )
        }
    );
    let dest = root.join(name);
    ensure!(
        dest.symlink_metadata()
            .is_ok_and(|m| m.file_type().is_dir()),
        ValidationSnafu {
            reason: format!(
                "no checkout `{}` in `{}` (a symlink is never a checkout)",
                name,
                root.display()
            )
        }
    );
    fs::remove_dir_all(&dest).context(IoSnafu { path: dest.clone() })?;
    let stamp = import_integrity_stamp(&dest);
    if stamp.exists() {
        fs::remove_file(&stamp).context(IoSnafu { path: stamp })?;
    }
    Ok(dest)
}

/// Every regular file under `root`, as sorted paths relative to it, with a
/// content hash per file. `.git/` and `.argosy/` directories are skipped at
/// any depth: they are git's own state and derived caches.
fn file_hashes(root: &Path) -> Result<std::collections::BTreeMap<PathBuf, String>> {
    fn walk(
        dir: &Path,
        rel: &Path,
        out: &mut std::collections::BTreeMap<PathBuf, String>,
    ) -> Result<()> {
        for entry in fs::read_dir(dir).context(IoSnafu {
            path: dir.to_path_buf(),
        })? {
            let entry = entry.context(IoSnafu {
                path: dir.to_path_buf(),
            })?;
            let name = entry.file_name();
            let path = entry.path();
            let file_type = entry.file_type().context(IoSnafu { path: path.clone() })?;
            if file_type.is_dir() {
                if name == ".git" || name == ".argosy" {
                    continue;
                }
                walk(&path, &rel.join(&name), out)?;
            } else if file_type.is_file() {
                let bytes = fs::read(&path).context(IoSnafu { path: path.clone() })?;
                out.insert(rel.join(&name), crate::hash::sha256_hex(&bytes));
            }
        }
        Ok(())
    }
    let mut out = std::collections::BTreeMap::new();
    if root.is_dir() {
        walk(root, Path::new(""), &mut out)?;
    }
    Ok(out)
}

/// Counts changed files between two trees (added, removed, or content
/// changed), bucketed by review namespace.
fn change_counts(old: &Path, new: &Path) -> Result<ChangeCounts> {
    let old_files = file_hashes(old)?;
    let new_files = file_hashes(new)?;
    let mut counts = ChangeCounts::default();
    for (rel, old_hash) in &old_files {
        let changed = match new_files.get(rel) {
            Some(new_hash) => new_hash != old_hash,
            None => true,
        };
        if changed {
            counts.bump(rel);
        }
    }
    for rel in new_files.keys() {
        if !old_files.contains_key(rel) {
            counts.bump(rel);
        }
    }
    Ok(counts)
}

impl ChangeCounts {
    /// Counts one changed file into its bucket, by first path component.
    fn bump(&mut self, rel: &Path) {
        match rel
            .components()
            .next()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
        {
            Some(first) if first == REVIEW_NAMESPACES[0] => self.skill += 1,
            Some(first) if first == REVIEW_NAMESPACES[1] => self.styleguide += 1,
            _ => self.other += 1,
        }
    }
}

/// The unified diff of the review namespaces between the checkout `name`
/// and the candidate `candidate` (both siblings under `root`), computed
/// tree against tree so it still works when upstream rewrote history and
/// the old commit no longer exists in the candidate's object store.
fn review_diff(root: &Path, name: &str, candidate: &Path) -> Result<String> {
    let candidate_name = candidate
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| candidate.display().to_string());
    let mut out = String::new();
    for ns in REVIEW_NAMESPACES {
        let old_arg = format!("{name}/{ns}");
        let new_arg = format!("{candidate_name}/{ns}");
        let old_dir = root.join(&old_arg);
        let new_dir = root.join(&new_arg);
        match (old_dir.is_dir(), new_dir.is_dir()) {
            (true, true) => {
                let output = Command::new("git")
                    .args(["diff", "--no-index"])
                    .arg(&old_arg)
                    .arg(&new_arg)
                    .current_dir(root)
                    .output()
                    .context(IoSnafu {
                        path: root.to_path_buf(),
                    })?;
                match output.status.code() {
                    Some(0) => {}
                    Some(1) => out.push_str(&normalize_diff_paths(
                        &String::from_utf8_lossy(&output.stdout),
                        &old_arg,
                        &new_arg,
                        ns,
                    )),
                    _ => {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        return ValidationSnafu {
                            reason: format!(
                                "`git diff --no-index {old_arg} {new_arg}` failed: {}",
                                stderr.trim()
                            ),
                        }
                        .fail();
                    }
                }
            }
            (false, true) => {
                let added = file_hashes(&new_dir).map(|files| files.len()).unwrap_or(0);
                out.push_str(&format!(
                    "## {ns}/ appears in the new commit and not in the current \
                     checkout ({added} file{} added wholesale)\n",
                    if added == 1 { "" } else { "s" }
                ));
            }
            (true, false) => {
                let removed = file_hashes(&old_dir).map(|files| files.len()).unwrap_or(0);
                out.push_str(&format!(
                    "## {ns}/ is removed wholesale in the new commit ({removed} file{})\n",
                    if removed == 1 { "" } else { "s" }
                ));
            }
            (false, false) => {}
        }
    }
    Ok(out)
}

/// Rewrites the path spellings of a `git diff --no-index` run into
/// `a/<ns>/…` / `b/<ns>/…` headers, so the review diff names book paths,
/// not state-dir scratch directories. Only diff header line shapes are
/// touched; body lines pass through verbatim.
fn normalize_diff_paths(diff: &str, old_arg: &str, new_arg: &str, ns: &str) -> String {
    let header = |line: &str| {
        line.starts_with("diff --git")
            || line.starts_with("--- ")
            || line.starts_with("+++ ")
            || line.starts_with("rename ")
            || line.starts_with("copy ")
            || line.starts_with("Binary files")
    };
    diff.lines()
        .map(|line| {
            if !header(line) {
                return line.to_string();
            }
            // Prefixed forms first; `new_arg` starts with `old_arg`'s name,
            // so bare forms are replaced newest-first.
            line.replace(&format!("a/{old_arg}"), &format!("a/{ns}"))
                .replace(&format!("b/{new_arg}"), &format!("b/{ns}"))
                .replace(new_arg, ns)
                .replace(old_arg, ns)
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use tempfile::TempDir;

    use super::*;

    /// Builds a minimal bundle at `dir` and commits it as a git repo.
    fn git_repo(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        fs::write(
            dir.join("argosy.md"),
            "---\ntype: Argosy Manifest\nname: remote-rules\nargosy_version: \"1.2.0\"\n---\n# remote-rules\n",
        )
        .unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?} failed");
        };
        git(&["init", "--quiet"]);
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "--quiet",
            "-m",
            "x",
        ]);
    }

    #[test]
    fn git_spawn_error_names_git_when_the_binary_is_missing() {
        // Regression: a failed spawn used to surface as a misleading I/O error
        // on the destination path.
        let err = git_spawn_error(
            Path::new("/tmp/dest"),
            std::io::Error::from(std::io::ErrorKind::NotFound),
        );
        let msg = err.to_string();
        assert!(msg.contains("`git` was not found on PATH"), "got {msg}");
        assert!(!msg.contains("dest"), "no misleading path: {msg}");

        // Any other spawn failure stays an I/O error on the destination.
        let err = git_spawn_error(
            Path::new("/tmp/dest"),
            std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        );
        assert!(matches!(err, crate::error::Error::Io { .. }), "got {err:?}");
    }

    /// `--upload-pack=<cmd>` and `ext::` transports execute commands.
    #[test]
    fn clone_as_checkout_refuses_option_like_urls() {
        for bad in ["--upload-pack=touch /tmp/pwned", "-", ""] {
            let err = clone_as_checkout(bad, Path::new("/tmp/none"), "name").unwrap_err();
            assert!(
                err.to_string().contains("invalid clone url"),
                "{bad:?}: {err}"
            );
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn home_state_base_is_posix_local_state() {
        assert_eq!(
            home_state_base(PathBuf::from("/home/u")),
            PathBuf::from("/home/u/.local/state")
        );
    }

    #[test]
    fn project_slug_is_name_plus_a_short_hash_of_the_absolute_path() {
        let a = Path::new("/home/me/Projects/craft");
        let slug = project_slug(a);
        assert!(
            slug.starts_with("craft-") && slug.len() == "craft-".len() + 8,
            "got {slug}"
        );
        assert!(
            slug["craft-".len()..]
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "hash is lowercase hex: {slug}"
        );
        assert_ne!(slug, project_slug(Path::new("/home/you/Projects/craft")));
        assert_eq!(slug, project_slug(a));
    }

    #[cfg(unix)]
    #[test]
    fn project_slug_canonicalizes_so_symlinked_spellings_share_one_slug() {
        let tmp = TempDir::new().unwrap();
        let real = tmp.path().join("real-project");
        fs::create_dir_all(&real).unwrap();
        let link = tmp.path().join("linked");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(project_slug(&real), project_slug(&link));
    }

    #[test]
    fn project_argosy_dir_at_nests_the_slug_under_projects() {
        let dir = project_argosy_dir_at(Path::new("/state/argosy"), "/home/me/Projects/craft");
        assert_eq!(
            dir.parent().unwrap(),
            Path::new("/state/argosy/projects"),
            "got {}",
            dir.display()
        );
        assert!(
            dir.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("craft-")),
            "got {}",
            dir.display()
        );
    }

    #[test]
    fn record_project_root_writes_and_reads_back_the_canonical_root() {
        let tmp = TempDir::new().unwrap();
        let state = tmp.path().join("state/argosy");
        let project = tmp.path().join("work/myproj");
        fs::create_dir_all(&project).unwrap();

        let slot = record_project_root_at(&state, &project).unwrap();
        assert_eq!(slot, project_argosy_dir_at(&state, &project));
        let recorded = recorded_project_root(&slot).unwrap();
        assert_eq!(recorded, canonical_project_root(&project));
        assert!(recorded.is_absolute());

        // A deleted project keeps its absolutized spelling so stale slots stay detectable.
        fs::remove_dir_all(&project).unwrap();
        assert_eq!(recorded_project_root(&slot), Some(recorded));
    }

    #[test]
    fn recorded_project_root_is_none_without_a_sidecar() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(recorded_project_root(tmp.path()), None);
    }

    #[test]
    fn clone_pulls_a_bundle_into_a_named_checkout() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        git_repo(&repo);
        let root = tmp.path().join("state/projects/craft-1a2b3c4d");

        let argosy = clone_as_checkout(repo.to_str().unwrap(), &root, "company-rules").unwrap();

        assert_eq!(argosy.manifest().name(), "remote-rules");
        assert_eq!(argosy.manifest().argosy_version().to_string(), "1.2.0");
        assert!(root.join("company-rules/argosy.md").is_file());
    }

    #[test]
    fn clone_refuses_an_existing_checkout_and_bad_names() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("store");
        fs::create_dir_all(root.join("taken")).unwrap();

        let err = clone_as_checkout("irrelevant", &root, "taken").unwrap_err();
        assert!(err.to_string().contains("refusing to overwrite"), "{err}");

        let err = clone_as_checkout("irrelevant", &root, "bad name").unwrap_err();
        assert!(err.to_string().contains("invalid checkout name"), "{err}");

        let err = clone_as_checkout("irrelevant", &root, LOCAL_ARGOSY_NAME).unwrap_err();
        assert!(err.to_string().contains("reserved"), "{err}");
        assert!(!root.join(LOCAL_ARGOSY_NAME).exists(), "nothing created");
    }

    #[test]
    fn clone_of_a_non_argosy_repo_leaves_no_checkout() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("plain-repo");
        fs::create_dir_all(&repo).unwrap();
        for (args, pwd) in [
            (vec!["init", "--quiet"], repo.clone()),
            (
                vec![
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "commit",
                    "--quiet",
                    "--allow-empty",
                    "-m",
                    "x",
                ],
                repo.clone(),
            ),
        ] {
            let out = Command::new("git")
                .args(&args)
                .current_dir(pwd)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?} failed");
        }

        let root = tmp.path().join("store");
        let err = clone_as_checkout(repo.to_str().unwrap(), &root, "notargosy").unwrap_err();

        assert!(err.to_string().contains("not an argosy"), "{err}");
        assert!(!root.join("notargosy").exists(), "partial checkout removed");
    }

    /// Builds a repo from a *packaged* bundle, so clones carry a sidecar.
    fn packaged_repo(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        let source = tempfile::tempdir().unwrap();
        fs::write(
            source.path().join("argosy.md"),
            "---\ntype: Argosy Manifest\nname: remote-rules\nargosy_version: \"1.2.0\"\n---\n# remote-rules\n",
        )
        .unwrap();
        fs::create_dir_all(source.path().join("document")).unwrap();
        fs::write(source.path().join("document/guide.md"), "Guide.\n").unwrap();
        crate::package::package(
            &crate::bundle::Argosy::open(source.path()).unwrap(),
            dir,
            &crate::package::PackageOptions::default(),
        )
        .unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?} failed");
        };
        git(&["init", "--quiet"]);
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "--quiet",
            "-m",
            "x",
        ]);
    }

    #[test]
    fn clone_accepts_a_repo_whose_sidecar_matches_its_contents() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("pkg-repo");
        packaged_repo(&repo);
        let root = tmp.path().join("store");

        let argosy = clone_as_checkout(repo.to_str().unwrap(), &root, "shared").unwrap();

        assert_eq!(argosy.manifest().name(), "remote-rules");
        assert_eq!(integrity_status(&root.join("shared")), "verified");
    }

    #[test]
    fn pull_records_an_integrity_stamp_beside_the_checkout() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("pkg-repo");
        packaged_repo(&repo);
        let root = tmp.path().join("store");

        clone_as_checkout(repo.to_str().unwrap(), &root, "shared").unwrap();

        // The stamp is the importer's record of the verified sidecar, outside the clone.
        let stamp = fs::read_to_string(import_integrity_stamp(&root.join("shared"))).unwrap();
        let sidecar =
            fs::read(root.join("shared").join(crate::package::INTEGRITY_FILENAME)).unwrap();
        assert_eq!(stamp, crate::hash::sha256_hex(&sidecar));
        assert_eq!(stamp.len(), 64, "a sha256 hex digest: {stamp}");

        assert!(verify_import_integrity(&root.join("shared")).is_ok());
    }

    #[test]
    fn a_sidecarless_pull_stamps_absent() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        git_repo(&repo); // hand-rolled: no sidecar
        let root = tmp.path().join("store");

        clone_as_checkout(repo.to_str().unwrap(), &root, "shared").unwrap();

        let stamp = fs::read_to_string(import_integrity_stamp(&root.join("shared"))).unwrap();
        assert_eq!(stamp, "absent");
        assert!(verify_import_integrity(&root.join("shared")).is_ok());
    }

    #[test]
    fn clone_refuses_a_tampered_sidecar_and_leaves_no_checkout() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("pkg-repo");
        packaged_repo(&repo);
        // Tamper after the commit — this is what a rug-pulled repo looks like.
        fs::write(repo.join("document/guide.md"), "Injected.\n").unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?} failed");
        };
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "--quiet",
            "-m",
            "tamper",
        ]);
        let root = tmp.path().join("store");

        let err = clone_as_checkout(repo.to_str().unwrap(), &root, "shared").unwrap_err();

        assert!(
            err.to_string().contains("integrity"),
            "error names the integrity failure: {err}"
        );
        assert!(
            !root.join("shared").exists(),
            "refused clone leaves nothing"
        );
    }

    #[test]
    fn a_clone_without_a_sidecar_is_accepted_with_nothing_to_verify() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        git_repo(&repo); // hand-rolled: no sidecar
        let root = tmp.path().join("store");

        let argosy = clone_as_checkout(repo.to_str().unwrap(), &root, "shared").unwrap();

        assert_eq!(argosy.manifest().name(), "remote-rules");
        assert_eq!(integrity_status(&root.join("shared")), "no sidecar");
    }

    #[test]
    fn a_clone_reports_its_head_commit() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        git_repo(&repo);
        let root = tmp.path().join("store");
        clone_as_checkout(repo.to_str().unwrap(), &root, "shared").unwrap();

        let commit = checkout_head_commit(&root.join("shared")).unwrap();
        assert_eq!(commit.len(), 40, "a sha1 object id: {commit}");
        assert!(
            commit.bytes().all(|b| b.is_ascii_hexdigit()),
            "lowercase hex: {commit}"
        );
        // It is genuinely the remote's HEAD, not a ref spelling.
        let out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert_eq!(commit, String::from_utf8_lossy(&out.stdout).trim());
    }

    #[test]
    fn head_commit_resolves_a_packed_ref() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        git_repo(&repo);
        let root = tmp.path().join("store");
        clone_as_checkout(repo.to_str().unwrap(), &root, "shared").unwrap();

        // After a server-side repack the branch lives only in packed-refs.
        let git_dir = root.join("shared/.git");
        let head = fs::read_to_string(git_dir.join("HEAD")).unwrap();
        let ref_name = head.trim().strip_prefix("ref: ").unwrap().to_string();
        let loose = fs::read_to_string(git_dir.join(&ref_name))
            .unwrap()
            .trim()
            .to_string();
        fs::remove_file(git_dir.join(&ref_name)).unwrap();
        fs::write(
            git_dir.join("packed-refs"),
            format!("# pack-refs-with: peeled fully-peeled sorted\n{loose} {ref_name}\n"),
        )
        .unwrap();

        assert_eq!(
            checkout_head_commit(&root.join("shared")).unwrap(),
            loose,
            "packed-refs answers when the loose ref is gone"
        );
    }

    #[test]
    fn a_detached_head_reports_the_commit_directly() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        git_repo(&repo);
        let root = tmp.path().join("store");
        clone_as_checkout(repo.to_str().unwrap(), &root, "shared").unwrap();
        let out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(root.join("shared"))
            .output()
            .unwrap();
        let commit = String::from_utf8_lossy(&out.stdout).trim().to_string();
        fs::write(root.join("shared/.git/HEAD"), &commit).unwrap();

        assert_eq!(checkout_head_commit(&root.join("shared")).unwrap(), commit);
    }

    #[test]
    fn a_checkout_without_git_metadata_reports_no_commit() {
        let tmp = TempDir::new().unwrap();
        // A plain tree (hosts point imports at these) and garbage HEADs.
        fs::create_dir_all(tmp.path().join("plain/argosy.md").parent().unwrap()).unwrap();
        assert_eq!(checkout_head_commit(&tmp.path().join("plain")), None);

        let junk = tmp.path().join("junk");
        fs::create_dir_all(junk.join(".git")).unwrap();
        fs::write(junk.join(".git/HEAD"), "not a ref\n").unwrap();
        assert_eq!(checkout_head_commit(&junk), None);
    }

    /// Runs git in `dir`, panicking with git's stderr on failure.
    fn run_git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn commit_all(dir: &Path, message: &str) {
        run_git(
            dir,
            &["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"],
        );
        run_git(
            dir,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "--quiet",
                "-m",
                message,
            ],
        );
    }

    fn head_sha(dir: &Path) -> String {
        let out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A book repo on branch `main` with one commit and a skill file.
    fn book_repo(dir: &Path) -> String {
        fs::create_dir_all(dir.join("skill")).unwrap();
        fs::write(
            dir.join("argosy.md"),
            "---\ntype: Argosy Manifest\nname: remote-rules\nargosy_version: \"1.2.0\"\n---\n# remote-rules\n",
        )
        .unwrap();
        fs::write(dir.join("skill/one.md"), "old line\n").unwrap();
        run_git(dir, &["init", "--quiet", "-b", "main"]);
        commit_all(dir, "one");
        dir.to_string_lossy().into_owned()
    }

    #[test]
    fn update_previews_the_review_diff_and_moves_the_pin() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        let url = book_repo(&repo);
        let root = tmp.path().join("store");
        clone_as_checkout(&url, &root, "shared").unwrap();
        let first = head_sha(&repo);

        // Upstream moves: a skill edit, a new rule, and a document change.
        fs::write(repo.join("skill/one.md"), "new line\n").unwrap();
        fs::create_dir_all(repo.join("styleguide")).unwrap();
        fs::write(repo.join("styleguide/rule.md"), "# rule\n").unwrap();
        fs::write(repo.join("argosy.md.new"), "scratch\n").unwrap();
        fs::remove_file(repo.join("argosy.md.new")).unwrap();
        fs::create_dir_all(repo.join("document")).unwrap();
        fs::write(repo.join("document/guide.md"), "Guide.\n").unwrap();
        commit_all(&repo, "two");
        let second = head_sha(&repo);

        let prepared = prepare_update(&root, "shared", &url, "main").unwrap();
        let PreparedUpdate::Ready(update) = prepared else {
            panic!("a moved ref must prepare a Ready update");
        };
        let plan = update.plan();
        assert_eq!(plan.from_commit.as_deref(), Some(first.as_str()));
        assert_eq!(plan.to_commit, second);
        assert!(!plan.up_to_date);
        assert_eq!(plan.counts.skill, 1, "{:?}", plan.counts);
        assert_eq!(plan.counts.styleguide, 1, "{:?}", plan.counts);
        assert_eq!(plan.counts.other, 1, "{:?}", plan.counts);
        assert!(
            plan.review_diff.contains("skill/one.md"),
            "{}",
            plan.review_diff
        );
        // `styleguide/` did not exist before, so it reports wholesale.
        assert!(
            plan.review_diff
                .contains("## styleguide/ appears in the new commit"),
            "{}",
            plan.review_diff
        );
        // Headers name book paths, not state-dir scratch directories.
        assert!(
            !plan.review_diff.contains(".update-"),
            "no scratch paths leak into headers: {}",
            plan.review_diff
        );

        let applied = update.apply().unwrap();
        assert_eq!(applied.to_commit, second);
        assert_eq!(
            checkout_head_commit(&root.join("shared")).as_deref(),
            Some(second.as_str())
        );
        assert!(verify_import_integrity(&root.join("shared")).is_ok());
        let leftovers: Vec<String> = fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("shared.update-") || n.starts_with("shared.replaced-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "no candidate or backup dirs: {leftovers:?}"
        );

        // The ref has not moved again: preparing once more is UpToDate.
        match prepare_update(&root, "shared", &url, "main").unwrap() {
            PreparedUpdate::UpToDate(plan) => assert!(plan.up_to_date),
            PreparedUpdate::Ready(_) => panic!("no change left to apply"),
        }
    }

    #[test]
    fn dropping_a_prepared_update_changes_nothing() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        let url = book_repo(&repo);
        let root = tmp.path().join("store");
        clone_as_checkout(&url, &root, "shared").unwrap();
        let pinned = head_sha(&repo);

        fs::write(repo.join("skill/one.md"), "new line\n").unwrap();
        commit_all(&repo, "two");

        let prepared = prepare_update(&root, "shared", &url, "main").unwrap();
        let PreparedUpdate::Ready(update) = prepared else {
            panic!("expected a Ready update");
        };
        drop(update);

        assert_eq!(
            checkout_head_commit(&root.join("shared")).as_deref(),
            Some(pinned.as_str()),
            "the pin did not move"
        );
        let leftovers: Vec<String> = fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("shared.update-") || n.starts_with("shared.replaced-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "candidate cleaned on drop: {leftovers:?}"
        );
    }

    #[test]
    fn update_refuses_a_tampered_candidate_and_keeps_the_old_checkout() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("pkg-repo");
        packaged_repo(&repo);
        let root = tmp.path().join("store");
        clone_as_checkout(repo.to_str().unwrap(), &root, "shared").unwrap();

        // Upstream commits content that disagrees with its own sidecar.
        fs::write(repo.join("document/guide.md"), "Injected.\n").unwrap();
        commit_all(&repo, "tamper");

        let err = prepare_update(&root, "shared", repo.to_str().unwrap(), "main").unwrap_err();
        assert!(err.to_string().contains("integrity"), "{err}");

        // The live checkout is untouched and still verifies.
        assert!(verify_import_integrity(&root.join("shared")).is_ok());
        let leftovers: Vec<String> = fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("shared.update-") || n.starts_with("shared.replaced-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "refused candidate removed: {leftovers:?}"
        );
    }

    #[test]
    fn update_diffs_trees_so_a_force_push_cannot_hide_the_change() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        let url = book_repo(&repo);
        let root = tmp.path().join("store");
        clone_as_checkout(&url, &root, "shared").unwrap();

        // Rewrite history: the old commit is no longer reachable upstream.
        fs::write(repo.join("skill/one.md"), "rewritten\n").unwrap();
        commit_all(&repo, "two");
        run_git(
            &repo,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "--quiet",
                "--amend",
                "-m",
                "rewritten history",
            ],
        );

        let prepared = prepare_update(&root, "shared", &url, "main").unwrap();
        let PreparedUpdate::Ready(update) = prepared else {
            panic!("a rewritten remote must still prepare a Ready update");
        };
        let plan = update.plan();
        assert!(
            plan.review_diff.contains("skill/one.md"),
            "{}",
            plan.review_diff
        );
        assert_eq!(plan.counts.skill, 1, "{:?}", plan.counts);
        assert_ne!(plan.from_commit.as_deref(), Some(plan.to_commit.as_str()));
        update.apply().unwrap();
        assert!(verify_import_integrity(&root.join("shared")).is_ok());
    }

    #[test]
    fn clone_at_a_pin_reproduces_the_pinned_commit() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        let url = book_repo(&repo);
        let first = head_sha(&repo);
        fs::write(repo.join("skill/one.md"), "second\n").unwrap();
        commit_all(&repo, "two");

        let root = tmp.path().join("store");
        let argosy = clone_as_checkout_at(&url, &root, "shared", &first).unwrap();

        assert_eq!(argosy.manifest().name(), "remote-rules");
        assert_eq!(
            checkout_head_commit(&root.join("shared")).as_deref(),
            Some(first.as_str()),
            "the pin wins over the remote's HEAD"
        );
    }

    #[test]
    fn clone_at_a_vanished_pin_fails_loudly() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        let url = book_repo(&repo);
        let first = head_sha(&repo);
        // Orphan branch: `first` leaves the fetched history entirely.
        run_git(&repo, &["checkout", "--quiet", "--orphan", "fresh"]);
        fs::write(repo.join("skill/one.md"), "unrelated\n").unwrap();
        commit_all(&repo, "unrelated");
        run_git(&repo, &["branch", "-D", "main"]);
        // A local clone hardlinks every object, reachable or not; prune so
        // the vanished pin is genuinely absent.
        run_git(&repo, &["reflog", "expire", "--expire=now", "--all"]);
        run_git(&repo, &["gc", "--quiet", "--prune=now"]);

        let root = tmp.path().join("store");
        let err = clone_as_checkout_at(&url, &root, "shared", &first).unwrap_err();

        assert!(
            err.to_string().contains("force-pushed"),
            "the error explains the rug-pull shape: {err}"
        );
        assert!(!root.join("shared").exists(), "nothing left behind");
    }

    #[test]
    fn remove_checkout_deletes_the_dir_and_its_stamp() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        let url = book_repo(&repo);
        let root = tmp.path().join("store");
        clone_as_checkout(&url, &root, "shared").unwrap();
        assert!(import_integrity_stamp(&root.join("shared")).exists());

        let removed = remove_checkout(&root, "shared").unwrap();

        assert_eq!(removed, root.join("shared"));
        assert!(!root.join("shared").exists());
        assert!(!import_integrity_stamp(&root.join("shared")).exists());
    }

    #[test]
    fn remove_checkout_refuses_reserved_and_missing_names() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("store");
        fs::create_dir_all(&root).unwrap();

        let err = remove_checkout(&root, LOCAL_ARGOSY_NAME).unwrap_err();
        assert!(err.to_string().contains("project-local bundle"), "{err}");

        let err = remove_checkout(&root, crate::lockfile::LOCKFILE_NAME).unwrap_err();
        assert!(err.to_string().contains("lockfile"), "{err}");

        let err = remove_checkout(&root, "nope").unwrap_err();
        assert!(err.to_string().contains("no checkout"), "{err}");

        let err = remove_checkout(&root, "bad/name").unwrap_err();
        assert!(err.to_string().contains("invalid checkout name"), "{err}");
    }

    #[test]
    fn checkout_remote_reads_origin_and_tracking_branch() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        let url = book_repo(&repo);
        let root = tmp.path().join("store");
        clone_as_checkout(&url, &root, "shared").unwrap();

        let (found_url, reference) = checkout_remote(&root.join("shared")).unwrap();

        assert_eq!(found_url, url);
        assert_eq!(reference, "main");
    }

    #[test]
    fn checkout_remote_of_a_plain_tree_is_none() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("plain")).unwrap();
        assert_eq!(checkout_remote(&tmp.path().join("plain")), None);
    }

    #[test]
    fn resolve_remote_commit_prefers_a_branch_over_a_same_named_tag() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        let url = book_repo(&repo);
        let first = head_sha(&repo);
        run_git(&repo, &["tag", "main", &first]);
        fs::write(repo.join("skill/one.md"), "second\n").unwrap();
        commit_all(&repo, "two");
        let second = head_sha(&repo);

        assert_eq!(resolve_remote_commit(&url, "main").unwrap(), second);
        assert_eq!(resolve_remote_commit(&url, "HEAD").unwrap(), second);
        assert!(
            resolve_remote_commit(&url, "no-such-ref").is_err(),
            "an unresolvable ref is an error, not a guess"
        );
    }

    #[test]
    fn default_remote_branch_names_the_heads_branch() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        let url = book_repo(&repo);

        assert_eq!(default_remote_branch(&url).unwrap(), "main");
    }

    #[test]
    fn normalize_diff_paths_rewrites_only_headers() {
        let diff = "diff --git a/shared/skill/one.md b/shared.update-1/skill/one.md\n\
                    index 0906fba..86ba82a 100644\n\
                    --- a/shared/skill/one.md\n\
                    +++ b/shared.update-1/skill/one.md\n\
                    @@ -1 +1 @@\n\
                    -old\n\
                    +a/shared/skill/one.md stays in the body\n";

        let normalized =
            normalize_diff_paths(diff, "shared/skill", "shared.update-1/skill", "skill");

        assert!(normalized.contains("diff --git a/skill/one.md b/skill/one.md"));
        assert!(normalized.contains("--- a/skill/one.md"));
        assert!(normalized.contains("+++ b/skill/one.md"));
        assert!(
            normalized.contains("+a/shared/skill/one.md stays in the body"),
            "body lines pass through verbatim: {normalized}"
        );
    }
}
