//! The sync, unit-testable handlers behind every tool: one
//! [`ProjectSession`] per opened project, cached by [`McpState`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::bundle::Namespace;
use crate::concept::ConceptId;
use crate::context::{ProjectContext, QualifiedConceptId};
use crate::error::{Error, Result};
use crate::index::{EmbeddingProvider, Filter, Index, Query, VectorStore};
use crate::local::PromotionTarget;

use super::params::*;
use super::reports::*;
use super::{ARGOSY_INDEX_SUFFIX, ARGOSYS_URI, DEFAULT_K, UNVERIFIED};

/// One opened project: its argosy set and its semantic index, reconciled
/// after every mutating tool — a written or deleted concept is visible to
/// `search`/`search_rules` in the same session.
pub struct ProjectSession<P: EmbeddingProvider, S: VectorStore> {
    /// The active argosys: one local (writable) plus imported (read-only).
    pub context: ProjectContext,
    /// The semantic index backing `search`/`search_rules`.
    pub index: Index<P, S>,
}

/// Opens the [`ProjectSession`] for a project root: context discovery,
/// index store, embedding provider, and first-open reconciliation.
/// Returning `Err` fails only that tool call — the failed root is never
/// cached, so a later `argosy init` is picked up by the next call.
pub type SessionFactory<P, S> = Arc<dyn Fn(&Path) -> Result<ProjectSession<P, S>> + Send + Sync>;

/// Everything the server serves: one cached [`ProjectSession`] per project
/// root, opened on first use through the [`SessionFactory`]. Mutations
/// reconcile their session's index in place; argosys pulled or initialized
/// after a session opened appear on the next open of that root.
pub struct McpState<P: EmbeddingProvider, S: VectorStore> {
    factory: SessionFactory<P, S>,
    sessions: HashMap<PathBuf, ProjectSession<P, S>>,
}

impl<P: EmbeddingProvider, S: VectorStore> ProjectSession<P, S> {
    /// A session over an already-opened context and index. The session
    /// factory normally runs [`Index::reconcile`] first, and every mutating
    /// tool re-reconciles before returning.
    pub fn new(context: ProjectContext, index: Index<P, S>) -> Self {
        Self { context, index }
    }

    /// Brings the index back in line with disk after a mutation. The write
    /// already succeeded, so a failure here is reported, never fatal:
    /// `Err` text goes to the caller's report as `index_error`.
    fn reindex(&mut self) -> std::result::Result<(), String> {
        self.index
            .reconcile(&self.context)
            .map(|_| ())
            .map_err(|err| {
                format!(
                    "{err:#}; the change is on disk but not yet indexed — \
                     retry after fixing (e.g. run `argosy index build`)"
                )
            })
    }

    fn hit_out(hit: crate::index::SearchHit) -> SearchHitOut {
        SearchHitOut {
            uri: hit.concept.to_uri(),
            argosy: hit.concept.argosy,
            namespace: hit.concept.namespace.as_dir_name().to_string(),
            concept_id: hit.concept.id.to_string(),
            score: hit.score,
            concept_type: hit.meta.concept_type,
            description: hit.meta.description,
            tags: hit.meta.tags,
            language: hit.meta.language,
            category: hit.meta.category,
            good: None,
            bad: None,
        }
    }

    /// Semantic search across every active argosy and indexed namespace.
    /// `argosy` is validated against the active set and `namespaces` against
    /// the indexed names: an unknown spelling of either errors rather than
    /// silently returning nothing.
    pub fn search(&self, params: SearchParams) -> Result<SearchReport> {
        if let Some(names) = &params.namespaces {
            for name in names {
                // Custom namespaces are not indexed, so anything outside the
                // reserved set can never match — a typo must error, not
                // return empty.
                if !Namespace::RESERVED.contains(&name.as_str()) {
                    return Err(Error::Validation {
                        reason: format!(
                            "unknown namespace `{name}`; the indexed namespaces are \
                             `{}` (custom namespaces are not indexed)",
                            Namespace::RESERVED.join("`, `")
                        ),
                    });
                }
            }
        }
        let mut filter = Filter {
            namespaces: params
                .namespaces
                .map(|ns| ns.iter().map(|n| Namespace::from_dir_name(n)).collect()),
            argosies: params.argosy.map(|a| vec![a]),
            concept_types: params.r#type.map(|t| vec![t]),
            tags: params.tags,
            language: params.language,
            category: params.category,
        };
        if let Some(list) = &mut filter.namespaces {
            list.dedup();
        }
        let query = Query {
            text: params.query,
            k: params.k.unwrap_or(DEFAULT_K),
            filter,
        };
        let hits = self.index.search(&self.context, &query)?;
        Ok(SearchReport {
            hits: hits.into_iter().map(Self::hit_out).collect(),
        })
    }

