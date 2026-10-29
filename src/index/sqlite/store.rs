//! The `VectorStore` implementation: upserts, deletes, filtered k-NN
//! search over sqlite-vec (the vector leg), and BM25 search over an FTS5
//! index (the lexical leg).

use std::collections::{HashMap, HashSet};

use rusqlite::types::ToSql;
use rusqlite::{params, params_from_iter};
use snafu::ResultExt;

use crate::context::QualifiedConceptId;
use crate::error::{Error, IndexSnafu, Result, SqliteSnafu};
use crate::index::{EmbeddingUnit, Filter, SearchHit, UnitMeta, VectorStore};

use super::{SqliteVecStore, ensure_vec_table, filter_is_active, row_key, vec_to_bytes};

impl VectorStore for SqliteVecStore {
    fn model_id(&self) -> Option<&str> {
        self.model_id.as_deref()
    }

    fn recorded_dimensions(&self) -> Option<usize> {
        self.dimensions
    }

    fn set_model_id(&mut self, id: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE meta SET model_id = ?1, updated_at = datetime('now') WHERE id = 1",
                params![id],
            )
            .context(SqliteSnafu {
                path: self.path.clone(),
            })?;
        self.model_id = Some(id.to_string());
        Ok(())
    }

    fn upsert(&mut self, units: &[EmbeddingUnit]) -> Result<()> {
        if units.is_empty() {
            return Ok(());
        }
        // Dimension safety: the vec0 table's width is fixed at creation
        // (first write, or re-established by `clear`); anything of a
        // different length is an error, never a silent truncation. A
        // zero-dimension store is the lexical-only index
        // (`LexicalOnlyProvider`): units are stored for BM25 and lookup
        // with no vec0 table at all.
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

        let tx = self.conn.transaction().context(SqliteSnafu {
            path: self.path.clone(),
        })?;
        // Stamp the store's dimensionality inside the batch transaction, so a
        // rolled-back insert never leaves `meta.dimensions` against an empty
        // store.
        if self.dimensions.is_none() {
            tx.execute(
                "UPDATE meta SET dimensions = ?1, updated_at = datetime('now') WHERE id = 1",
                params![dims as i64],
            )
            .context(SqliteSnafu {
                path: self.path.clone(),
            })?;
            if dims > 0 {
                ensure_vec_table(&tx, &self.path, dims)?;
            }
        }
        {
            // A zero-dimension (lexical-only) store has no vec0 table: the
            // two vector statements are not even prepared there, since
            // preparing against a nonexistent table is itself an error.
            let mut delete_vec = (dims > 0)
                .then(|| {
                    tx.prepare_cached(
                        "DELETE FROM unit_vectors WHERE rowid IN (
                     SELECT rowid FROM units
                     WHERE argosy = ?1 AND namespace = ?2 AND concept_id = ?3
                 )",
                    )
                    .context(SqliteSnafu {
                        path: self.path.clone(),
                    })
                })
                .transpose()?;
            let mut delete_units = tx
                .prepare_cached(
                    "DELETE FROM units
                     WHERE argosy = ?1 AND namespace = ?2 AND concept_id = ?3",
                )
                .context(SqliteSnafu {
                    path: self.path.clone(),
                })?;
            let mut insert_units = tx
                .prepare_cached(
                    "INSERT INTO units (argosy, namespace, concept_id, chunk_ordinal,
                                        text_hash, concept_type, description, tags,
                                        language, category, heading_path, start_line,
                                        end_line, passage)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                )
                .context(SqliteSnafu {
                    path: self.path.clone(),
                })?;
            let mut insert_vec = (dims > 0)
                .then(|| {
                    tx.prepare_cached("INSERT INTO unit_vectors (rowid, vector) VALUES (?1, ?2)")
                        .context(SqliteSnafu {
                            path: self.path.clone(),
                        })
                })
                .transpose()?;
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
                    if let Some(delete_vec) = delete_vec.as_mut() {
                        delete_vec.execute(key).context(SqliteSnafu {
                            path: self.path.clone(),
                        })?;
                    }
                    delete_units.execute(key).context(SqliteSnafu {
                        path: self.path.clone(),
                    })?;
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
                        unit.heading_path,
                        unit.start_line,
                        unit.end_line,
                        unit.passage,
                    ])
                    .context(SqliteSnafu {
                        path: self.path.clone(),
                    })?;
                let rowid = tx.last_insert_rowid();
                if let Some(insert_vec) = insert_vec.as_mut() {
                    insert_vec
                        .execute(params![rowid, vec_to_bytes(&unit.vector)])
                        .context(SqliteSnafu {
                            path: self.path.clone(),
                        })?;
                }
            }
        }
        tx.commit().context(SqliteSnafu {
            path: self.path.clone(),
        })?;
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
        // A zero-dimension (lexical-only) store has no vec0 table; only
        // the units row (and its FTS shadow) needs deleting.
        let dims = self.dimensions.unwrap_or_default();
        let tx = self.conn.transaction().context(SqliteSnafu {
            path: self.path.clone(),
        })?;
        if dims > 0 {
            tx.execute(
                "DELETE FROM unit_vectors WHERE rowid IN (
                     SELECT rowid FROM units
                     WHERE argosy = ?1 AND namespace = ?2 AND concept_id = ?3
                 )",
                key,
            )
            .context(SqliteSnafu {
                path: self.path.clone(),
            })?;
        }
        tx.execute(
            "DELETE FROM units
                 WHERE argosy = ?1 AND namespace = ?2 AND concept_id = ?3",
            key,
        )
        .context(SqliteSnafu {
            path: self.path.clone(),
        })?;
        tx.commit().context(SqliteSnafu {
            path: self.path.clone(),
        })?;
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
            .context(SqliteSnafu {
                path: self.path.clone(),
            })?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .context(SqliteSnafu {
                path: self.path.clone(),
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context(SqliteSnafu {
                path: self.path.clone(),
            })?;
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
        let tx = self.conn.transaction().context(SqliteSnafu {
            path: self.path.clone(),
        })?;
        tx.execute_batch("DROP TABLE IF EXISTS unit_vectors; DELETE FROM units;")
            .context(SqliteSnafu {
                path: self.path.clone(),
            })?;
        tx.execute(
            "UPDATE meta SET model_id = NULL, dimensions = NULL, updated_at = datetime('now') WHERE id = 1",
            [],
        )
        .context(SqliteSnafu { path: self.path.clone() })?;
        tx.commit().context(SqliteSnafu {
            path: self.path.clone(),
        })?;
        self.model_id = None;
        self.dimensions = None;
        Ok(())
    }

    fn search(&self, vector: &[f32], k: usize, filter: &Filter) -> Result<Vec<SearchHit>> {
        if k == 0 || self.dimensions.is_none() {
            return Ok(Vec::new());
        }
        let dims = self.dimensions.expect("dimensions checked above");
        // A lexical-only store (zero dimensions, `LexicalOnlyProvider`)
        // has no vec0 table and no vector leg; the lexical leg owns the
        // query.
        if dims == 0 {
            return Ok(Vec::new());
        }
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

        // Filtered queries rank the WHOLE corpus first: vec0 applies
        // metadata filters after its top-k truncation, so a small k would
        // drop every matching concept whenever the k nearest are
        // non-matching (empty `search_rules --language python` on a mixed
        // corpus). Fetching k = row count makes filtered search exact —
        // vec0 brute-forces all rows for any k, so this costs nothing
        // asymptotically — and the caller's k is applied by truncating
        // the fully ranked, fully filtered list below.
        let sql_k: i64 = if filter_is_active(filter) {
            self.conn
                .query_row("SELECT COUNT(*) FROM units", [], |row| row.get(0))
                .context(SqliteSnafu {
                    path: self.path.clone(),
                })?
        } else {
            k as i64
        };

        // Bound-parameter SQL: every user-influenced filter value travels as
        // a parameter, never string-interpolated. Only the number of
        // placeholders (from counts) shapes the text. Score = -distance:
        // descending order is descending similarity (module docs).
        let mut sql = format!(
            "SELECT {UNIT_COLUMNS}, v.distance AS leg_score
             FROM unit_vectors AS v
             JOIN units AS u ON u.rowid = v.rowid
             WHERE v.vector MATCH ? AND k = ?"
        );
        let mut values: Vec<rusqlite::types::Value> =
            vec![vec_to_bytes(vector).into(), sql_k.into()];
        push_filter_sql(&mut sql, &mut values, filter);

        let mut hits = self
            .query_hits(&sql, &values)?
            .into_iter()
            // vec0's KNN constraint returns at most sql_k rows; sort here
            // so descending-score order does not depend on iteration
            // order, then apply the caller's k to the fully filtered
            // ranking.
            .collect::<Result<Vec<_>>>()?;
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        hits.truncate(k);
        Ok(hits)
    }

    fn lexical_search(&self, text: &str, k: usize, filter: &Filter) -> Result<Vec<SearchHit>> {
        if k == 0 {
            return Ok(Vec::new());
        }
        // A query with no indexable token (empty, punctuation-only) cannot
        // rank lexically: the expression stays unset and the vector leg
        // owns the query alone.
        let Some(match_expr) = fts_match_expr(text) else {
            return Ok(Vec::new());
        };
        // Score = -bm25: lower BM25 ranks better, so negating keeps the
        // both-legs convention "higher score is a better hit" true. The
        // active filters constrain the FTS5 ranking itself (WHERE before
        // ORDER BY/LIMIT), so the lexical leg honors the same
        // filter-then-truncate contract as the vector leg.
        let mut sql = format!(
            "SELECT {UNIT_COLUMNS}, bm25(unit_fts) AS leg_score
             FROM unit_fts
             JOIN units AS u ON u.rowid = unit_fts.rowid
             WHERE unit_fts MATCH ?"
        );
        let mut values: Vec<rusqlite::types::Value> = vec![match_expr.into()];
        push_filter_sql(&mut sql, &mut values, filter);
        sql.push_str(" ORDER BY leg_score LIMIT ?");
        values.push((k as i64).into());

        let hits = self
            .query_hits(&sql, &values)?
            .into_iter()
            .collect::<Result<Vec<_>>>()?;
        Ok(hits)
    }
}

