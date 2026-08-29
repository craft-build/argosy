//! Test-only helpers shared across modules' unit tests: fixture copying
//! into fresh tempdirs so tests never mutate `tests/fixtures/`.

use std::fs;
use std::path::Path;

use tempfile::TempDir;

fn copy_dir_all(src: &Path, dst: &Path) {
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            fs::create_dir_all(&to).unwrap();
            copy_dir_all(&entry.path(), &to);
        } else {
            fs::copy(entry.path(), to).unwrap();
        }
    }
}

/// Copies a shared fixture into a fresh tempdir — tests must never
/// mutate `tests/fixtures/` directly.
pub(crate) fn fixture_copy(name: &str) -> TempDir {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let dst = tempfile::tempdir().unwrap();
    copy_dir_all(&src, dst.path());
    dst
}
