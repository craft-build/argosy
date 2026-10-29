//! The import lockfile (`argosy.lock`): one per checkout store, pinning
//! every import to the exact commit that was verified when it was accepted.
//!
//! The lockfile lives beside the checkouts in the user's state directory
//! (a project's slot or the global store), never in the project tree.
//! Reproducing a store on another machine means carrying the lockfile
//! there and running `argosy sync --from`. Entries record the URL an
//! import came from, the ref updates follow, the resolved commit, and
//! what integrity verification saw; a pin moves only when the importer
//! reviews the change (see [`crate::pull`] for the update path).
//!
//! ```bml
//! schema = 1
//! import {
//!     "company-rules" {
//!         url = "https://github.com/acme/argosy-rules"
//!         reference = "main"
//!         commit = "9f1c0b7…"
//!         integrity = "verified"     // or "no sidecar"
//!     }
//! }
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use snafu::{IntoError, ResultExt};

use crate::error::{IoSnafu, Result, ValidationSnafu};
use crate::pull::{LOCAL_ARGOSY_NAME, integrity_status_strings};

/// The lockfile name inside a checkout store.
pub const LOCKFILE_NAME: &str = "argosy.lock";

/// The only lockfile schema version written and accepted so far.
pub const LOCKFILE_VERSION: u32 = 1;

/// One pinned import: what was cloned, what to follow, and the exact
/// commit whose integrity was verified on acceptance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockEntry {
    /// The git URL the import was cloned from; `update` re-clones it.
    pub url: String,
    /// The ref updates follow (`main`, a tag, or `HEAD` when the default
    /// branch could not be named). The pin itself is `commit`.
    pub reference: String,
    /// The resolved commit the checkout is pinned to, lowercase hex.
    pub commit: String,
    /// What integrity verification saw at acceptance: `verified` (a
    /// sidecar was present and matched) or `no sidecar`.
    pub integrity: String,
}

/// The parsed content of one store's `argosy.lock`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lockfile {
    /// Schema version; only [`LOCKFILE_VERSION`] is accepted (`version` is
    /// a BarkML type keyword, so the field is `schema`).
    pub schema: u32,
    /// Pins keyed by checkout name (the directory name in the store). May
    /// be absent: a store whose pins were all removed writes `schema = 1`
    /// alone.
    #[serde(default)]
    pub import: BTreeMap<String, LockEntry>,
}

impl Lockfile {
    /// An empty lockfile: what a store with no pins holds.
    pub fn empty() -> Self {
        Self {
            schema: LOCKFILE_VERSION,
            import: BTreeMap::new(),
        }
    }

    /// Whether the store holds no pins.
    pub fn is_empty(&self) -> bool {
        self.import.is_empty()
    }

    /// The pin recorded for checkout `name`, if any.
    pub fn entry(&self, name: &str) -> Option<&LockEntry> {
        self.import.get(name)
    }

    /// All pins, keyed by checkout name in sorted order.
    pub fn entries(&self) -> &BTreeMap<String, LockEntry> {
        &self.import
    }

    /// Records or replaces the pin for checkout `name`.
    pub fn upsert(&mut self, name: impl Into<String>, entry: LockEntry) {
        self.import.insert(name.into(), entry);
    }

    /// Drops the pin for checkout `name`, returning it if it existed.
    pub fn remove(&mut self, name: &str) -> Option<LockEntry> {
        self.import.remove(name)
    }

    /// The lockfile path inside store `root`.
    pub fn path(root: &Path) -> PathBuf {
        root.join(LOCKFILE_NAME)
    }

    /// Reads the lockfile for the store rooted at `root`; a missing file is
    /// an empty lockfile, an unreadable or malformed one is an error. Pins
    /// guard what gets served, so a lockfile that cannot be understood is
    /// never silently ignored.
    ///
    /// # Errors
    ///
    /// [`Error::Io`](crate::error::Error) when the file cannot be read, or
    /// [`Error::Validation`](crate::error::Error) when it is not valid
    /// BarkML, carries an unsupported version, or holds an invalid entry.
    pub fn load(root: &Path) -> Result<Self> {
        Self::load_from(&Self::path(root))
    }