impl SqliteVecStore {
    /// Runs one retrieval leg's query and decodes every row; the score
    /// column is expected as `leg_score` at index 13 (see
    /// [`UNIT_COLUMNS`]). Hits arrive roughly ranked; each leg sorts or
    /// truncates as its engine requires.
    fn query_hits(
        &self,
        sql: &str,
        values: &[rusqlite::types::Value],
    ) -> Result<Vec<Result<SearchHit>>> {
        let mut stmt = self.conn.prepare(sql).context(SqliteSnafu {
            path: self.path.clone(),
        })?;
        let rows = stmt
            .query_map(params_from_iter(values.iter()), decode_unit_row)
            .context(SqliteSnafu {
                path: self.path.clone(),
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context(SqliteSnafu {
                path: self.path.clone(),
            })?;
        Ok(rows.into_iter().map(RawHit::into_hit).collect())
    }
}

/// The `units` columns both retrieval legs SELECT, in the fixed order
/// [`decode_unit_row`] reads them. Each leg appends one scalar score
/// column aliased `leg_score` at index 13 — `-distance` for the vector
/// leg, `-bm25` for the lexical leg.
const UNIT_COLUMNS: &str = "u.argosy, u.namespace, u.concept_id, u.concept_type,
       u.description, u.tags, u.language, u.category, u.chunk_ordinal,
       u.heading_path, u.start_line, u.end_line, u.passage";

/// One `units` row plus its leg score, mid-decode (between rusqlite's
/// strict `rusqlite::Result` and argosy's fallible key/JSON decoding).
struct RawHit {
    argosy: String,
    namespace: String,
    concept_id: String,
    concept_type: Option<String>,
    description: Option<String>,
    tags: String,
    language: Option<String>,
    category: Option<String>,
    chunk_ordinal: i64,
    heading_path: String,
    start_line: i64,
    end_line: i64,
    passage: String,
    leg_score: f64,
}

/// Decodes the 14 fixed columns both legs emit (see [`UNIT_COLUMNS`]).
fn decode_unit_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawHit> {
    Ok(RawHit {
        argosy: row.get(0)?,
        namespace: row.get(1)?,
        concept_id: row.get(2)?,
        concept_type: row.get(3)?,
        description: row.get(4)?,
        tags: row.get(5)?,
        language: row.get(6)?,
        category: row.get(7)?,
        chunk_ordinal: row.get(8)?,
        heading_path: row.get(9)?,
        start_line: row.get(10)?,
        end_line: row.get(11)?,
        passage: row.get(12)?,
        leg_score: row.get(13)?,
    })
}

impl RawHit {
    /// Converts a decoded row into a [`SearchHit`], scoring it as the
    /// leg already arranged: `leg_score` is negated here so both engines'
    /// "lower raw value is better" metrics (distance, bm25) share the
    /// store's higher-is-better convention.
    fn into_hit(self) -> Result<SearchHit> {
        let tags: Vec<String> = serde_json::from_str(&self.tags).map_err(|e| -> Error {
            IndexSnafu {
                reason: format!("corrupt index row: tags JSON failed to parse: {e}"),
            }
            .build()
        })?;
        Ok(SearchHit {
            concept: row_key(self.argosy, self.namespace, self.concept_id)?,
            score: -self.leg_score as f32,
            meta: UnitMeta {
                concept_type: self.concept_type,
                description: self.description,
                tags,
                language: self.language,
                category: self.category,
            },
            chunk_ordinal: self.chunk_ordinal.max(0) as u32,
            heading_path: self.heading_path,
            start_line: self.start_line.max(0) as u32,
            end_line: self.end_line.max(0) as u32,
            passage: self.passage,
        })
    }
}

/// Appends bound-parameter SQL for every active filter field, pushing the
/// values; shared by both retrieval legs so a filter constrains exactly
/// the same rows in each. Only the number of placeholders (from counts)
/// shapes the text — every user-influenced value travels as a parameter,
/// never string-interpolated.
fn push_filter_sql(sql: &mut String, values: &mut Vec<rusqlite::types::Value>, filter: &Filter) {
    let push_in_list = |sql: &mut String,
                        values: &mut Vec<rusqlite::types::Value>,
                        column: &str,
                        entries: &[String]| {
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
        push_in_list(sql, values, "u.namespace", &names);
    }
    if let Some(argosies) = &filter.argosies {
        push_in_list(sql, values, "u.argosy", argosies);
    }
    if let Some(concept_types) = &filter.concept_types {
        push_in_list(sql, values, "u.concept_type", concept_types);
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
}

/// The most terms ever OR-ed into one FTS5 MATCH expression: past this a
/// pathological query costs more to plan than it can ever return.
const MAX_FTS_TERMS: usize = 32;

/// Builds an FTS5 MATCH expression from free text: every maximal
/// alphanumeric run becomes one quoted term, and the terms are OR-ed.
///
/// Quoting disables every MATCH operator (`AND`, `OR`, `NOT`, `NEAR`,
/// prefixes, column filters, leading `-`), so arbitrary — adversarial —
/// query text can never be parsed as FTS syntax; alphanumeric runs also
/// cannot contain the quote character, so no escaping is needed. OR
/// semantics maximize recall (a document matching any query term ranks),
/// with BM25 weighting toward rarer and more-matched terms — the lexical
/// leg of the reciprocal-rank fusion, where only the ordering matters.
///
/// Returns `None` when the text holds no indexable term (empty or
/// punctuation-only queries), leaving the vector leg to own the query.
fn fts_match_expr(text: &str) -> Option<String> {
    let mut terms = 0usize;
    let mut expr = String::new();
    for term in text.split(|c: char| !c.is_alphanumeric()) {
        if term.is_empty() {
            continue;
        }
        if terms >= MAX_FTS_TERMS {
            break;
        }
        if !expr.is_empty() {
            expr.push_str(" OR ");
        }
        expr.push('"');
        expr.push_str(term);
        expr.push('"');
        terms += 1;
    }
    if expr.is_empty() { None } else { Some(expr) }
}