    /// The review-flow query: semantic search restricted to styleguide
    /// rules, optionally narrowed by `language`/`category` facets. Each hit
    /// carries the rule body's `## Good` / `## Bad` sections when present.
    pub fn search_rules(&self, params: RulesParams) -> Result<SearchReport> {
        let mut report = self.search(SearchParams {
            cwd: params.cwd,
            query: params.query,
            k: params.k,
            namespaces: Some(vec!["styleguide".to_string()]),
            argosy: None,
            tags: None,
            r#type: None,
            language: params.language,
            category: params.category,
        })?;
        for hit in &mut report.hits {
            // Tolerant like rule listing: a hit whose concept can no longer
            // be read (deleted mid-flight) keeps its index facets without
            // examples rather than failing the whole search.
            let Ok(concept) = self.context.read_uri(&hit.uri) else {
                continue;
            };
            let Ok(id) = hit.concept_id.parse() else {
                continue;
            };
            let rule = crate::styleguide::StyleguideRule::from_parts(id, concept);
            hit.good = rule.good_examples().map(str::to_string);
            hit.bad = rule.bad_examples().map(str::to_string);
        }
        Ok(report)
    }

    /// Every skill across all active argosys, shadowed ones annotated, each
    /// with origin and trust tier. (`params` carries only `cwd`, resolved
    /// by [`McpState`] before dispatch.)
    pub fn list_skills(&self, _params: ListSkillsParams) -> Result<SkillsReport> {
        let skills = self
            .context
            .list_skills()?
            .into_iter()
            .map(|listing| {
                let qid = QualifiedConceptId {
                    argosy: listing.argosy.clone(),
                    namespace: Namespace::Skill,
                    id: listing.skill.entry_point.clone(),
                };
                let verified = self
                    .context
                    .resolve(&qid)
                    .map(|c| verified_tier(&c))
                    .unwrap_or_else(|_| UNVERIFIED.to_string());
                SkillOut {
                    name: listing.skill.name,
                    argosy: listing.argosy,
                    uri: qid.to_uri(),
                    description: listing.skill.description,
                    shadowed: listing.shadowed,
                    verified,
                }
            })
            .collect();
        Ok(SkillsReport { skills })
    }

    /// One skill by name, resolved by precedence across argosies, with its
    /// entry-point content. Unknown names are errors, not empty results.
    pub fn get_skill(&self, params: GetSkillParams) -> Result<SkillContent> {
        let Some(listing) = self.context.resolve_skill(&params.name)? else {
            return Err(Error::ConceptNotFound {
                id: params.name.parse()?,
            });
        };
        let qid = QualifiedConceptId {
            argosy: listing.argosy.clone(),
            namespace: Namespace::Skill,
            id: listing.skill.entry_point.clone(),
        };
        let concept = self.context.resolve(&qid)?;
        let verified = verified_tier(&concept);
        Ok(SkillContent {
            skill: SkillOut {
                name: listing.skill.name,
                argosy: listing.argosy,
                uri: qid.to_uri(),
                description: listing.skill.description,
                shadowed: listing.shadowed,
                verified,
            },
            content: concept.to_string(),
        })
    }

    /// Direct read of a concept in the local argosy by bundle-relative path.
    /// Works for any namespace, used by harnesses for `memory/`.
    pub fn read_memory(&self, params: ReadPathParams) -> Result<UriContent> {
        let name = self.context.local().manifest().name().to_string();
        let uri = format!("argosy://{name}/{}", params.path);
        let concept = self.context.read_uri(&uri)?;
        Ok(UriContent {
            uri,
            content: concept.to_string(),
        })
    }

