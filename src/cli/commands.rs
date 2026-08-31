//! Subcommand handlers: argument parsing + one library call + output
//! formatting. Any real logic here is a bug — it belongs in the library.

use std::path::Path;
use std::process::ExitCode;

use argosy::index::{Filter, Query};
use argosy::package::{ImportReport, PackageOptions, PackageReport};
use argosy::{Argosy, LocalArgosy, Namespace, ValidationReport};

use crate::cli::args::*;
use crate::cli::{Output, current_dir};
use argosy::Result;

pub(super) fn cmd_init(out: &Output, args: &InitArgs) -> Result<ExitCode> {
    let default_project_path = args.path.is_none();
    // With the implicit project target, the bundle is named after the
    // project directory, not the state-dir slug.
    let (path, dir_name) = if default_project_path {
        let cwd = current_dir()?;
        let name = cwd.file_name().map(|n| n.to_string_lossy().into_owned());
        (
            argosy::pull::project_argosy_dir(&cwd)?.join(argosy::pull::LOCAL_ARGOSY_NAME),
            name,
        )
    } else {
        (
            args.path.clone().expect("checked default_project_path"),
            None,
        )
    };
    let name = args.name.clone().or(dir_name);
    let local = LocalArgosy::init(&path, name.as_deref(), args.description.as_deref())?;
    if out.json {
        let manifest = local.manifest();
        out.json(&serde_json::json!({
            "name": manifest.name(),
            "argosy_version": manifest.argosy_version(),
            "path": path,
        }))?;
    } else {
        let manifest = local.manifest();
        out.note(&format!(
            "created {} {} at {}",
            manifest.name(),
            manifest.argosy_version(),
            path.display()
        ));
    }
    Ok(ExitCode::SUCCESS)
}

pub(super) fn cmd_pull(out: &Output, args: &PullArgs) -> Result<ExitCode> {
    let root = if args.global {
        argosy::pull::global_argosy_dir()?
    } else {
        argosy::pull::project_argosy_dir(current_dir()?)?
    };
    let argosy = argosy::pull::clone_as_checkout(&args.url, &root, &args.name)?;
    let dest = root.join(&args.name);
    if out.json {
        out.json(&serde_json::json!({
            "name": argosy.manifest().name(),
            "argosy_version": argosy.manifest().argosy_version(),
            "path": dest,
            "global": args.global,
        }))?;
    } else {
        out.note(&format!(
            "pulled {} {} into {}",
            argosy.manifest().name(),
            argosy.manifest().argosy_version(),
            dest.display()
        ));
    }
    Ok(ExitCode::SUCCESS)
}