    /// [`Lockfile::load`] against an explicit lockfile path.
    pub fn load_from(path: &Path) -> Result<Self> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::empty());
            }
            Err(source) => {
                return Err(IoSnafu {
                    path: path.to_path_buf(),
                }
                .into_error(source));
            }
        };
        let statement = barkml::from_str(&text).map_err(|error| {
            ValidationSnafu {
                reason: format!("failed to parse `{}`: {error}", path.display()),
            }
            .build()
        })?;
        let lockfile: Self = barkml::de::from_statement(&statement).map_err(|error| {
            ValidationSnafu {
                reason: format!("failed to read `{}` as a lockfile: {error}", path.display()),
            }
            .build()
        })?;
        lockfile.validate(path)?;
        // Git prints lowercase object ids everywhere; folding here keeps
        // string comparisons against HEAD and ls-remote exact.
        let mut lockfile = lockfile;
        for entry in lockfile.import.values_mut() {
            entry.commit = entry.commit.to_lowercase();
        }
        Ok(lockfile)
    }

    /// Validates schema and every entry; `Err` names the file and field.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`](crate::error::Error) naming the first problem
    /// found: unsupported version, an unsafe checkout name, an
    /// option-shaped URL or ref, a non-hex commit, or an unknown integrity
    /// value.
    pub fn validate(&self, path: &Path) -> Result<()> {
        let at = |reason: String| ValidationSnafu {
            reason: format!("`{}`: {reason}", path.display()),
        };
        if self.schema != LOCKFILE_VERSION {
            return at(format!(
                "unsupported lockfile schema {} (expected {LOCKFILE_VERSION}); \
                 remove the file and re-pull the imports to rebuild it",
                self.schema
            ))
            .fail();
        }
        for (name, entry) in &self.import {
            if !crate::pull::is_safe_checkout_name(name) {
                return at(format!(
                    "import `{name}` is not a safe checkout name \
                     (allowed: [A-Za-z0-9._-], no leading `-`)"
                ))
                .fail();
            }
            if name == LOCAL_ARGOSY_NAME {
                return at(format!(
                    "import `{LOCAL_ARGOSY_NAME}` collides with the reserved \
                     local-bundle checkout name"
                ))
                .fail();
            }
            for (field, value) in [("url", &entry.url), ("reference", &entry.reference)] {
                if value.is_empty()
                    || value.starts_with('-')
                    || value.chars().any(|c| c.is_control())
                {
                    return at(format!(
                        "import `{name}`: `{field}` must be a non-empty value that does \
                         not start with `-` or contain control characters (got `{value}`)"
                    ))
                    .fail();
                }
            }
            let hex = entry.commit.len() == 40 || entry.commit.len() == 64;
            if !hex || !entry.commit.bytes().all(|b| b.is_ascii_hexdigit()) {
                return at(format!(
                    "import `{name}`: `commit` must be a 40- or 64-hex-digit \
                     object id (got `{}`)",
                    entry.commit
                ))
                .fail();
            }
            if !integrity_status_strings().contains(&entry.integrity.as_str()) {
                return at(format!(
                    "import `{name}`: `integrity` must be one of {} (got `{}`)",
                    integrity_status_strings().join(" or "),
                    entry.integrity
                ))
                .fail();
            }
        }
        Ok(())
    }

    /// Renders the lockfile to BarkML text: one `import { … }` block whose
    /// entries are quoted-key sub-blocks, sorted by name so the file diffs
    /// stably pin by pin. Hand-rendered because barkml's serializer emits
    /// type annotations its own parser rejects for table values.
    pub fn render(&self) -> String {
        fn quote(value: &str) -> String {
            // `unescape_strict` accepts `\"` and `\\`.
            format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
        }
        let mut out = format!("schema = {LOCKFILE_VERSION}u32\n");
        if self.import.is_empty() {
            return out;
        }
        out.push_str("\nimport {\n");
        for (name, entry) in &self.import {
            out.push_str(&format!("    {} {{\n", quote(name)));
            out.push_str(&format!("        url = {}\n", quote(&entry.url)));
            out.push_str(&format!(
                "        reference = {}\n",
                quote(&entry.reference)
            ));
            out.push_str(&format!("        commit = {}\n", quote(&entry.commit)));
            out.push_str(&format!(
                "        integrity = {}\n",
                quote(&entry.integrity)
            ));
            out.push_str("    }\n");
        }
        out.push_str("}\n");
        out
    }

    /// Writes the lockfile into store `root` atomically: a temp file in the
    /// same directory, then a rename over the target, so a crash never
    /// leaves a half-written lockfile.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`](crate::error::Error) when rendering would
    /// write an invalid lockfile (validate runs first), or
    /// [`Error::Io`](crate::error::Error) when the write or rename fails.
    pub fn save(&self, root: &Path) -> Result<()> {
        let path = Self::path(root);
        self.validate(&path)?;
        let text = self.render();
        fs::create_dir_all(root).context(IoSnafu {
            path: root.to_path_buf(),
        })?;
        let path = Self::path(root);
        let temp = root.join(format!("{LOCKFILE_NAME}.tmp-{}", std::process::id()));
        if let Err(error) = fs::write(&temp, text.as_bytes())
            .and_then(|()| fs::rename(&temp, &path))
            .context(IoSnafu { path: temp.clone() })
        {
            let _ = fs::remove_file(&temp);
            return Err(error);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn pinned(commit: &str) -> LockEntry {
        LockEntry {
            url: "https://github.com/acme/rules".to_string(),
            reference: "main".to_string(),
            commit: commit.to_string(),
            integrity: "verified".to_string(),
        }
    }

    fn filled() -> Lockfile {
        let mut lock = Lockfile::empty();
        lock.upsert(
            "company-rules",
            pinned("0123456789abcdef0123456789abcdef01234567"),
        );
        lock.upsert(
            "zeta",
            LockEntry {
                url: "/local/path".to_string(),
                reference: "HEAD".to_string(),
                commit: "fedcba9876543210fedcba9876543210fedcba98".to_string(),
                integrity: "no sidecar".to_string(),
            },
        );
        lock
    }

    #[test]
    fn save_and_load_round_trip() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("store");
        let lock = filled();

        lock.save(&root).unwrap();
        let back = Lockfile::load(&root).unwrap();

        assert_eq!(back, lock);
        // Sorted keys, so the rendered file diffs stably entry by entry.
        let text = fs::read_to_string(Lockfile::path(&root)).unwrap();
        let company = text.find("company-rules").unwrap();
        let zeta = text.find("zeta").unwrap();
        assert!(company < zeta, "entries render in sorted order:\n{text}");
    }

    #[test]
    fn a_missing_lockfile_loads_empty() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(Lockfile::load(tmp.path()).unwrap(), Lockfile::empty());
        assert!(Lockfile::load(tmp.path()).unwrap().is_empty());
    }

    #[test]
    fn a_corrupt_lockfile_fails_closed() {
        let tmp = TempDir::new().unwrap();
        fs::write(Lockfile::path(tmp.path()), "import {").unwrap();

        let err = Lockfile::load(tmp.path()).unwrap_err();

        assert!(err.to_string().contains("failed to parse"), "{err}");
        assert!(
            err.to_string().contains("argosy.lock"),
            "the error names the file: {err}"
        );
    }

    #[test]
    fn save_leaves_no_temp_file_behind() {
        let tmp = TempDir::new().unwrap();
        filled().save(tmp.path()).unwrap();

        let leftovers: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n != LOCKFILE_NAME)
            .collect();
        assert!(leftovers.is_empty(), "leftover files: {leftovers:?}");
    }

    #[test]
    fn validation_rejects_bad_entries() {
        let tmp = TempDir::new().unwrap();
        let path = Lockfile::path(tmp.path());

        let mut bad_version = Lockfile::empty();
        bad_version.schema = 2;
        let err = bad_version.validate(&path).unwrap_err();
        assert!(err.to_string().contains("schema 2"), "{err}");

        let mut bad_name = Lockfile::empty();
        bad_name.upsert(
            "../escape",
            pinned("0123456789abcdef0123456789abcdef01234567"),
        );
        let err = bad_name.validate(&path).unwrap_err();
        assert!(err.to_string().contains("safe checkout name"), "{err}");

        let mut reserved = Lockfile::empty();
        reserved.upsert(
            "default",
            pinned("0123456789abcdef0123456789abcdef01234567"),
        );
        let err = reserved.validate(&path).unwrap_err();
        assert!(err.to_string().contains("reserved"), "{err}");

        let mut bad_url = Lockfile::empty();
        let mut entry = pinned("0123456789abcdef0123456789abcdef01234567");
        entry.url = "--upload-pack=evil".to_string();
        bad_url.upsert("shared", entry);
        let err = bad_url.validate(&path).unwrap_err();
        assert!(err.to_string().contains("`url`"), "{err}");

        let mut bad_commit = Lockfile::empty();
        bad_commit.upsert("shared", pinned("not-a-commit"));
        let err = bad_commit.validate(&path).unwrap_err();
        assert!(err.to_string().contains("`commit`"), "{err}");

        let mut bad_integrity = Lockfile::empty();
        let mut entry = pinned("0123456789abcdef0123456789abcdef01234567");
        entry.integrity = "trust me".to_string();
        bad_integrity.upsert("shared", entry);
        let err = bad_integrity.validate(&path).unwrap_err();
        assert!(err.to_string().contains("`integrity`"), "{err}");
    }

    #[test]
    fn load_from_accepts_an_externally_written_lockfile() {
        let tmp = TempDir::new().unwrap();
        let from = tmp.path().join("carried-over.lock");
        filled().save(tmp.path()).unwrap();
        fs::rename(Lockfile::path(tmp.path()), &from).unwrap();

        let lock = Lockfile::load_from(&from).unwrap();

        assert_eq!(lock.entry("company-rules").unwrap().reference, "main");
    }

    #[test]
    fn save_rejects_control_characters_in_values() {
        let tmp = TempDir::new().unwrap();
        let mut lock = Lockfile::empty();
        let mut entry = pinned("0123456789abcdef0123456789abcdef01234567");
        entry.url = "https://example.com/\ninject".to_string();
        lock.upsert("shared", entry);

        let err = lock.save(tmp.path()).unwrap_err();

        assert!(err.to_string().contains("control characters"), "{err}");
        assert!(!Lockfile::path(tmp.path()).exists(), "nothing written");
    }

    #[test]
    fn uppercase_hex_commits_fold_to_lowercase_on_load() {
        let tmp = TempDir::new().unwrap();
        let upper = "0123456789ABCDEF0123456789ABCDEF01234567";
        let mut lock = Lockfile::empty();
        lock.upsert(
            "shared",
            LockEntry {
                url: "https://example.com/rules".to_string(),
                reference: "main".to_string(),
                commit: upper.to_string(),
                integrity: "verified".to_string(),
            },
        );
        lock.save(tmp.path()).unwrap();

        let loaded = Lockfile::load(tmp.path()).unwrap();

        assert_eq!(
            loaded.entry("shared").unwrap().commit,
            upper.to_lowercase(),
            "string comparisons against git output stay exact"
        );
    }

    #[test]
    fn leading_dash_import_names_fail_validation() {
        let tmp = TempDir::new().unwrap();
        let mut lock = Lockfile::empty();
        lock.upsert(
            "-shared",
            pinned("0123456789abcdef0123456789abcdef01234567"),
        );

        let err = lock.validate(&Lockfile::path(tmp.path())).unwrap_err();

        assert!(err.to_string().contains("no leading `-`"), "{err}");
    }
}