    /// Direct read of a concept from any active argosy by bundle-relative
    /// path — the read path for imported argosys, which the resource
    /// surface cannot serve (the resource protocol carries no `cwd`).
    /// A search hit from an imported argosy is fetched with `argosy` set to
    /// the hit's `argosy` name. Imported content is untrusted input (SEC-1).
    pub fn read_concept(&self, params: ReadParams) -> Result<ConceptContent> {
        let (name, kind) = match params.argosy.as_deref() {
            None => (self.context.local().manifest().name().to_string(), "local"),
            Some(name) => match self.context.argosy_named(name) {
                Some(crate::context::ArgosyRef::Local(_)) => (name.to_string(), "local"),
                Some(crate::context::ArgosyRef::Imported(_)) => (name.to_string(), "imported"),
                None => {
                    return Err(Error::UnknownArgosy {
                        name: name.to_string(),
                    });
                }
            },
        };
        let uri = format!("argosy://{name}/{}", params.path);
        let concept = self.context.read_uri(&uri)?;
        Ok(ConceptContent {
            uri,
            argosy: name,
            kind,
            content: concept.to_string(),
        })
    }

    /// Writes a memory concept (full markdown with frontmatter) to the local
    /// argosy, then reconciles the index so the concept is immediately
    /// searchable. A missing or empty frontmatter `type` is auto-filled as
    /// `type: Memory`. Overwrites report `action: "updated"` so silent
    /// destruction is never silent. Only the local argosy is writable.
    pub fn write_memory(&mut self, params: WriteParams) -> Result<WriteReport> {
        let id = concept_id(&params.path)?;
        let concept = parse_concept(&params.content)?;
        let existed = self.existed(&id);
        self.context.local().write_memory(&id, &concept)?;
        let action = if existed { "updated" } else { "created" };
        Ok(self.written_report(action, params.path, &id))
    }

    /// Deletes a memory concept from the local argosy, then reconciles the
    /// index so the deletion is immediately reflected in search.
    pub fn delete_memory(&mut self, params: ReadPathParams) -> Result<WriteReport> {
        let id = concept_id(&params.path)?;
        self.context.local().delete_memory(&id)?;
        Ok(self.deleted_report(params.path))
    }

    /// Writes a styleguide rule to the local argosy, enabling user rule
    /// extension, then reconciles the index. The namespace contract is
    /// validated by the library before anything touches disk.
    pub fn write_rule(&mut self, params: WriteParams) -> Result<WriteReport> {
        let id = concept_id(&params.path)?;
        let concept = parse_concept(&params.content)?;
        let existed = self.existed(&id);
        self.context.local().write_rule(&id, &concept)?;
        let action = if existed { "updated" } else { "created" };
        Ok(self.written_report(action, params.path, &id))
    }

    /// Deletes a styleguide rule from the local argosy, then reconciles the
    /// index.
    pub fn delete_rule(&mut self, params: ReadPathParams) -> Result<WriteReport> {
        let id = concept_id(&params.path)?;
        self.context.local().delete_rule(&id)?;
        Ok(self.deleted_report(params.path))
    }

    /// Writes a document concept (full markdown with frontmatter) to the
    /// local argosy, then reconciles the index so the document is
    /// immediately searchable. Overwrites report `action: "updated"`.
    /// Only the local argosy is writable.
    pub fn write_document(&mut self, params: WriteParams) -> Result<WriteReport> {
        let id = concept_id(&params.path)?;
        let concept = parse_concept(&params.content)?;
        let existed = self.existed(&id);
        self.context.local().write_document(&id, &concept)?;
        let action = if existed { "updated" } else { "created" };
        Ok(self.written_report(action, params.path, &id))
    }

    /// Deletes a document concept from the local argosy, then reconciles
    /// the index so the deletion is immediately reflected in search.
    pub fn delete_document(&mut self, params: ReadPathParams) -> Result<WriteReport> {
        let id = concept_id(&params.path)?;
        self.context.local().delete_document(&id)?;
        Ok(self.deleted_report(params.path))
    }

