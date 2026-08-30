//! The stale-read guard for the code tools' write paths, ported verbatim
//! from Craft's `tools/file_tracker.rs`: files read through these tools are
//! tracked by (canonical path, mtime), and `astgrep` apply / `conflicts`
//! resolve refuse to write when the file changed since the last read.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use tracing::warn;

const STALE_READ_MSG: &str = "file changed since last read";

pub struct FileReadTracker(Mutex<HashMap<PathBuf, SystemTime>>);

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

    pub fn check_before_edit(&self, path: &Path) -> Result<(), String> {
        let normalized = normalize_path(path);
        let mut guard = self.lock();
        let Some(&recorded) = guard.get(&normalized) else {
            return Ok(());
        };
        let Some(current) = get_mtime(&normalized) else {
            guard.remove(&normalized);
            return Ok(());
        };
        if recorded != current {
            return Err(format!(
                "{STALE_READ_MSG}: {} - re-read the file (e.g. zoom it) before editing",
                path.display(),
            ));
        }
        Ok(())
    }

    pub fn read_paths(&self) -> Vec<PathBuf> {
        self.lock().keys().cloned().collect()
    }
}

impl FileReadTracker {
    /// A poisoned lock only means some other call panicked mid-edit; the
    /// map itself is still consistent, so recover the guard instead of
    /// panicking on every later call (`NO-UNWRAP-IN-PROD`).
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
    fn untracked_file_allows_edit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.rs");
        fs::write(&path, "content").unwrap();

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
        let err = tracker.check_before_edit(&path).unwrap_err();
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
