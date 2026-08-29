//! The `sqlite-vec`-backed default [`VectorStore`], gated behind the
//! `default-index` Cargo feature: one `.argosy/index.db` file spanning all
//! active argosys (`meta` model row, `units` facet rows, `unit_vectors`
//! vec0 keyed by units rowid). Scores are `-distance`. v1 limits: filters
//! apply after vec0's top-k truncation; single-process writers only.

use std::collections::{HashMap, HashSet};
use std::ffi::{c_char, c_int};
use std::fs;
use std::path::Path;
use std::sync::Once;

use rusqlite::types::ToSql;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use snafu::ResultExt;

use crate::bundle::Namespace;
use crate::concept::ConceptId;
use crate::context::QualifiedConceptId;
use crate::error::{Error, IndexSnafu, IoSnafu, Result, SqliteSnafu};

use super::{EmbeddingUnit, Filter, SearchHit, UnitMeta, VectorStore};

/// Schema generation (`PRAGMA user_version`); bump with a migration when the
/// layout changes.
const SCHEMA_VERSION: i32 = 1;

/// Pointer signature SQLite expects for an extension init function.
type SqliteExtensionInit = unsafe extern "C" fn(
    *mut rusqlite::ffi::sqlite3,
    *mut *mut c_char,
    *const rusqlite::ffi::sqlite3_api_routines,
) -> c_int;

static REGISTER_EXTENSION: Once = Once::new();

/// Registers sqlite-vec as an auto-extension so every subsequently opened
/// connection sees `vec0` virtual tables. Idempotent; must run before the
/// first [`Connection::open`].
fn register_extension() {
    REGISTER_EXTENSION.call_once(|| {
        // SAFETY: `sqlite3_vec_init` has the C signature SQLite requires of
        // an extension init function; the transmute reshapes the crate's
        // exported `extern "C" fn()` symbol into that type. SQLite stores
        // the pointer and invokes it on every connection open.
        unsafe {
            rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
                *const (),
                SqliteExtensionInit,
            >(
                sqlite_vec::sqlite3_vec_init as *const (),
            )));
        }
    });
}

/// Serializes a float vector into the native-byte-order blob the vec0
/// extension stores and matches against.
fn vec_to_bytes(vector: &[f32]) -> Vec<u8> {
    vector.iter().flat_map(|v| v.to_ne_bytes()).collect()
}

/// A [`VectorStore`] on a local `sqlite-vec` database file — the default
/// backend of the binary and MCP server behind `default-index`.
///
/// See the module docs for the schema, scoring convention, and limitations.
pub struct SqliteVecStore {
    conn: Connection,
    model_id: Option<String>,
    dimensions: Option<usize>,
}

impl SqliteVecStore {
    /// Opens (creating on first use) the store at `path`, creating parent
    /// directories as needed. Reopening an existing store restores the
    /// recorded `model_id` and vector dimensionality from `meta`, which is
    /// what lets a subsequent reconcile reuse the index with zero re-embeds.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        register_extension();
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).context(IoSnafu {
                path: parent.to_path_buf(),
            })?;
        }
        let conn = Connection::open(path).context(SqliteSnafu)?;
        // WAL: crash-safe single-writer operation (module docs: no
        // multi-process guarantees).
        conn.pragma_update(None, "journal_mode", "WAL")
            .context(SqliteSnafu)?;
        // Stamp the schema version on fresh databases only; refuse dbs from a
        // newer argosy rather than silently misreading their layout (and
        // never clobber a higher version, so a future migration can notice).
        let existing_version = Self::check_schema_version(&conn)?;
        if existing_version == 0 {
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)
                .context(SqliteSnafu)?;
        }
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                model_id TEXT,
                dimensions INTEGER,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            INSERT OR IGNORE INTO meta (id) VALUES (1);
            CREATE TABLE IF NOT EXISTS units (
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
            );",
        )
        .context(SqliteSnafu)?;

        let (model_id, dimensions) = Self::read_meta(&conn)?;
        if let Some(dims) = dimensions {
            ensure_vec_table(&conn, dims)?;
        }
        Ok(Self {
            conn,
            model_id,
            dimensions,
        })
    }

    /// Opens the store strictly read-only (the CLI's `index status`): no
    /// parent-directory creation, no WAL pragma, no schema DDL — the
    /// database must already exist and remain untouched. This is what lets
    /// `status` answer on a mounted-read-only or permission-locked index
    /// where [`Self::open`] would fail before reading a single row.
    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self> {
        register_extension();
        let conn =
            Connection::open_with_flags(path.as_ref(), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .context(SqliteSnafu)?;
        Self::check_schema_version(&conn)?;
        let (model_id, dimensions) = Self::read_meta(&conn)?;
        Ok(Self {
            conn,
            model_id,
            dimensions,
        })
    }

    /// Refuses databases stamped with a newer schema than this build; returns
    /// the on-disk `user_version` otherwise (read-only).
    fn check_schema_version(conn: &Connection) -> Result<i64> {
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .context(SqliteSnafu)?;
        if version > SCHEMA_VERSION as i64 {
            return IndexSnafu {
                reason: format!(
                    "index schema version {version} is newer than this build's {SCHEMA_VERSION}; upgrade argosy"
                ),
            }
            .fail();
        }
        Ok(version)
    }

    /// Reads the recorded model identity and vector dimensionality from
    /// `meta` (read-only).
    fn read_meta(conn: &Connection) -> Result<(Option<String>, Option<usize>)> {
        conn.query_row(
            "SELECT model_id, dimensions FROM meta WHERE id = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<i64>>(1)?.map(|d| d as usize),
                ))
            },
        )
        .optional()
        .context(SqliteSnafu)
        .map(|row| row.unwrap_or_default())
    }
}

