//! The `VectorStore` implementation: upserts, deletes, and filtered
//! k-NN search over sqlite-vec.

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
                .context(SqliteSnafu)?
        } else {
            k as i64
        };

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
            vec![vec_to_bytes(vector).into(), sql_k.into()];

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
        // vec0's KNN constraint returns at most sql_k rows; sort here so
        // descending-score order does not depend on iteration order, then
        // apply the caller's k to the fully filtered ranking.
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        hits.truncate(k);
        Ok(hits)
    }
}
