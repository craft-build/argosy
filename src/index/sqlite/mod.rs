//! The `sqlite-vec`-backed default [`VectorStore`], gated behind the
//! `default-index` Cargo feature: one `.argosy/index.db` file spanning all
//! active argosys (`meta` model row, `units` facet rows, `unit_vectors`
//! vec0 keyed by units rowid). Scores are `-distance`. Filtered queries
//! rank the full corpus (vec0 applies filters after its top-k, so
//! `search` raises the SQL-side k to the row count and truncates after
//! filtering — exact, at no asymptotic cost). v1 limit: single-process
//! writers only.

use std::ffi::{c_char, c_int};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Once;

use rusqlite::{Connection, OptionalExtension};
use snafu::ResultExt;

use crate::bundle::Namespace;
use crate::concept::ConceptId;
use crate::context::QualifiedConceptId;
use crate::error::{Error, IndexSnafu, IoSnafu, Result, SqliteSnafu};

use super::Filter;

mod store;

#[cfg(test)]
mod tests;

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
pub(super) fn vec_to_bytes(vector: &[f32]) -> Vec<u8> {
    vector.iter().flat_map(|v| v.to_ne_bytes()).collect()
}

/// A [`VectorStore`] on a local `sqlite-vec` database file — the default
/// backend of the binary and MCP server behind `default-index`.
///
/// See the module docs for the schema, scoring convention, and limitations.
pub struct SqliteVecStore {
    path: PathBuf,
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
        let conn = Connection::open(path).context(SqliteSnafu { path: path.to_path_buf() })?;
        // WAL: crash-safe single-writer operation (module docs: no
        // multi-process guarantees).
        conn.pragma_update(None, "journal_mode", "WAL")
            .context(SqliteSnafu { path: path.to_path_buf() })?;
        // Wait briefly instead of failing instantly when another process
        // (e.g. `argosy index build` while `argosy mcp` serves) holds the
        // write lock: "database is locked" after 0 ms is cryptic, after
        // 5 s it means something is genuinely stuck.
        conn.pragma_update(None, "busy_timeout", 5_000)
            .context(SqliteSnafu { path: path.to_path_buf() })?;
        // Stamp the schema version on fresh databases only; refuse dbs from a
        // newer argosy rather than silently misreading their layout (and
        // never clobber a higher version, so a future migration can notice).
        let existing_version = Self::check_schema_version(&conn, path)?;
        if existing_version == 0 {
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)
                .context(SqliteSnafu { path: path.to_path_buf() })?;
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
        .context(SqliteSnafu { path: path.to_path_buf() })?;

        let (model_id, dimensions) = Self::read_meta(&conn, path)?;
        if let Some(dims) = dimensions {
            ensure_vec_table(&conn, path, dims)?;
        }
        Ok(Self {
            path: path.to_path_buf(),
            conn,
            model_id,
            dimensions,
        })
    }

    /// Opens the store strictly read-only (the CLI's `index status`): no
    /// parent-directory creation, no WAL pragma, no schema DDL — the
    /// database must already exist and remain untouched. This lets
    /// `status` answer on a read-only or permission-locked index where
    /// [`Self::open`] would fail before reading a single row.
    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self> {
        register_extension();
        let path = path.as_ref();
        let conn =
            Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .context(SqliteSnafu { path: path.to_path_buf() })?;
        // Readers hit locks too (WAL checkpoints); the same wait applies.
        conn.pragma_update(None, "busy_timeout", 5_000)
            .context(SqliteSnafu { path: path.to_path_buf() })?;
        Self::check_schema_version(&conn, path)?;
        let (model_id, dimensions) = Self::read_meta(&conn, path)?;
        Ok(Self {
            path: path.to_path_buf(),
            conn,
            model_id,
            dimensions,
        })
    }

    /// Refuses databases stamped with a newer schema than this build; returns
    /// the on-disk `user_version` otherwise (read-only).
    fn check_schema_version(conn: &Connection, db: &Path) -> Result<i64> {
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .context(SqliteSnafu { path: db.to_path_buf() })?;
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
    fn read_meta(conn: &Connection, db: &Path) -> Result<(Option<String>, Option<usize>)> {
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
        .context(SqliteSnafu { path: db.to_path_buf() })
        .map(|row| row.unwrap_or_default())
    }
}

/// Creates the `unit_vectors` vec0 table if absent. `dims` is an internal
/// usize, never user input, so interpolation is safe here.
pub(super) fn ensure_vec_table(conn: &Connection, db: &Path, dims: usize) -> Result<()> {
    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'unit_vectors'",
            [],
            |row| row.get(0),
        )
        .context(SqliteSnafu { path: db.to_path_buf() })?;
    if exists == 0 {
        conn.execute_batch(&format!(
            "CREATE VIRTUAL TABLE unit_vectors USING vec0(vector float[{dims}])"
        ))
        .context(SqliteSnafu { path: db.to_path_buf() })?;
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
    let conn = Connection::open(path).context(SqliteSnafu { path: path.to_path_buf() })?;
    // `(busy, wal frames, checkpointed frames)`; `-1` log/ckpt = not a WAL
    // database (nothing to move, the copy is complete by definition).
    let (busy, log, checkpointed): (i64, i64, i64) = conn
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .context(SqliteSnafu { path: path.to_path_buf() })?;
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

/// True iff any filter field constrains the result set — the signal that
/// `search` must rank the full corpus rather than trust vec0's top-k.
fn filter_is_active(filter: &Filter) -> bool {
    filter.namespaces.is_some()
        || filter.argosies.is_some()
        || filter.concept_types.is_some()
        || filter.tags.is_some()
        || filter.language.is_some()
        || filter.category.is_some()
}

impl SqliteVecStore {
    /// Shared read-side vec-table check (kept separate from `ensure_vec_table`
    /// so `search` can stay `&self`).
    fn ensure_vec_table_read(&self) -> Result<()> {
        let Some(dimensions) = self.dimensions else {
            return IndexSnafu {
                reason: "index dimensionality is unknown; write once before searching"
                    .to_string(),
            }
            .fail();
        };
        ensure_vec_table(&self.conn, &self.path, dimensions)
    }
}