/// Creates the `unit_vectors` vec0 table if absent. `dims` is an internal
/// usize, never user input, so interpolation is safe here.
fn ensure_vec_table(conn: &Connection, dims: usize) -> Result<()> {
    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'unit_vectors'",
            [],
            |row| row.get(0),
        )
        .context(SqliteSnafu)?;
    if exists == 0 {
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE unit_vectors USING vec0(vector float[{dims}])"
        ))
        .context(SqliteSnafu)?;
    }
    Ok(())
}

/// Forces a complete `TRUNCATE` WAL checkpoint on the store at `path`, so
/// a raw file copy (`package --include-index`) snapshots the full state
/// instead of a pre-checkpoint main file plus torn sidecars (excluded from
/// the copy). A no-op on missing or non-database files (magic-checked);
/// an incomplete checkpoint errors rather than packaging silently.
pub fn checkpoint_wal(path: &Path) -> Result<()> {
    use std::io::Read as _;

    const SQLITE_MAGIC: &[u8; 16] = b"SQLite format 3\0";
    let Ok(mut file) = fs::File::open(path) else {
        return Ok(());
    };
    let mut magic = [0u8; 16];
    if file.read_exact(&mut magic).is_err() || &magic != SQLITE_MAGIC {
        return Ok(());
    }
    let conn = Connection::open(path).context(SqliteSnafu)?;
    // `(busy, wal frames, checkpointed frames)`; `-1` log/ckpt = not a WAL
    // database (nothing to move, the copy is complete by definition).
    let (busy, log, checkpointed): (i64, i64, i64) = conn
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .context(SqliteSnafu)?;
    if busy != 0 || (log >= 0 && log != checkpointed) {
        return IndexSnafu {
            reason: format!(
                "cannot snapshot `{}` for packaging: the index's WAL checkpoint is incomplete ({checkpointed}/{log} frames checkpointed, busy={busy}); another process holds the index — close it or retry",
                path.display()
            ),
        }
        .fail();
    }
    Ok(())
}

/// Rebuilds the [`QualifiedConceptId`] of a `units` row, failing loudly on
/// a corrupt (unparseable) stored id rather than dropping the row silently.
fn row_key(argosy: String, namespace: String, concept_id: String) -> Result<QualifiedConceptId> {
    let id: ConceptId = concept_id.parse().map_err(|_| -> Error {
        IndexSnafu {
            reason: format!("corrupt index row: invalid stored concept id `{concept_id}`"),
        }
        .build()
    })?;
    Ok(QualifiedConceptId {
        argosy,
        namespace: Namespace::from_dir_name(&namespace),
        id,
    })
}

impl VectorStore for SqliteVecStore {
    fn model_id(&self) -> Option<&str> {
        self.model_id.as_deref()
    }

