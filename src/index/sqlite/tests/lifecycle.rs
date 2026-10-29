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
    // Locked-permissions artifact: read-only really means read-only (the `index status` guarantee).
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

    // A reader pins an early WAL snapshot, so the truncate must be reported incomplete.
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

    checkpoint_wal(&db).unwrap();
}

#[test]
fn an_older_schema_is_cleared_and_rebuilt_on_writable_open() {
    use rusqlite::Connection;

    let dir = TempDir::new().unwrap();
    let db = dir.path().join("index.db");
    // Forge a v1 database: the pre-passage layout plus data.
    {
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "PRAGMA user_version = 1;
             CREATE TABLE meta (
                 id INTEGER PRIMARY KEY CHECK (id = 1),
                 model_id TEXT,
                 dimensions INTEGER,
                 created_at TEXT NOT NULL DEFAULT (datetime('now')),
                 updated_at TEXT NOT NULL DEFAULT (datetime('now'))
             );
             INSERT INTO meta (id, model_id, dimensions) VALUES (1, 'old-model@1', 3);
             CREATE TABLE units (
                 argosy TEXT NOT NULL,
                 namespace TEXT NOT NULL,
                 concept_id TEXT NOT NULL,
                 chunk_ordinal INTEGER NOT NULL,
                 text_hash TEXT NOT NULL,
                 concept_type TEXT,
                 description TEXT,
                 tags TEXT NOT NULL DEFAULT '[]',
                 language TEXT,
                 category TEXT,
                 PRIMARY KEY (argosy, namespace, concept_id, chunk_ordinal)
             );
             INSERT INTO units (argosy, namespace, concept_id, chunk_ordinal, text_hash)
                 VALUES ('local', 'document', 'document/a', 0, 'old-hash');",
        )
        .unwrap();
    }
    // The read-only path cannot migrate: it says how.
    let err = match SqliteVecStore::open_read_only(&db) {
        Ok(_) => panic!("the read-only open must refuse an older schema"),
        Err(err) => err,
    };
    assert!(
        format!("{err:#}").contains("argosy index build"),
        "got {err:#}"
    );

    // The writable open drops the stale layout (IDX-1: the index is a derived artifact).
    let store = SqliteVecStore::open(&db).unwrap();
    assert_eq!(store.model_id(), None, "the stale identity is released");
    assert_eq!(store.dimensions, None);
    assert!(store.unit_hashes().unwrap().is_empty());
    let version: i64 = store
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION as i64);
    let columns: Vec<String> = store
        .conn
        .prepare("SELECT name FROM pragma_table_info('units')")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<String>>>()
        .unwrap();
    assert!(
        ["heading_path", "start_line", "end_line", "passage"]
            .iter()
            .all(|c| columns.iter().any(|found| found == c)),
        "the v2 columns exist: {columns:?}"
    );
    // v3's lexical leg: the FTS index and its triggers are recreated, empty.
    let fts_objects: Vec<String> = store
        .conn
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE name IN ('unit_fts', 'units_fts_insert', 'units_fts_delete')",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<String>>>()
        .unwrap();
    assert_eq!(
        fts_objects.len(),
        3,
        "the FTS table and both sync triggers exist: {fts_objects:?}"
    );
    let hits = store.lexical_search("old", 10, &Filter::default()).unwrap();
    assert!(
        hits.is_empty(),
        "the dropped layout's text left no FTS shadow"
    );
}