pub(super) fn cmd_validate(out: &Output, args: &ValidateArgs) -> Result<ExitCode> {
    let report = match args.namespace {
        None => Argosy::validate(&args.path),
        Some(Ns::Skill) => {
            // The namespace contracts are defined over an open argosy; a
            // path that cannot open is a command-level failure here (the
            // unscoped validator is the one that accepts broken fixtures).
            let argosy = Argosy::open(&args.path)?;
            ValidationReport::from_findings(argosy.validate_skills())
        }
        Some(Ns::Styleguide) => {
            let argosy = Argosy::open(&args.path)?;
            ValidationReport::from_findings(argosy.validate_styleguide())
        }
        Some(ns @ (Ns::Document | Ns::Memory)) => {
            let full = Argosy::validate(&args.path);
            let dir = Namespace::from(ns).as_dir_name().to_string();
            // Bundle-level findings (no path — manifest missing, root
            // problems) always survive the namespace filter: a bundle
            // with no `argosy.md` must never validate "OK" under any
            // scope. Path findings stay scoped to the namespace.
            let findings = full
                .findings()
                .iter()
                .filter(|f| {
                    f.path
                        .as_ref()
                        .is_none_or(|p| p.starts_with(Path::new(&dir)))
                })
                .cloned()
                .collect();
            ValidationReport::from_findings(findings)
        }
    };

    let conformant = report.is_conformant();
    if out.json {
        out.json(&report)?;
    } else if !conformant {
        print!("{report}");
    } else {
        // Conformant bundles always open (manifest errors are error
        // findings); fall back to a bare OK should open ever disagree.
        match Argosy::open(&args.path) {
            Ok(argosy) => {
                let manifest = argosy.manifest();
                out.note(&format!(
                    "OK: {} {}",
                    manifest.name(),
                    manifest.argosy_version()
                ));
            }
            Err(_) => out.note("OK"),
        }
    }
    Ok(if conformant {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

pub(super) fn cmd_package(out: &Output, args: &PackageArgs) -> Result<ExitCode> {
    // Gate packaging on validation (DIST-1's "only conformant bundles ship"
    // spirit): a broken bundle fails with its validation errors on stderr.
    let report = Argosy::validate(&args.source);
    if !report.is_conformant() {
        if out.json {
            out.json(&report)?;
        } else {
            eprint!("{report}");
        }
        return Ok(ExitCode::FAILURE);
    }
    let source = Argosy::open(&args.source)?;
    let options = PackageOptions {
        include_index: args.include_index,
        format: args.format.into(),
    };
    let report: PackageReport = argosy::package::package(&source, &args.dest, &options)?;
    if out.json {
        out.json(&report)?;
    } else {
        out.note(&format!(
            "packaged {} {}: {} file(s)",
            report.name, report.argosy_version, report.files_copied
        ));
    }
    for warning in &report.warnings {
        out.warn(warning);
    }
    Ok(ExitCode::SUCCESS)
}

pub(super) fn cmd_convert(out: &Output, args: &ConvertArgs) -> Result<ExitCode> {
    match &args.format {
        ConvertFormat::Styleguide(imp) => {
            // Implicit target is the current project's `default` argosy
            // under the user state dir, the same path `argosy init`
            // creates when none is given; a missing one is a setup
            // problem worth naming, not a bare I/O error on a hashed path.
            let argosy_path = match &imp.argosy_path {
                Some(path) => path.clone(),
                None => {
                    let path = argosy::pull::project_argosy_dir(current_dir()?)?
                        .join(argosy::pull::LOCAL_ARGOSY_NAME);
                    if !path.join("argosy.md").is_file() {
                        return Err(argosy::error::Error::Validation {
                            reason: format!(
                                "no local `default` argosy for this project at {} — run \
                                 `argosy init` in the project root",
                                path.display()
                            ),
                        });
                    }
                    path
                }
            };
            let local = LocalArgosy::open(&argosy_path)?;
            let report: ImportReport =
                argosy::package::import_styleguide_yaml(&local, &imp.yaml_dir)?;
            // An existing directory with no YAML files is almost always a
            // wrong path spelling — a silent "written: 0" success is how
            // imports get pointed at nothing.
            if report.yaml_files_seen == 0 && report.findings.is_empty() {
                out.warn(&format!(
                    "no .yaml or .yml files found in {} — nothing imported (check the path)",
                    imp.yaml_dir.display()
                ));
            }
            if out.json {
                out.json(&report)?;
            } else {
                out.note(&format!(
                    "written: {} rule(s); skipped (existing): {}",
                    report.written,
                    report.skipped_existing.len()
                ));
                for skipped in &report.skipped_existing {
                    out.note(&format!("skipped: {skipped}"));
                }
                if !report.findings.is_empty() {
                    print!(
                        "{}",
                        ValidationReport::from_findings(report.findings.clone())
                    );
                }
            }
            // An import is a bulk write, and the index reconciles on every
            // write: without this, freshly imported rules stay invisible to
            // search until a manual `index build` or the next MCP start.
            // A failed reconcile degrades retrieval without undoing the
            // import.
            if report.written > 0 && report.findings.is_empty() {
                #[cfg(feature = "default-index")]
                match reconcile_index_after_import() {
                    Ok(Some(reconciled)) => out.note(&format!(
                        "index reconciled: {} upserted, {} removed, {} unchanged",
                        reconciled.upserted, reconciled.removed, reconciled.unchanged
                    )),
                    Ok(None) => {}
                    Err(e) => out.warn(&format!(
                        "index reconcile failed ({e:#}) — run `argosy index build` to update it"
                    )),
                }
            }
            Ok(if report.findings.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
    }
}

/// Reconciles the project's index after `convert styleguide` wrote rules
/// into one of its argosys. `Ok(None)` — still without loading the model —
/// when no index db exists yet: a later `index build` starts fresh and sees
/// the imported rules, and creating one here would force a ~90 MB model
/// download as a side effect of a convert.
#[cfg(feature = "default-index")]
fn reconcile_index_after_import() -> Result<Option<argosy::index::IndexReport>> {
    use argosy::context::ProjectContext;
    use argosy::index::Index;
    use argosy::index::fastembed::FastembedProvider;
    use argosy::index::sqlite::SqliteVecStore;

    let root = current_dir()?;
    let db = argosy::pull::project_argosy_dir(&root)?.join(argosy::pull::INDEX_DB_NAME);
    if !db.is_file() {
        return Ok(None);
    }
    // Same UX as `index build`: say so on stderr so the pause while the
    // embedding model loads never reads as a hang.
    eprintln!("argosy: loading embedding model (first run downloads ~90 MB)…");
    let context = ProjectContext::open_project(&root)?;
    let store = SqliteVecStore::open(&db)?;
    let provider = FastembedProvider::new_default()?;
    let mut index = Index::new(provider, store);
    Ok(Some(index.reconcile(&context)?))
}

pub(super) fn cmd_agent(out: &Output, args: &AgentArgs) -> Result<ExitCode> {
    match &args.verb {
        AgentVerb::Reviewer(reviewer) => {
            // The project root is the working directory — the same scope
            // the index and mcp verbs use. Agent definitions are harness
            // config, so no argosy needs to exist here.
            let root = current_dir()?;
            let report = argosy::setup_reviewer(reviewer.harness.into(), &root, reviewer.force)?;
            if out.json {
                out.json(&report)?;
            } else {
                let verb = if report.overwritten {
                    "replaced"
                } else {
                    "created"
                };
                out.note(&format!(
                    "{verb} reviewer agent for {} at {}",
                    report.harness,
                    report.path.display()
                ));
                // The reviewer grounds findings in rules through the argosy
                // MCP server; without it, it degrades to ungrounded review
                // (the prompt says so) — a hint, not a warning.
                out.note(
                    "note: the reviewer grounds findings in styleguide rules via the argosy \
                     MCP server (`argosy mcp` on stdio) — register it with the harness to \
                     enable rule grounding",
                );
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

#[cfg(feature = "default-index")]
pub(super) fn cmd_index(out: &Output, args: &IndexArgs) -> Result<ExitCode> {
    use argosy::context::ProjectContext;
    use argosy::index::fastembed::FastembedProvider;
    use argosy::index::sqlite::SqliteVecStore;
    use argosy::index::{Index, VectorStore, staleness_report};

    // The project root is the working directory: discovery walks the
    // project's argosy store under the user state dir plus the global
    // store, all keyed by that root.
    let root = current_dir()?;
    let db = argosy::pull::project_argosy_dir(&root)?.join(argosy::pull::INDEX_DB_NAME);

    match &args.verb {
        IndexVerb::Status => {
            // The path must be a project before any rest answer makes
            // sense — otherwise "no index" would mask "not a project".
            let context = ProjectContext::open_project(&root)?;
            if !db.is_file() {
                out.note(&format!(
                    "no index at {} — run `argosy index build`",
                    db.display()
                ));
                if out.json {
                    out.json(&serde_json::json!({"index": null, "db": db}))?;
                }
                return Ok(ExitCode::SUCCESS);
            }
            // `status` is a read-only verb: it must work on a read-only
            // index and must never write (no directory creation, no pragma,
            // no DDL).
            let store = SqliteVecStore::open_read_only(&db)?;
            let expected_model = FastembedProvider::default_model_id()?;
            let stale = staleness_report(&context, &store, &expected_model)?;

            // Unit counts per argosy/namespace, derived from `unit_hashes`
            // keys: one unit per concept in v1's chunking (this would count
            // chunks, not concepts, if multi-chunk embedding ever lands).
            let mut units: Vec<(&str, &str, usize)> = Vec::new();
            let hashes = store.unit_hashes()?;
            for qid in hashes.keys() {
                let key = (qid.argosy.as_str(), qid.namespace.as_dir_name());
                match units.iter_mut().find(|(a, n, _)| (*a, *n) == key) {
                    Some((_, _, count)) => *count += 1,
                    None => units.push((key.0, key.1, 1)),
                }
            }
            units.sort();
            let total: usize = units.iter().map(|(_, _, n)| n).sum();

            if out.json {
                let by: Vec<serde_json::Value> = units
                    .iter()
                    .map(|(argosy, namespace, count)| {
                        serde_json::json!({
                            "argosy": argosy,
                            "namespace": namespace,
                            "units": count,
                        })
                    })
                    .collect();
                out.json(&serde_json::json!({
                    "db": db,
                    "model_id": store.model_id(),
                    "expected_model_id": expected_model,
                    "units": total,
                    "by_argosy_namespace": by,
                    "staleness": stale,
                }))?;
            } else {
                out.note(&format!(
                    "model: {}",
                    store.model_id().unwrap_or("<unrecorded>")
                ));
                out.note(&format!(
                    "expected model (current default): {expected_model}"
                ));
                out.note(&format!("units: {total}"));
                for (argosy, namespace, count) in &units {
                    out.note(&format!("  {argosy}/{namespace}: {count}"));
                }
                if stale.model_mismatch {
                    out.note("stale: model identity changed — `build` performs a full rebuild");
                } else if stale.added + stale.changed + stale.removed == 0 {
                    out.note(&format!(
                        "stale: up to date ({} unchanged)",
                        stale.unchanged
                    ));
                } else {
                    out.note(&format!(
                        "stale: {} added, {} changed, {} removed ({} unchanged)",
                        stale.added, stale.changed, stale.removed, stale.unchanged
                    ));
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        IndexVerb::Build => {
            let context = ProjectContext::open_project(&root)?;
            let store = SqliteVecStore::open(&db)?;
            // Loading the model takes a moment (and a ~90 MB download on a
            // cold cache): say so on stderr so the pause never reads as a
            // hang. stdout stays the machine-readable channel.
            eprintln!("argosy: loading embedding model (first run downloads ~90 MB)…");
            let provider = FastembedProvider::new_default()?;
            let mut index = Index::new(provider, store);
            let report = index.reconcile(&context)?;
            if out.json {
                out.json(&report)?;
            } else {
                let how = if report.rebuilt { "rebuilt" } else { "updated" };
                out.note(&format!(
                    "index {how}: {} upserted, {} removed, {} unchanged [{}]",
                    report.upserted, report.removed, report.unchanged, report.model_id
                ));
            }
            Ok(ExitCode::SUCCESS)
        }
        IndexVerb::Query(q) => {
            let context = ProjectContext::open_project(&root)?;
            if !db.is_file() {
                out.note(&format!(
                    "no index at {} — run `argosy index build`",
                    db.display()
                ));
                if out.json {
                    out.json(&serde_json::json!({"hits": [], "db": db}))?;
                }
                return Ok(ExitCode::SUCCESS);
            }
            // `query` is a read-only verb like `status`: it searches the
            // index as built (the query text is embedded in-process, never
            // in the store) and must work on a read-only index.
            let store = SqliteVecStore::open_read_only(&db)?;
            eprintln!("argosy: loading embedding model (first run downloads ~90 MB)…");
            let provider = FastembedProvider::new_default()?;
            let index = Index::new(provider, store);
            let query = Query {
                text: q.text.clone(),
                k: q.k,
                filter: build_filter(q),
            };
            let hits = index.search(&context, &query)?;
            if out.json {
                out.json(&hits)?;
            } else {
                for hit in &hits {
                    let description = hit.meta.description.as_deref().unwrap_or("");
                    println!(
                        "{:.4}  {}  —  {}",
                        hit.score,
                        hit.concept.to_uri(),
                        description
                    );
                }
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

#[cfg(not(feature = "default-index"))]
pub(super) fn cmd_index(_out: &Output, _args: &IndexArgs) -> Result<ExitCode> {
    eprintln!(
        "error: this `argosy` binary was built without the `default-index` feature; \
         rebuild with default features to use the index subcommand"
    );
    Ok(ExitCode::FAILURE)
}

#[cfg(all(feature = "mcp", feature = "default-index"))]
pub(super) fn cmd_mcp(_out: &Output, _args: &McpArgs) -> Result<ExitCode> {
    use std::sync::Arc;

    use argosy::context::ProjectContext;
    use argosy::error::Error;
    use argosy::index::Index;
    use argosy::index::fastembed::LazyFastembedProvider;
    use argosy::index::sqlite::SqliteVecStore;
    use argosy::mcp::{ArgosyMcpServer, McpState, ProjectSession, SessionFactory};
    use rmcp::ServiceExt;

    // No project is opened at startup: the server runs from any directory,
    // and every tool call names its project (`cwd`). Projects open lazily
    // through this factory and stay cached for the process lifetime.
    // stdout is the stdio protocol channel: every diagnostic is stderr.
    let factory: SessionFactory<LazyFastembedProvider, SqliteVecStore> = Arc::new(|root| {
        let context = ProjectContext::open_project(root)?;
        let store = SqliteVecStore::open(
            argosy::pull::project_argosy_dir(root)?.join(argosy::pull::INDEX_DB_NAME),
        )?;
        // The lazy provider makes the open instant and offline-tolerant:
        // the model (and its ~90 MB first-run download) loads only when
        // something actually needs embedding.
        let mut index = Index::new(LazyFastembedProvider::new_default()?, store);
        // A failed reconcile degrades retrieval, it must not fail the open
        // (spec §11: an out-of-date index degrades search quality, never
        // correctness) — warn on stderr and serve the session anyway;
        // mutating tools re-attempt reconciliation on every write.
        match index.reconcile(&context) {
            Ok(report) => eprintln!(
                "argosy mcp: {}: index reconciled ({} upserted, {} removed, {} unchanged)",
                root.display(),
                report.upserted,
                report.removed,
                report.unchanged
            ),
            Err(err) => eprintln!(
                "argosy mcp: {}: warning: index reconcile failed ({err:#}); serving degraded — \
                 search may error or miss changes until `argosy index build` succeeds",
                root.display()
            ),
        }
        Ok(ProjectSession::new(context, index))
    });
    let server = ArgosyMcpServer::new(McpState::new(factory));

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::Transport {
            reason: format!("failed to start the tokio runtime: {e}"),
        })?
        .block_on(async move {
            eprintln!("argosy mcp: serving on stdio");
            let service =
                server
                    .serve(rmcp::transport::stdio())
                    .await
                    .map_err(|e| Error::Transport {
                        reason: format!("MCP stdio handshake failed: {e}"),
                    })?;
            // `cancel()` would shut the server down immediately; wait for
            // the natural end (stdin EOF / client disconnect) instead.
            service.waiting().await.map_err(|e| Error::Transport {
                reason: format!("MCP server task failed: {e}"),
            })?;
            Ok::<(), Error>(())
        })?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(not(all(feature = "mcp", feature = "default-index")))]
pub(super) fn cmd_mcp(_out: &Output, _args: &McpArgs) -> Result<ExitCode> {
    eprintln!(
        "error: this `argosy` binary was built without the `mcp` feature; \
         rebuild with default features to use the mcp subcommand"
    );
    Ok(ExitCode::FAILURE)
}

/// Maps query flags 1:1 onto the library's [`Filter`].
#[cfg(feature = "default-index")]
pub(super) fn build_filter(q: &QueryArgs) -> Filter {
    Filter {
        namespaces: (!q.namespace.is_empty())
            .then(|| q.namespace.iter().map(|ns| (*ns).into()).collect()),
        argosies: (!q.argosy.is_empty()).then(|| q.argosy.clone()),
        concept_types: (!q.concept_type.is_empty()).then(|| q.concept_type.clone()),
        tags: (!q.tag.is_empty()).then(|| q.tag.clone()),
        language: q.language.clone(),
        category: q.category.clone(),
    }
}