    fn set_model_id(&mut self, id: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE meta SET model_id = ?1, updated_at = datetime('now') WHERE id = 1",
                params![id],
            )
            .context(SqliteSnafu)?;
        self.model_id = Some(id.to_string());
        Ok(())
    }

    fn upsert(&mut self, units: &[EmbeddingUnit]) -> Result<()> {
        if units.is_empty() {
            return Ok(());
        }
        // Dimension safety: the vec0 table's width is fixed at creation
        // (first write, or re-established by `clear`); anything of a
        // different length is an error, never a silent truncation.
        let dims = self.dimensions.unwrap_or(units[0].vector.len());
        for unit in units {
            if unit.vector.len() != dims {
                return IndexSnafu {
                    reason: format!(
                        "vector of dimension {} does not match the store's dimension {dims} (store keeps untruncated vectors)",
                        unit.vector.len()
                    ),
                }
                .fail();
            }
        }

        let tx = self.conn.transaction().context(SqliteSnafu)?;
        // Stamp the store's dimensionality inside the batch transaction, so a
        // rolled-back insert never leaves `meta.dimensions` against an empty
        // store.
        if self.dimensions.is_none() {
            tx.execute(
                "UPDATE meta SET dimensions = ?1, updated_at = datetime('now') WHERE id = 1",
                params![dims as i64],
            )
            .context(SqliteSnafu)?;
            ensure_vec_table(&tx, dims)?;
        }
        {
            let mut delete_vec = tx
                .prepare_cached(
                    "DELETE FROM unit_vectors WHERE rowid IN (
                     SELECT rowid FROM units
                     WHERE argosy = ?1 AND namespace = ?2 AND concept_id = ?3
                 )",
                )
                .context(SqliteSnafu)?;
            let mut delete_units = tx
                .prepare_cached(
                    "DELETE FROM units
                     WHERE argosy = ?1 AND namespace = ?2 AND concept_id = ?3",
                )
                .context(SqliteSnafu)?;
            let mut insert_units = tx
                .prepare_cached(
                    "INSERT INTO units (argosy, namespace, concept_id, chunk_ordinal,
                                        text_hash, concept_type, description, tags,
                                        language, category)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                )
                .context(SqliteSnafu)?;
            let mut insert_vec = tx
                .prepare_cached("INSERT INTO unit_vectors (rowid, vector) VALUES (?1, ?2)")
                .context(SqliteSnafu)?;

            // vec0 has no UPDATE: re-upserting is delete-then-insert of the
            // concept's units and vec rows. The delete runs once per
            // CONCEPT, not per unit — a batch may hold several chunks, and
            // deleting per unit would wipe chunks inserted earlier here.
            let mut deleted: HashSet<(&str, &str, &str)> = HashSet::new();
            for unit in units {
                let namespace = unit.concept.namespace.as_dir_name();
                let concept_id = unit.concept.id.as_str();
                let key: &[&dyn ToSql] = &[&unit.concept.argosy, &namespace, &concept_id];
                if deleted.insert((&unit.concept.argosy, namespace, concept_id)) {
                    delete_vec.execute(key).context(SqliteSnafu)?;
                    delete_units.execute(key).context(SqliteSnafu)?;
                }
                let tags = serde_json::to_string(&unit.meta.tags).map_err(|e| -> Error {
                    IndexSnafu {
                        reason: format!("tags failed JSON serialization: {e}"),
                    }
                    .build()
                })?;
                insert_units
                    .execute(params![
                        unit.concept.argosy,
                        namespace,
                        concept_id,
                        unit.chunk_ordinal,
                        unit.text_hash,
                        unit.meta.concept_type,
                        unit.meta.description,
                        tags,
                        unit.meta.language,
                        unit.meta.category,
                    ])
                    .context(SqliteSnafu)?;
                let rowid = tx.last_insert_rowid();
                insert_vec
                    .execute(params![rowid, vec_to_bytes(&unit.vector)])
                    .context(SqliteSnafu)?;
            }
        }
        tx.commit().context(SqliteSnafu)?;
        if self.dimensions.is_none() {
            self.dimensions = Some(dims);
        }
        Ok(())
    }

    fn remove_concept(&mut self, concept: &QualifiedConceptId) -> Result<()> {
        let namespace = concept.namespace.as_dir_name();
        let concept_id = concept.id.as_str();
        let key: &[&dyn ToSql] = &[&concept.argosy, &namespace, &concept_id];
        // One transaction: a partially applied pair would leave units rows
        // whose vectors are gone — `unit_hashes` would still report such a
        // concept while search can never return it (silently unsearchable).
        let tx = self.conn.transaction().context(SqliteSnafu)?;
        tx.execute(
            "DELETE FROM unit_vectors WHERE rowid IN (
                     SELECT rowid FROM units
                     WHERE argosy = ?1 AND namespace = ?2 AND concept_id = ?3
                 )",
            key,
        )
        .context(SqliteSnafu)?;
        tx.execute(
            "DELETE FROM units
                 WHERE argosy = ?1 AND namespace = ?2 AND concept_id = ?3",
            key,
        )
        .context(SqliteSnafu)?;
        tx.commit().context(SqliteSnafu)?;
        Ok(())
    }

    fn unit_hashes(&self) -> Result<HashMap<QualifiedConceptId, String>> {
        // One concept may hold several chunk rows (same hash each); the map
        // keeps one entry per concept — dedup by construction.
        let mut stmt = self
            .conn
            .prepare(
                "SELECT argosy, namespace, concept_id, text_hash FROM units
                 GROUP BY argosy, namespace, concept_id",
            )
            .context(SqliteSnafu)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .context(SqliteSnafu)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context(SqliteSnafu)?;
        rows.into_iter()
            .map(|(argosy, namespace, concept_id, text_hash)| {
                Ok((row_key(argosy, namespace, concept_id)?, text_hash))
            })
            .collect()
    }

    fn clear(&mut self) -> Result<()> {
        // Drop the data tables but release BOTH the identity and the
        // dimensionality: the next upsert re-establishes them in-batch, so
        // a width-changing model upgrade rebuilds via the ordinary reconcile
        // path. One transaction — a partial clear would leave `meta`
        // claiming hashes while the data is gone, silently emptying search.
        let tx = self.conn.transaction().context(SqliteSnafu)?;
        tx.execute_batch("DROP TABLE IF EXISTS unit_vectors; DELETE FROM units;")
            .context(SqliteSnafu)?;
        tx.execute(
            "UPDATE meta SET model_id = NULL, dimensions = NULL, updated_at = datetime('now') WHERE id = 1",
            [],
        )
        .context(SqliteSnafu)?;
        tx.commit().context(SqliteSnafu)?;
        self.model_id = None;
        self.dimensions = None;
        Ok(())
    }

    fn search(&self, vector: &[f32], k: usize, filter: &Filter) -> Result<Vec<SearchHit>> {
        if k == 0 || self.dimensions.is_none() {
            return Ok(Vec::new());
        }
        let dims = self.dimensions.expect("dimensions checked above");
        if vector.len() != dims {
            return IndexSnafu {
                reason: format!(
                    "query vector of dimension {} does not match the store's dimension {dims}",
                    vector.len()
                ),
            }
            .fail();
        }
        self.ensure_vec_table_read()?;

        // Bound-parameter SQL: every user-influenced filter value travels as
        // a parameter, never string-interpolated. Only the number of
        // placeholders (from counts) shapes the text.
        let mut sql = String::from(
            "SELECT u.argosy, u.namespace, u.concept_id, u.concept_type,
                    u.description, u.tags, u.language, u.category, v.distance
             FROM unit_vectors AS v
             JOIN units AS u ON u.rowid = v.rowid
             WHERE v.vector MATCH ? AND k = ?",
        );
        let mut values: Vec<rusqlite::types::Value> =
            vec![vec_to_bytes(vector).into(), (k as i64).into()];

        let mut push_in_list = |column: &str, entries: &[String]| {
            if entries.is_empty() {
                // An empty allow-list matches NOTHING (the doc-06
                // `filter_matches` reference semantics) — never emit `IN ()`,
                // which is a SQLite syntax error.
                sql.push_str(" AND 1 = 0");
            } else {
                sql.push_str(&format!(
                    " AND {column} IN ({})",
                    entries.iter().map(|_| "?").collect::<Vec<_>>().join(", ")
                ));
                values.extend(entries.iter().cloned().map(Into::into));
            }
        };

        if let Some(namespaces) = &filter.namespaces {
            let names: Vec<String> = namespaces
                .iter()
                .map(|ns| ns.as_dir_name().to_string())
                .collect();
            push_in_list("u.namespace", &names);
        }
        if let Some(argosies) = &filter.argosies {
            push_in_list("u.argosy", argosies);
        }
        if let Some(concept_types) = &filter.concept_types {
            push_in_list("u.concept_type", concept_types);
        }
        if let Some(tags) = &filter.tags {
            if tags.is_empty() {
                sql.push_str(" AND 1 = 0");
            } else {
                sql.push_str(&format!(
                    " AND EXISTS (SELECT 1 FROM json_each(u.tags) je WHERE je.value IN ({}))",
                    tags.iter().map(|_| "?").collect::<Vec<_>>().join(", ")
                ));
                values.extend(tags.iter().cloned().map(Into::into));
            }
        }
        if let Some(language) = &filter.language {
            sql.push_str(" AND u.language = ?");
            values.push(language.clone().into());
        }
        if let Some(category) = &filter.category {
            sql.push_str(" AND u.category = ?");
            values.push(category.clone().into());
        }

        let mut stmt = self.conn.prepare(&sql).context(SqliteSnafu)?;
        let rows = stmt
            .query_map(params_from_iter(values.iter()), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    // Score = -distance: descending order is descending
                    // similarity (module docs).
                    -row.get::<_, f64>(8)? as f32,
                ))
            })
            .context(SqliteSnafu)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context(SqliteSnafu)?;

        let mut hits = rows
            .into_iter()
            .map(
                |(a, ns, cid, ty, desc, tags, lang, cat, score)| -> Result<SearchHit> {
                    let tags: Vec<String> = serde_json::from_str(&tags).map_err(|e| -> Error {
                        IndexSnafu {
                            reason: format!("corrupt index row: tags JSON failed to parse: {e}"),
                        }
                        .build()
                    })?;
                    Ok(SearchHit {
                        concept: row_key(a, ns, cid)?,
                        score,
                        meta: UnitMeta {
                            concept_type: ty,
                            description: desc,
                            tags,
                            language: lang,
                            category: cat,
                        },
                    })
                },
            )
            .collect::<Result<Vec<_>>>()?;
        // vec0's KNN constraint returns at most k rows; sort here so
        // descending-score order does not depend on iteration order.
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        Ok(hits)
    }
}