    /// True iff a concept file already exists at `id` — distinguishes
    /// `created` from `updated` in write reports.
    fn existed(&self, id: &ConceptId) -> bool {
        self.context
            .local()
            .root()
            .join(id.to_relative_path())
            .is_file()
    }

    /// Promotes a memory concept to a curated target, then reconciles the
    /// index so the new concept is immediately searchable. The outcome
    /// carries the untouched source and the drafted concept for the
    /// client's confirmation step.
    pub fn promote(&mut self, params: PromoteParams) -> Result<PromoteReport> {
        let source = concept_id(&params.source_path)?;
        let new_id = concept_id(&params.new_path)?;
        let target = match params.target {
            PromoteTarget::Document => PromotionTarget::Document,
            PromoteTarget::StyleguideRule => PromotionTarget::StyleguideRule,
        };
        let name = self.context.local().manifest().name().to_string();
        let source_content = self
            .context
            .read_uri(&format!("argosy://{name}/{}", params.source_path))?
            .to_string();
        let promotion = self.context.local().promote_memory(
            &source,
            target,
            &new_id,
            params.description.as_deref(),
        )?;
        let index_result = self.reindex();
        Ok(PromoteReport {
            source_uri: format!("argosy://{name}/{}", params.source_path),
            source_content,
            target: match params.target {
                PromoteTarget::Document => "document",
                PromoteTarget::StyleguideRule => "styleguide",
            },
            new_uri: format!("argosy://{name}/{}", params.new_path),
            drafted: promotion.drafted.to_string(),
            indexed: index_result.is_ok(),
            index_error: index_result.err(),
        })
    }

    fn written_report(
        &mut self,
        action: &'static str,
        path: String,
        id: &ConceptId,
    ) -> WriteReport {
        let index_result = self.reindex();
        // The file's real size, not the input's: the library may transform
        // the concept on write (memory auto-fills `type: Memory`). An
        // unreadable size is omitted rather than reported as 0.
        let bytes = std::fs::metadata(self.context.local().root().join(id.to_relative_path()))
            .ok()
            .map(|m| m.len());
        WriteReport {
            action,
            uri: format!("argosy://{}/{path}", self.context.local().manifest().name()),
            bytes,
            indexed: index_result.is_ok(),
            index_error: index_result.err(),
        }
    }

    fn deleted_report(&mut self, path: String) -> WriteReport {
        let index_result = self.reindex();
        WriteReport {
            action: "deleted",
            uri: format!("argosy://{}/{path}", self.context.local().manifest().name()),
            bytes: None,
            indexed: index_result.is_ok(),
            index_error: index_result.err(),
        }
    }

    /// Reads an `argosy://` resource: any concept in any active argosy via
    /// [`ProjectContext::read_uri`], plus the pseudo-resources
    /// [`ARGOSYS_URI`] and `argosy://<name>/_index`.
    pub fn read_resource(&self, uri: &str) -> Result<ResourceBody> {
        if uri == ARGOSYS_URI {
            let infos = self.argosy_infos();
            let text = serde_json::to_string_pretty(&ArgosysReport { argosys: infos })
                .expect("ArgosysReport always serializes");
            return Ok(ResourceBody {
                uri: uri.to_string(),
                text,
                mime: "application/json",
                meta: None,
            });
        }
        if let Some(name) = uri
            .strip_prefix("argosy://")
            .and_then(|rest| rest.strip_suffix(ARGOSY_INDEX_SUFFIX))
            && !name.is_empty()
        {
            return self.read_argosy_index(name, uri);
        }
        let concept = self.context.read_uri(uri)?;
        let qid = QualifiedConceptId::from_uri(uri).ok();
        let meta = qid
            .as_ref()
            .map(|q| identity_meta(&q.argosy, q.namespace.as_dir_name(), &q.id.to_string()));
        Ok(ResourceBody {
            uri: uri.to_string(),
            text: concept.to_string(),
            mime: "text/markdown",
            meta,
        })
    }

