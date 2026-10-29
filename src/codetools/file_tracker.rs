//! The stale-read guard for the code tools' write paths: tracks reads by
//! (canonical path, mtime) and refuses writes to files that changed since
//! the last read or were never read (fail closed).
//!
//! A file that does not exist may be written unread: the write creates it.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use tracing::warn;

use super::tool_error;
use crate::error::Result;

const STALE_READ_MSG: &str = "file changed since last read";
const NO_READ_MSG: &str = "no read on record";

pub struct FileReadTracker(Mutex<HashMap<PathBuf, SystemTime>>);

impl std::fmt::Debug for FileReadTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileReadTracker")
            .field("tracked", &self.lock().len())
            .finish_non_exhaustive()
    }
}

fn get_mtime(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn normalize_path(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

impl Default for FileReadTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl FileReadTracker {
    pub fn new() -> Self {
        Self(Mutex::new(HashMap::new()))
    }

    pub fn record_read(&self, path: &Path) {
        let normalized = normalize_path(path);
        match get_mtime(&normalized) {
            Some(mtime) => {
                self.lock().insert(normalized, mtime);
            }
            None => warn!(
                path = %path.display(),
                "record_read: could not get mtime, file will not be tracked"
            ),
        }
    }

    pub fn check_before_edit(&self, path: &Path) -> Result<()> {
        let normalized = normalize_path(path);
        let mut guard = self.lock();
        let Some(&recorded) = guard.get(&normalized) else {
            // Fail closed: a never-read file is refused — the writer has not
            // seen what it is about to overwrite. A new file clobbers nothing.
            if get_mtime(&normalized).is_some() {
                return Err(tool_error(format!(
                    "{NO_READ_MSG}: {} - read it through these tools first (zoom or outline it, \
                     or run astgrep/conflicts without apply/resolve), then retry",
                    path.display(),
                )));
            }
            return Ok(());
        };
        let Some(current) = get_mtime(&normalized) else {
            guard.remove(&normalized);
            return Ok(());
        };
        if recorded != current {
            return Err(tool_error(format!(
                "{STALE_READ_MSG}: {} - re-read the file (e.g. zoom it) before editing",
                path.display(),
            )));
        }
        Ok(())
    }

    pub fn read_paths(&self) -> Vec<PathBuf> {
        self.lock().keys().cloned().collect()
    }
}

impl FileReadTracker {
    /// A poisoned lock only means another call panicked mid-edit; the map is
    /// still consistent, so recover the guard instead of panicking.
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<PathBuf, SystemTime>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn future_mtime(path: &Path) {
        let future = SystemTime::now() + Duration::from_secs(10);
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(future)
            .unwrap();
    }

    #[test]
    fn untracked_file_on_disk_rejects_edit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.rs");
        fs::write(&path, "content").unwrap();

        let tracker = FileReadTracker::new();
        let err = tracker.check_before_edit(&path).unwrap_err().to_string();
        assert!(err.contains(NO_READ_MSG), "{err}");
    }

    /// The write creates the file, so there is nothing to clobber.
    #[test]
    fn untracked_missing_file_allows_edit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new.rs");

        let tracker = FileReadTracker::new();
        tracker.check_before_edit(&path).unwrap();
    }

    #[test]
    fn stale_read_rejects_edit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.rs");
        fs::write(&path, "original").unwrap();

        let tracker = FileReadTracker::new();
        tracker.record_read(&path);
        future_mtime(&path);
        let err = tracker.check_before_edit(&path).unwrap_err().to_string();
        assert!(err.contains(STALE_READ_MSG), "{err}");
    }

    #[test]
    fn deleted_file_allows_edit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.rs");
        fs::write(&path, "content").unwrap();

        let tracker = FileReadTracker::new();
        tracker.record_read(&path);
        fs::remove_file(&path).unwrap();
        tracker.check_before_edit(&path).unwrap();
    }

    #[test]
    fn re_read_after_change_allows_edit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.rs");
        fs::write(&path, "v1").unwrap();

        let tracker = FileReadTracker::new();
        tracker.record_read(&path);
        future_mtime(&path);
        tracker.record_read(&path);
        tracker.check_before_edit(&path).unwrap();
    }

    #[test]
    fn nonexistent_file_not_tracked() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ghost.rs");

        let tracker = FileReadTracker::new();
        tracker.record_read(&path);
        tracker.check_before_edit(&path).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn symlink_resolves_to_canonical() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.rs");
        let link = dir.path().join("link.rs");
        fs::write(&real, "content").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let tracker = FileReadTracker::new();
        tracker.record_read(&real);
        tracker.check_before_edit(&link).unwrap();
    }
}