impl SqliteVecStore {
    /// Shared read-side vec-table check (kept separate from `ensure_vec_table`
    /// so `search` can stay `&self`).
    fn ensure_vec_table_read(&self) -> Result<()> {
        ensure_vec_table(
            &self.conn,
            self.dimensions.expect("dimensions checked by caller"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{MockEmbedder, fixture};
    use super::*;
    use crate::index::{EmbeddingProvider, Index, Query};

    use tempfile::TempDir;

    fn open_in_tmp() -> (TempDir, SqliteVecStore) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".argosy/index.db");
        let store = SqliteVecStore::open(&path).unwrap();
        (dir, store)
    }

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

    /// Embeds `text` and wraps it into a unit with the given identity/facets.
    fn make_unit(
        embedder: &MockEmbedder,
        argosy: &str,
        namespace: Namespace,
        id: &str,
        text: &str,
        meta: UnitMeta,
    ) -> EmbeddingUnit {
        EmbeddingUnit {
            concept: QualifiedConceptId {
                argosy: argosy.to_string(),
                namespace,
                id: id.parse().unwrap(),
            },
            chunk_ordinal: 0,
            text_hash: text.len().to_string(),
            vector: embedder.embed(&[text.to_string()]).unwrap()[0].clone(),
            meta,
        }
    }

    fn meta(
        concept_type: Option<&str>,
        tags: &[&str],
        language: Option<&str>,
        category: Option<&str>,
    ) -> UnitMeta {
        UnitMeta {
            concept_type: concept_type.map(str::to_string),
            description: None,
            tags: tags.iter().map(|s| s.to_string()).collect(),
            language: language.map(str::to_string),
            category: category.map(str::to_string),
        }
    }

    /// The standard corpus: three concepts across two argosies/namespaces
    /// with distinct facets, so every Filter dimension has a discriminant.
    fn seed(store: &mut SqliteVecStore, embedder: &MockEmbedder) -> Vec<EmbeddingUnit> {
        let units = vec![
            make_unit(
                embedder,
                "local",
                Namespace::Document,
                "document/arch",
                "water flows downhill through valleys",
                meta(Some("Note"), &["geo", "rust"], None, None),
            ),
            make_unit(
                embedder,
                "local",
                Namespace::Skill,
                "skill/deploy",
                "rust compile cargo build release",
                meta(Some("Skill"), &["rust"], None, None),
            ),
            make_unit(
                embedder,
                "vendor",
                Namespace::Styleguide,
                "styleguide/rust/naming",
                "naming conventions snake case identifiers",
                meta(
                    Some("Styleguide Rule"),
                    &["style"],
                    Some("rust"),
                    Some("naming"),
                ),
            ),
        ];
        store.upsert(&units).unwrap();
        units
    }

    #[test]
    fn open_creates_schema_and_sets_user_version() {
        let (_dir, store) = open_in_tmp();
        let user_version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(user_version, SCHEMA_VERSION as i64);
        let journal: String = store
            .conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal, "wal");
    }