    fn read_argosy_index(&self, name: &str, uri: &str) -> Result<ResourceBody> {
        let Some(argosy_ref) = self.context.argosy_named(name) else {
            return Err(Error::UnknownArgosy {
                name: name.to_string(),
            });
        };
        let (root, writable) = match argosy_ref {
            crate::context::ArgosyRef::Local(local) => (local.root().to_path_buf(), true),
            crate::context::ArgosyRef::Imported(argosy) => (argosy.root().to_path_buf(), false),
        };
        let path = root.join("index.md");
        // The walk never follows symlinks (`SEC` containment policy); reads
        // enforce the same: a symlinked root index.md is invisible, not read.
        let meta = std::fs::symlink_metadata(&path);
        let is_file = meta.as_ref().is_ok_and(|m| m.is_file());
        let is_symlink = meta.as_ref().is_ok_and(|m| m.file_type().is_symlink());
        if !is_file || is_symlink {
            return Err(Error::ConceptNotFound {
                id: format!("{name}/index").parse()?,
            });
        }
        let text = std::fs::read_to_string(&path).map_err(|source| Error::Io {
            path: path.clone(),
            source,
        })?;
        Ok(ResourceBody {
            uri: uri.to_string(),
            text,
            mime: "text/markdown",
            meta: Some(meta_with_writable(name, writable)),
        })
    }

    /// The resources clients can browse statically: the argosys listing and
    /// every bundle's root `_index`. Concepts themselves are too numerous to
    /// enumerate; clients discover them via `_index` listings and resource
    /// templates.
    pub fn list_resources(&self) -> Result<Vec<ResourceDescriptor>> {
        let mut resources = vec![ResourceDescriptor {
            uri: ARGOSYS_URI.to_string(),
            name: "Active argosys".to_string(),
            description: "Every active argosy: name, version, and whether it is the writable local or a read-only import.".to_string(),
            mime: "application/json",
        }];
        for info in self.argosy_infos() {
            let index_path = self
                .context
                .argosy_named(&info.name)
                .map(|r| match r {
                    crate::context::ArgosyRef::Local(l) => l.root().join("index.md"),
                    crate::context::ArgosyRef::Imported(a) => a.root().join("index.md"),
                })
                .is_some_and(|p| {
                    std::fs::symlink_metadata(&p)
                        .is_ok_and(|m| m.is_file() && !m.file_type().is_symlink())
                });
            if index_path {
                resources.push(ResourceDescriptor {
                    uri: format!("argosy://{}{ARGOSY_INDEX_SUFFIX}", info.name),
                    name: format!("{} index", info.name),
                    description: format!(
                        "Root index.md of argosy `{}` — the progressive-disclosure entry point for browsing its concepts.",
                        info.name
                    ),
                    mime: "text/markdown",
                });
            }
        }
        Ok(resources)
    }

    fn argosy_infos(&self) -> Vec<ArgosyInfo> {
        let local = self.context.local();
        let mut infos = vec![ArgosyInfo {
            name: local.manifest().name().to_string(),
            argosy_version: local.manifest().argosy_version().to_string(),
            okf_version: local.manifest().okf_version().map(str::to_string),
            kind: "local",
        }];
        for imported in self.context.imported() {
            infos.push(ArgosyInfo {
                name: imported.manifest().name().to_string(),
                argosy_version: imported.manifest().argosy_version().to_string(),
                okf_version: imported.manifest().okf_version().map(str::to_string),
                kind: "imported",
            });
        }
        infos
    }
}

impl<P: EmbeddingProvider, S: VectorStore> McpState<P, S> {
    /// Multi-project server state over an opener: roots are opened by
    /// `factory` on first use and cached for the state's lifetime.
    pub fn new(factory: SessionFactory<P, S>) -> Self {
        Self {
            factory,
            sessions: HashMap::new(),
        }
    }

    /// The session for `cwd` (the project root), opening it on first use.
    /// The cache key is the canonicalized path, so different spellings of
    /// one directory share a session; a failed open is never cached.
    pub fn session(&mut self, cwd: impl AsRef<Path>) -> Result<&mut ProjectSession<P, S>> {
        let as_ref = cwd.as_ref();
        let key = as_ref
            .canonicalize()
            .unwrap_or_else(|_| as_ref.to_path_buf());
        if !self.sessions.contains_key(&key) {
            let session = (self.factory)(&key)?;
            self.sessions.insert(key.clone(), session);
        }
        Ok(self.sessions.get_mut(&key).expect("inserted above"))
    }

