//! Open/read-only/checkpoint lifecycle tests.

use super::*;

#[test]
fn open_read_only_reads_meta_after_a_writable_open_recorded_it() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("index.db");
    let mut store = SqliteVecStore::open(&db).unwrap();
    store.set_model_id("mock-embedder@1").unwrap();
    drop(store);

    let store = SqliteVecStore::open_read_only(&db).unwrap();

    assert_eq!(store.model_id(), Some("mock-embedder@1"));
    assert_eq!(store.unit_hashes().unwrap().len(), 0);
}

#[cfg(unix)]
#[test]
fn open_read_only_succeeds_where_open_fails_on_a_readonly_database() {
    use std::os::unix::fs::PermissionsExt;

    let (dir, store) = open_in_tmp();
    let db = dir.path().join(".argosy/index.db");
    drop(store);
    // A checkout/artifact with locked permissions (the `index
    // status` guarantee: read-only really means read-only).
    let mut perms = std::fs::metadata(&db).unwrap().permissions();
    perms.set_mode(0o444);
    std::fs::set_permissions(&db, perms).unwrap();

    assert!(
        SqliteVecStore::open(&db).is_err(),
        "the writable open must fail on a read-only file (its pragma/DDL writes)"
    );
    assert!(
        SqliteVecStore::open_read_only(&db).is_ok(),
        "the read-only open reads it"
    );
}

#[test]
fn open_read_only_on_a_missing_file_is_an_error() {
    let dir = TempDir::new().unwrap();
    let missing = dir.path().join("nope/index.db");
    assert!(SqliteVecStore::open_read_only(&missing).is_err());
}

#[test]
fn checkpoint_wal_fails_when_a_reader_reaches_into_the_wal() {
    use rusqlite::Connection;

    let (dir, mut store) = open_in_tmp();
    let db = dir.path().join(".argosy/index.db");
    store.set_model_id("mock-embedder@1").unwrap();

    // A reader pins an early WAL snapshot: the checkpoint can move
    // frames but cannot fully reset the WAL past this read-mark, so the
    // truncate must be reported incomplete rather than packaging a copy
    // that misses the second commit.
    let mut reader = Connection::open(&db).unwrap();
    let tx = reader.transaction().unwrap();
    let _: Option<String> = tx
        .query_row("SELECT model_id FROM meta WHERE id = 1", [], |r| r.get(0))
        .unwrap();
    store.set_model_id("mock-embedder@2").unwrap();

    let err = checkpoint_wal(&db).unwrap_err();
    assert!(
        err.to_string().contains("checkpoint is incomplete"),
        "unexpected error: {err}"
    );
    drop(tx);
    drop(reader);

    // Once the reader is gone, the same checkpoint completes.
    checkpoint_wal(&db).unwrap();
}