    #[test]
    fn recorded_identity_and_dimensions_survive_reopen() {
        let (_dir, mut store) = open_in_tmp();
        assert_eq!(store.model_id(), None);
        let embedder = MockEmbedder::new();
        let unit = make_unit(
            &embedder,
            "local",
            Namespace::Document,
            "document/a",
            "persist me",
            UnitMeta::default(),
        );
        store.upsert(&[unit]).unwrap();
        store.set_model_id("mock-embedder@1").unwrap();
        let path_holder = _dir.path().join(".argosy/index.db");
        drop(store);

        let store = SqliteVecStore::open(&path_holder).unwrap();
        assert_eq!(store.model_id(), Some("mock-embedder@1"));
        assert_eq!(store.dimensions, Some(128));
        assert_eq!(store.unit_hashes().unwrap().len(), 1);
    }

    #[test]
    fn upsert_then_search_returns_nearest_text_first() {
        let (_dir, mut store) = open_in_tmp();
        let embedder = MockEmbedder::new();
        let units = seed(&mut store, &embedder);

        let query = embedder
            .embed(&["water flows downhill".to_string()])
            .unwrap()
            .remove(0);
        let hits = store.search(&query, 3, &Filter::default()).unwrap();

        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].concept, units[0].concept, "nearest text is first");
        assert!(
            hits.windows(2).all(|w| w[0].score >= w[1].score),
            "scores are descending (IDX-7)"
        );
        assert!(hits[0].score > hits[2].score, "ranking discriminates");
    }

    #[test]
    fn upsert_replaces_a_concept_instead_of_duplicating_it() {
        let (_dir, mut store) = open_in_tmp();
        let embedder = MockEmbedder::new();
        seed(&mut store, &embedder);
        let edited = make_unit(
            &embedder,
            "local",
            Namespace::Document,
            "document/arch",
            "completely different text about birds",
            meta(Some("Note"), &["birds"], None, None),
        );
        store.upsert(std::slice::from_ref(&edited)).unwrap();

        let hashes = store.unit_hashes().unwrap();
        assert_eq!(hashes.len(), 3, "re-upsert replaces, does not duplicate");
        assert_eq!(hashes[&edited.concept], edited.text_hash);
    }

    #[test]
    fn search_filters_on_namespace_argosy_type_and_facets() {
        let (_dir, mut store) = open_in_tmp();
        let embedder = MockEmbedder::new();
        let units = seed(&mut store, &embedder);
        let query = embedder
            .embed(&["broad query water rust naming".to_string()])
            .unwrap()
            .remove(0);

        let expect_single = |filter: Filter, want: &EmbeddingUnit, label: &str| {
            let hits = store.search(&query, 10, &filter).unwrap();
            assert_eq!(hits.len(), 1, "{label} isolates one concept");
            assert_eq!(hits[0].concept, want.concept, "{label}");
        };

        expect_single(
            Filter {
                namespaces: Some(vec![Namespace::Document]),
                ..Filter::default()
            },
            &units[0],
            "namespace filter",
        );
        expect_single(
            Filter {
                namespaces: Some(vec![Namespace::Skill]),
                argosies: Some(vec!["local".to_string()]),
                ..Filter::default()
            },
            &units[1],
            "argosy ∧ namespace filter",
        );
        expect_single(
            Filter {
                concept_types: Some(vec!["Styleguide Rule".to_string()]),
                ..Filter::default()
            },
            &units[2],
            "concept_type filter",
        );
        expect_single(
            Filter {
                tags: Some(vec!["geo".to_string(), "style".to_string()]),
                language: Some("rust".to_string()),
                category: Some("naming".to_string()),
                ..Filter::default()
            },
            &units[2],
            "tags/language/category filter",
        );
    }

    #[test]
    fn remove_concept_drops_every_trace_of_it() {
        let (_dir, mut store) = open_in_tmp();
        let embedder = MockEmbedder::new();
        let units = seed(&mut store, &embedder);
        store.remove_concept(&units[0].concept).unwrap();

        assert_eq!(store.unit_hashes().unwrap().len(), 2);
        let query = embedder
            .embed(&["water flows downhill".to_string()])
            .unwrap()
            .remove(0);
        let hits = store.search(&query, 10, &Filter::default()).unwrap();
        assert!(
            hits.iter().all(|h| h.concept != units[0].concept),
            "a removed concept is no longer retrievable"
        );
    }

    #[test]
    fn clear_empties_and_stays_operational() {
        let (_dir, mut store) = open_in_tmp();
        let embedder = MockEmbedder::new();
        seed(&mut store, &embedder);
        store.set_model_id("mock-embedder@1").unwrap();

        store.clear().unwrap();

        assert_eq!(
            store.model_id(),
            None,
            "clear drops identity (rebuild re-stamps)"
        );
        assert!(store.unit_hashes().unwrap().is_empty());
        assert_eq!(
            store.dimensions, None,
            "clear releases the dimensionality so a differently-sized model can rebuild"
        );
        let query = embedder
            .embed(&["water flows downhill".to_string()])
            .unwrap()
            .remove(0);
        assert!(
            store
                .search(&query, 10, &Filter::default())
                .unwrap()
                .is_empty()
        );

        // And the store is immediately usable again (the rebuild path):
        // the re-seed re-establishes identity-less dimensionality in the same
        // transaction as its inserts.
        seed(&mut store, &embedder);
        assert_eq!(store.unit_hashes().unwrap().len(), 3);
        assert_eq!(store.dimensions, Some(128));
        // The re-established vec table is queryable immediately.
        let hits = store.search(&query, 10, &Filter::default()).unwrap();
        assert_eq!(hits.len(), 3);
    }

    /// Regression (review P1): a batch holding several chunks of ONE concept
    /// must keep all of them — the delete is per concept, not per unit.
    #[test]
    fn upsert_keeps_every_chunk_of_a_multi_chunk_concept() {
        let (_dir, mut store) = open_in_tmp();
        let embedder = MockEmbedder::new();
        let mut chunk0 = make_unit(
            &embedder,
            "local",
            Namespace::Document,
            "document/arch",
            "water flows downhill through valleys",
            meta(Some("Note"), &["geo"], None, None),
        );
        let mut chunk1 = chunk0.clone();
        chunk0.chunk_ordinal = 0;
        chunk1.chunk_ordinal = 1;
        chunk1.vector = embedder
            .embed(&["distant mountains rivers oceans".to_string()])
            .unwrap()
            .remove(0);
        store.upsert(&[chunk0.clone(), chunk1.clone()]).unwrap();

        let units_rows: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM units", [], |row| row.get(0))
            .unwrap();
        let vec_rows: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM unit_vectors", [], |row| row.get(0))
            .unwrap();
        assert_eq!(units_rows, 2, "both chunks kept");
        assert_eq!(vec_rows, 2, "every chunk has exactly one vector");
        assert_eq!(store.unit_hashes().unwrap().len(), 1, "one concept");

        // And re-upserting the pair remains stable (no growth, no loss).
        store.upsert(&[chunk0, chunk1]).unwrap();
        let units_rows: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM units", [], |row| row.get(0))
            .unwrap();
        assert_eq!(units_rows, 2, "re-upsert replaces both chunks in place");
    }

    /// Regression (review P1): a provider whose model has a DIFFERENT vector
    /// width must be able to rebuild through mismatch → clear → upsert, not
    /// wedge on the store's old dimensionality forever.
    #[test]
    fn model_upgrade_with_a_different_dimension_rebuilds_cleanly() {
        let (_dir, mut store) = open_in_tmp();
        let embedder = MockEmbedder::new();
        seed(&mut store, &embedder); // 128-dim
        store.set_model_id("mock-embedder@1").unwrap();

        // The engine's mismatch path.
        store.clear().unwrap();
        let mut moved = make_unit(
            &embedder,
            "local",
            Namespace::Document,
            "document/arch",
            "water flows downhill through valleys",
            meta(Some("Note"), &["geo"], None, None),
        );
        moved.vector = vec![0.25f32; 64]; // a new model width
        store.upsert(&[moved]).unwrap();
        store.set_model_id("mock-embedder@2").unwrap();

        assert_eq!(store.dimensions, Some(64), "the new width is adopted");
        let hits = store
            .search(&[0.25f32; 64], 10, &Filter::default())
            .unwrap();
        assert_eq!(
            hits.len(),
            1,
            "rebuilt store is searchable at the new width"
        );
    }

    /// Regression (review P2): empty allow-list filters return no hits, never
    /// a SQLite syntax error — parity with the doc-06 reference semantics.
    #[test]
    fn empty_filter_lists_return_no_hits_not_an_error() {
        let (_dir, mut store) = open_in_tmp();
        let embedder = MockEmbedder::new();
        seed(&mut store, &embedder);
        let query = embedder
            .embed(&["water flows downhill".to_string()])
            .unwrap()
            .remove(0);

        for filter in [
            Filter {
                namespaces: Some(vec![]),
                ..Filter::default()
            },
            Filter {
                argosies: Some(vec![]),
                ..Filter::default()
            },
            Filter {
                concept_types: Some(vec![]),
                ..Filter::default()
            },
            Filter {
                tags: Some(vec![]),
                ..Filter::default()
            },
        ] {
            assert!(
                store.search(&query, 10, &filter).unwrap().is_empty(),
                "an empty allow-list matches nothing, without error"
            );
        }
    }

    /// Regression (review P3): a db written by a NEWER argosy must fail
    /// loudly instead of being misread as v1 (and its version must not be
    /// clobbered).
    #[test]
    fn opening_a_newer_schema_version_is_a_loud_error() {
        let (dir, store) = open_in_tmp();
        store
            .conn
            .pragma_update(None, "user_version", SCHEMA_VERSION as i64 + 1)
            .unwrap();
        let path = dir.path().join(".argosy/index.db");
        drop(store);

        let err = SqliteVecStore::open(&path).err().expect("open should fail");
        assert!(
            err.to_string().contains("newer"),
            "names the schema-version mismatch: {err}"
        );
        let version_held: i64 = rusqlite::Connection::open(&path)
            .unwrap()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            version_held,
            SCHEMA_VERSION as i64 + 1,
            "the newer version is never clobbered"
        );
    }

    #[test]
    fn wrong_dimension_upsert_is_an_error_not_a_truncation() {
        let (_dir, mut store) = open_in_tmp();
        let embedder = MockEmbedder::new();
        let units = seed(&mut store, &embedder);
        let mut bad = units[0].clone();
        bad.vector = vec![0.5f32; 64];

        let err = store.upsert(&[bad]).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("64") && msg.contains("128"),
            "error names both dimensions: {msg}"
        );
        assert_eq!(
            store.unit_hashes().unwrap().len(),
            3,
            "a rejected upsert stores nothing"
        );

        let err = store
            .search(&[0.0f32; 64], 5, &Filter::default())
            .unwrap_err();
        assert!(err.to_string().contains("dimension"));
    }

    #[test]
    fn unit_hashes_round_trip_tracks_every_mutation() {
        let (_dir, mut store) = open_in_tmp();
        let embedder = MockEmbedder::new();
        let units = seed(&mut store, &embedder);
        let hashes = store.unit_hashes().unwrap();
        assert_eq!(hashes.len(), 3);
        for unit in &units {
            assert_eq!(hashes[&unit.concept], unit.text_hash);
        }
        store.remove_concept(&units[1].concept).unwrap();
        assert_eq!(store.unit_hashes().unwrap().len(), 2);
    }

    /// Real-database coverage: engine + store on a temp ProjectContext,
    /// including the model-mismatch rebuild paths.
    #[test]
    fn reconcile_end_to_end_with_sqlite_store() {
        let (local, _imported, ctx) = fixture();
        let db_path = local.path().join(".argosy/index.db");
        let mut index = Index::new(MockEmbedder::new(), SqliteVecStore::open(&db_path).unwrap());

        let report = index.reconcile(&ctx).unwrap();
        assert_eq!(report.upserted, 5);
        assert_eq!(report.model_id, "mock-embedder@1");

        // Search over real SQL: the architecture doc wins its own query.
        let hits = index
            .search(&ctx, &Query::unscoped("architecture", 10))
            .unwrap();
        assert!(
            hits.iter()
                .any(|h| h.concept.id.as_str() == "document/arch" && h.concept.argosy == "local")
        );
        assert_eq!(hits[0].concept.id.as_str(), "document/arch");

        // Edit one concept → exactly it is re-staged, and its old text is gone.
        let edited = crate::concept::Concept::from_str(
            "---\ntype: Note\ndescription: Build gotchas.\ntags: [build]\n---\nMoldova espresso deadlines.\n",
        )
        .unwrap();
        ctx.local()
            .write_concept(
                Namespace::Memory,
                &"memory/gotchas".parse().unwrap(),
                &edited,
            )
            .unwrap();
        let report = index.reconcile(&ctx).unwrap();
        assert_eq!(report.upserted, 1);
        assert_eq!(report.unchanged, 4);

        let hits = index
            .search(&ctx, &Query::unscoped("moldova espresso", 10))
            .unwrap();
        assert_eq!(hits[0].concept.id.as_str(), "memory/gotchas");
        let hits = index
            .search(&ctx, &Query::unscoped("cargo needs lockfile", 10))
            .unwrap();
        assert_ne!(
            hits[0].concept.id.as_str(),
            "memory/gotchas",
            "the replaced text no longer tops its own old query"
        );

        // A provider with a different identity rebuilds
        // everything with zero errors and zero mixed vectors.
        index.set_provider(MockEmbedder::with_model_id(
            "fastembed/sentence-transformers/all-MiniLM-L6-v2@fastembed-5",
        ));
        let report = index.reconcile(&ctx).unwrap();
        assert!(report.rebuilt);
        assert_eq!(report.upserted, 5);
        assert_eq!(
            index.store().model_id(),
            Some("fastembed/sentence-transformers/all-MiniLM-L6-v2@fastembed-5")
        );
    }

    /// A second open over the same db reuses it with zero re-embeds.
    #[test]
    fn reopening_the_store_reuses_the_index_with_zero_reembeds() {
        let (_local, _imported, ctx) = fixture();
        let db_dir = TempDir::new().unwrap();
        let db_path = db_dir.path().join(".argosy/index.db");
        let mut index = Index::new(MockEmbedder::new(), SqliteVecStore::open(&db_path).unwrap());
        index.reconcile(&ctx).unwrap();
        drop(index);

        let mut index = Index::new(MockEmbedder::new(), SqliteVecStore::open(&db_path).unwrap());
        assert_eq!(
            index.store().model_id(),
            Some("mock-embedder@1"),
            "the recorded identity survives the reopen"
        );
        let report = index.reconcile(&ctx).unwrap();
        assert!(!report.rebuilt);
        assert_eq!(report.upserted, 0);
        assert_eq!(report.unchanged, 5);
        assert_eq!(
            index.provider().embed_calls(),
            0,
            "NFR-4: nothing re-embedded"
        );
    }
}