    /// The session for the process working directory — the project the
    /// resource surface serves (the MCP resource protocol carries no cwd).
    fn spawn_session(&mut self) -> Result<&mut ProjectSession<P, S>> {
        let cwd = std::env::current_dir().map_err(|source| Error::Io {
            path: ".".into(),
            source,
        })?;
        self.session(cwd)
    }

    /// Semantic search across every active argosy of the project named by
    /// `params.cwd`. Unknown `argosy` names error rather than silently
    /// returning nothing.
    pub fn search(&mut self, params: SearchParams) -> Result<SearchReport> {
        self.session(&params.cwd)?.search(params)
    }

    /// The review-flow query for the project named by `params.cwd`.
    pub fn search_rules(&mut self, params: RulesParams) -> Result<SearchReport> {
        self.session(&params.cwd)?.search_rules(params)
    }

    /// Every skill of the project named by `params.cwd`.
    pub fn list_skills(&mut self, params: ListSkillsParams) -> Result<SkillsReport> {
        self.session(&params.cwd)?.list_skills(params)
    }

    /// One skill of the project named by `params.cwd`, resolved by
    /// precedence across argosies, with its entry-point content.
    pub fn get_skill(&mut self, params: GetSkillParams) -> Result<SkillContent> {
        self.session(&params.cwd)?.get_skill(params)
    }

    /// Direct read of a concept in the local argosy of the project named
    /// by `params.cwd`.
    pub fn read_memory(&mut self, params: ReadPathParams) -> Result<UriContent> {
        self.session(&params.cwd)?.read_memory(params)
    }

    /// Direct read of a concept from any active argosy of the project named
    /// by `params.cwd` (local by default, an import by manifest name).
    pub fn read(&mut self, params: ReadParams) -> Result<ConceptContent> {
        self.session(&params.cwd)?.read_concept(params)
    }

    /// Writes a memory concept to the local argosy of the project named by
    /// `params.cwd`.
    pub fn write_memory(&mut self, params: WriteParams) -> Result<WriteReport> {
        self.session(&params.cwd)?.write_memory(params)
    }

    /// Deletes a memory concept from the local argosy of the project named
    /// by `params.cwd`.
    pub fn delete_memory(&mut self, params: ReadPathParams) -> Result<WriteReport> {
        self.session(&params.cwd)?.delete_memory(params)
    }

    /// Writes a styleguide rule to the local argosy of the project named by
    /// `params.cwd`.
    pub fn write_rule(&mut self, params: WriteParams) -> Result<WriteReport> {
        self.session(&params.cwd)?.write_rule(params)
    }

    /// Deletes a styleguide rule from the local argosy of the project
    /// named by `params.cwd`.
    pub fn delete_rule(&mut self, params: ReadPathParams) -> Result<WriteReport> {
        self.session(&params.cwd)?.delete_rule(params)
    }

    /// Writes a document concept to the local argosy of the project named
    /// by `params.cwd`.
    pub fn write_document(&mut self, params: WriteParams) -> Result<WriteReport> {
        self.session(&params.cwd)?.write_document(params)
    }

    /// Deletes a document concept from the local argosy of the project
    /// named by `params.cwd`.
    pub fn delete_document(&mut self, params: ReadPathParams) -> Result<WriteReport> {
        self.session(&params.cwd)?.delete_document(params)
    }

    /// Promotes a memory concept of the project named by `params.cwd` into
    /// a curated target.
    pub fn promote(&mut self, params: PromoteParams) -> Result<PromoteReport> {
        self.session(&params.cwd)?.promote(params)
    }

    /// Reads an `argosy://` resource of the process working directory's
    /// project.
    pub fn read_resource(&mut self, uri: &str) -> Result<ResourceBody> {
        self.spawn_session()?.read_resource(uri)
    }

    /// Lists the browsable resources of the process working directory's
    /// project.
    pub fn list_resources(&mut self) -> Result<Vec<ResourceDescriptor>> {
        self.spawn_session()?.list_resources()
    }
}
