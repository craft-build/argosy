//! Subcommand handlers: argument parsing + one library call + output
//! formatting. Any real logic here is a bug — it belongs in the library.

use std::path::Path;
use std::process::ExitCode;

#[cfg(feature = "default-index")]
use argosy::index::{EvalDataset, Filter, Query};
use argosy::package::{ImportReport, PackageOptions, PackageReport};
use argosy::{Argosy, LocalArgosy, Namespace, ValidationReport};

use crate::cli::args::*;
use crate::cli::{Output, current_dir, embed_cache_override};
use argosy::Config;
use argosy::Result;

pub(super) fn cmd_init(out: &Output, args: &InitArgs) -> Result<ExitCode> {
    let default_project_path = args.path.is_none();
    // With the implicit project target, the bundle is named after the
    // project directory, not the state-dir slug.
    let (path, dir_name) = if default_project_path {
        let cwd = current_dir()?;
        let name = cwd.file_name().map(|n| n.to_string_lossy().into_owned());
        // The catalog maps this root record back to the slot to flag stale ones.
        argosy::pull::record_project_root(&cwd)?;
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

pub(super) fn cmd_pull(out: &Output, config: &Config, args: &PullArgs) -> Result<ExitCode> {
    let global = args.global.unwrap_or(config.pull.default_global);
    let root = if global {
        argosy::pull::global_argosy_dir()?
    } else {
        // A project-scoped pull creates (or joins) the project's slot:
        // record its canonical root for the catalog.
        let cwd = current_dir()?;
        argosy::pull::record_project_root(&cwd)?;
        argosy::pull::project_argosy_dir(cwd)?
    };
    let argosy = argosy::pull::clone_as_checkout(&args.url, &root, &args.name)?;
    let dest = root.join(&args.name);
    // Any sidecar on an accepted clone passed verification (clone_as_checkout refuses mismatches).
    let integrity = argosy::pull::integrity_status(&dest);
    if out.json {
        out.json(&serde_json::json!({
            "name": argosy.manifest().name(),
            "argosy_version": argosy.manifest().argosy_version(),
            "path": dest,
            "global": global,
            "integrity": integrity,
        }))?;
    } else {
        out.note(&format!(
            "pulled {} {} into {} (integrity: {})",
            argosy.manifest().name(),
            argosy.manifest().argosy_version(),
            dest.display(),
            integrity
        ));
    }
    Ok(ExitCode::SUCCESS)
}

pub(super) fn cmd_validate(out: &Output, args: &ValidateArgs) -> Result<ExitCode> {
    let report = match args.namespace {
        None => Argosy::validate(&args.path),
        Some(Ns::Skill) => {
            // Scoped validation needs an open argosy: a path that cannot
            // open fails here (the unscoped validator accepts broken
            // fixtures).
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
            // Bundle-level findings (no path) always survive the namespace
            // filter: a bundle with no `argosy.md` must never validate "OK"
            // under any scope.
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
        // Conformant bundles always open; fall back to a bare OK should open ever disagree.
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

pub(super) fn cmd_package(out: &Output, config: &Config, args: &PackageArgs) -> Result<ExitCode> {
    // Gate packaging on validation: a broken bundle fails with its validation errors on stderr.
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
        include_index: args.include_index.unwrap_or(config.package.include_index),
        format: args
            .format
            .unwrap_or_else(|| Format::from(config.package.format))
            .into(),
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

pub(super) fn cmd_convert(out: &Output, config: &Config, args: &ConvertArgs) -> Result<ExitCode> {
    match &args.format {
        ConvertFormat::Styleguide(imp) => {
            // A missing implicit target is a setup problem worth naming, not a bare I/O error.
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
            // wrong path spelling; warn rather than a silent "written: 0".
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
            // Without this, imported rules stay invisible to search until
            // a manual `index build` or the next MCP start; a failed
            // reconcile degrades retrieval without undoing the import.
            if report.written > 0 && report.findings.is_empty() {
                #[cfg(feature = "default-index")]
                match reconcile_index_after_import(config) {
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

/// Builds and prints (or writes) the global catalog; the scan is
/// read-only and only `--write` touches disk (the catalog file).
pub(super) fn cmd_catalog(out: &Output, config: &Config, args: &CatalogArgs) -> Result<ExitCode> {
    use argosy::catalog::{CatalogOptions, redact_home, render_markdown, write};

    let state_root = argosy::pull::state_dir()?;
    let mut catalog = argosy::catalog::build(&state_root, &CatalogOptions::from_config(config))?;
    let redact = args.redact_home.unwrap_or(config.catalog.redact_home);
    if redact && let Some(home) = argosy::config::home_dir() {
        redact_home(&mut catalog, &home);
    }

    let written = if args.write {
        Some(write(&state_root, &catalog)?)
    } else {
        None
    };

    if out.json {
        out.json(&catalog)?;
    } else if written.is_none() {
        print!("{}", render_markdown(&catalog));
    }
    if let Some(path) = written {
        out.note(&format!("wrote catalog to {}", path.display()));
    }
    Ok(ExitCode::SUCCESS)
}

/// Reconciles the project's index after `convert styleguide` wrote rules.
///
/// `Ok(None)` when no index db exists yet, so the convert never forces a
/// ~90 MB model download; a later `index build` sees the imported rules.
#[cfg(feature = "default-index")]
fn reconcile_index_after_import(config: &Config) -> Result<Option<argosy::index::IndexReport>> {
    use argosy::context::ProjectContext;
    use argosy::index::Index;
    use argosy::index::sqlite::SqliteVecStore;

    let root = current_dir()?;
    let db = argosy::pull::project_argosy_dir(&root)?.join(config.index_db_name());
    if !db.is_file() {
        return Ok(None);
    }
    // Announce on stderr so the model-load pause never reads as a hang;
    // lexical mode loads nothing and says nothing.
    let provider = runtime_provider(config)?;
    if provider.loads_model() {
        eprintln!("argosy: loading embedding model (first run downloads ~90 MB)…");
    }
    let context = ProjectContext::open_project(&root)?;
    let store = SqliteVecStore::open(&db)?;
    let mut index = Index::new(provider, store).with_chunk_config(config.chunk_config());
    Ok(Some(index.reconcile(&context)?))
}

pub(super) fn cmd_agent(out: &Output, args: &AgentArgs) -> Result<ExitCode> {
    match &args.verb {
        AgentVerb::Reviewer(reviewer) => {
            // Agent definitions are harness config, so no argosy needs to exist here.
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
                // Without the MCP server the reviewer degrades to
                // ungrounded review — a hint, not a warning.
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

/// Terminal-only build progress; never writes to stdout or to redirected
/// stderr. A trailing newline is emitted even if reconciliation fails.
#[cfg(feature = "default-index")]
struct IndexProgress {
    enabled: bool,
    displayed: bool,
}

#[cfg(feature = "default-index")]
impl IndexProgress {
    fn new(out: &Output) -> Self {
        use std::io::IsTerminal;
        Self {
            enabled: !out.quiet && std::io::stderr().is_terminal(),
            displayed: false,
        }
    }

    fn update(&mut self, completed: usize, total: usize) {
        if !self.enabled || total == 0 {
            return;
        }
        use std::io::Write;
        let width = total.to_string().len();
        let _ = write!(
            std::io::stderr(),
            "\rargosy: embedding {completed:width$}/{total} passages"
        );
        let _ = std::io::stderr().flush();
        self.displayed = true;
    }
}

#[cfg(feature = "default-index")]
impl Drop for IndexProgress {
    fn drop(&mut self) {
        if self.displayed {
            eprintln!();
        }
    }
}

#[cfg(feature = "default-index")]
pub(super) fn cmd_index(out: &Output, config: &Config, args: &IndexArgs) -> Result<ExitCode> {
    use argosy::context::ProjectContext;
    use argosy::index::sqlite::SqliteVecStore;
    use argosy::index::{Index, VectorStore, staleness_report};

    // Discovery walks the project's argosy store under the user state dir
    // plus the global store, all keyed by the project root.
    let root = current_dir()?;
    let db = argosy::pull::project_argosy_dir(&root)?.join(config.index_db_name());

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
            // `status` must work on a read-only index and never write.
            let store = SqliteVecStore::open_read_only(&db)?;
            // The model identity the current mode's next build would
            // record; a mismatch is the usual full-rebuild signal, not a
            // silent half-mode index.
            let expected_model = match config.index.search_mode {
                argosy::config::SearchModeConfig::Hybrid => model_spec(config)?.model_id(),
                argosy::config::SearchModeConfig::Lexical => {
                    argosy::index::LEXICAL_ONLY_MODEL_ID.to_string()
                }
            };
            let stale = staleness_report(&context, &store, &expected_model, config.chunk_config())?;

            // `unit_hashes` yields one key per concept (concepts hold
            // several passages; `index build`'s report counts those).
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
                            "concepts": count,
                        })
                    })
                    .collect();
                out.json(&serde_json::json!({
                    "db": db,
                    "model_id": store.model_id(),
                    "expected_model_id": expected_model,
                    "concepts": total,
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
                out.note(&format!("concepts: {total}"));
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
            // Announce the pause on stderr — never stdout, the
            // machine-readable channel — and not in lexical mode.
            let provider = runtime_provider(config)?;
            note_model_load(out, &provider);
            let mut index = Index::new(provider, store).with_chunk_config(config.chunk_config());
            let mut progress = IndexProgress::new(out);
            let report = index
                .reconcile_with_progress(&context, |done, total| progress.update(done, total))?;
            drop(progress);
            if out.json {
                out.json(&report)?;
            } else {
                let how = if report.rebuilt { "rebuilt" } else { "updated" };
                out.note(&format!(
                    "index {how}: {} concept(s) ({} passage(s)) upserted, {} removed, {} unchanged [{}]",
                    report.upserted,
                    report.passages,
                    report.removed,
                    report.unchanged,
                    report.model_id
                ));
            }
            // Invisible-Unicode flags stripped from indexed text — loud by design.
            for warning in &report.warnings {
                out.warn(warning);
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
            // Read-only like `status`: the query text embeds in-process, never in the store.
            let store = SqliteVecStore::open_read_only(&db)?;
            let provider = runtime_provider(config)?;
            note_model_load(out, &provider);
            let index = Index::new(provider, store);
            let query = Query {
                text: q.text.clone(),
                k: q.k.unwrap_or(config.index.default_k),
                filter: build_filter(q),
            };
            let hits = index.search(&context, &query)?;
            if out.json {
                out.json(&hits)?;
            } else {
                for hit in &hits {
                    let description = hit.meta.description.as_deref().unwrap_or("");
                    let section = if hit.heading_path.is_empty() {
                        format!("  [lines {}-{}]", hit.start_line, hit.end_line)
                    } else {
                        format!(
                            "  [{} · lines {}-{}]",
                            hit.heading_path, hit.start_line, hit.end_line
                        )
                    };
                    println!(
                        "{:.4}  {}  —  {}{section}",
                        hit.score,
                        hit.concept.to_uri(),
                        description
                    );
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        IndexVerb::Eval(args) => {
            let context = ProjectContext::open_project(&root)?;
            if !db.is_file() {
                out.note(&format!(
                    "no index at {} — run `argosy index build`",
                    db.display()
                ));
                if out.json {
                    out.json(&serde_json::json!({"report": null, "db": db}))?;
                }
                return Ok(ExitCode::SUCCESS);
            }
            // Read-only like `query`: the eval measures the built index as-is.
            let store = SqliteVecStore::open_read_only(&db)?;
            let provider = runtime_provider(config)?;
            note_model_load(out, &provider);
            let index = Index::new(provider, store);

            let mut dataset = read_eval_dataset(&args.dataset)?;
            apply_floor_overrides(&mut dataset, args)?;
            let report = argosy::index::eval::run_eval(&index, &context, &dataset)?;

            if out.json {
                out.json(&report)?;
            } else {
                print_eval_report(out, &report);
            }
            // Warnings print even under `--report-only`: the floors are still
            // evaluated; the flag only rescues the exit code.
            if !report.passed {
                for check in report.checks.iter().filter(|check| !check.passed) {
                    out.warn(&format!(
                        "eval below threshold: {} observed {:.3} < required {:.3}",
                        check.metric, check.observed, check.required
                    ));
                }
                if args.report_only {
                    Ok(ExitCode::SUCCESS)
                } else {
                    Ok(ExitCode::FAILURE)
                }
            } else {
                Ok(ExitCode::SUCCESS)
            }
        }
    }
}

#[cfg(not(feature = "default-index"))]
pub(super) fn cmd_index(_out: &Output, _config: &Config, _args: &IndexArgs) -> Result<ExitCode> {
    eprintln!(
        "error: this `argosy` binary was built without the `default-index` feature; \
         rebuild with default features to use the index subcommand"
    );
    Ok(ExitCode::FAILURE)
}

/// Reads and parses the eval dataset; I/O and JSON failures both name the path.
#[cfg(feature = "default-index")]
fn read_eval_dataset(path: &Path) -> Result<EvalDataset> {
    let text = std::fs::read_to_string(path).map_err(|source| argosy::error::Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_str(&text).map_err(|source| argosy::error::Error::Validation {
        reason: format!(
            "failed to parse eval dataset `{}`: {source}",
            path.display()
        ),
    })
}

/// Applies `--min-recall`/`--min-mrr` over the dataset's floors, replacing
/// per-`k` values the flags repeat.
#[cfg(feature = "default-index")]
fn apply_floor_overrides(dataset: &mut EvalDataset, args: &EvalArgs) -> Result<()> {
    let reject =
        |reason: String| -> Result<()> { Err(argosy::error::Error::Validation { reason }) };
    for pair in &args.min_recall {
        let Some((k, floor)) = pair.split_once('=') else {
            return reject(format!(
                "--min-recall expects <k>=<floor>, got `{pair}` (e.g. --min-recall 10=0.85)"
            ));
        };
        let k = k
            .trim()
            .parse::<usize>()
            .map_err(|_| argosy::error::Error::Validation {
                reason: format!("--min-recall: `{k}` is not an integer k"),
            })?;
        let floor = floor
            .trim()
            .parse::<f32>()
            .map_err(|_| argosy::error::Error::Validation {
                reason: format!("--min-recall: `{floor}` is not a number"),
            })?;
        dataset.min.with_recall(k, floor);
    }
    if let Some(mrr) = args.min_mrr {
        dataset.min.with_mrr(mrr);
    }
    Ok(())
}

/// Prints the human-readable eval report: per-query misses and violations,
/// aggregate recall per `k`, MRR, and the gate verdict.
#[cfg(feature = "default-index")]
fn print_eval_report(out: &Output, report: &argosy::index::EvalReport) {
    for (index, score) in report.queries.iter().enumerate() {
        out.note(&format!(
            "{:>3}. {:<48}  recall@max {:.2}  rr {:.2}",
            index + 1,
            truncate_at(&score.query, 48),
            score
                .recall_at_k
                .iter()
                .map(|(_, recall)| *recall)
                .next_back()
                .unwrap_or(0.0),
            score.reciprocal_rank
        ));
        for (uri, rank) in &score.ranks {
            if rank.is_none() {
                out.note(&format!("       miss  {uri}"));
            }
        }
        for (uri, rank) in &score.absent_violations {
            out.note(&format!("       drift  {uri} ranked {rank}"));
        }
    }
    for (k, recall) in &report.recall_at_k {
        out.note(&format!("recall@{k:<4} {recall:.3}"));
    }
    out.note(&format!("mrr      {:.3}", report.mrr));
    for check in &report.checks {
        let verdict = if check.passed { "ok" } else { "FAIL" };
        out.note(&format!(
            "floor    {} observed {:.3} required {:.3}  {verdict}",
            check.metric, check.observed, check.required
        ));
    }
    if report.checks.is_empty() {
        out.note("floor    none (report-only dataset)");
    }
}

/// Shortens `text` to `width` characters plus an ellipsis.
#[cfg(feature = "default-index")]
fn truncate_at(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        text.to_string()
    } else {
        let mut cut: String = text.chars().take(width - 1).collect();
        cut.push('…');
        cut
    }
}

#[cfg(all(feature = "mcp", feature = "default-index"))]
pub(super) fn cmd_mcp(_out: &Output, config: &Config, _args: &McpArgs) -> Result<ExitCode> {
    use std::sync::Arc;

    use argosy::context::ProjectContext;
    use argosy::error::Error;
    use argosy::index::Index;
    use argosy::index::sqlite::SqliteVecStore;
    use argosy::mcp::{ArgosyMcpServer, McpState, ProjectSession, SessionFactory};
    use rmcp::ServiceExt;

    let default_k = config.index.mcp_default_k;
    let db_name = config.index_db_name().to_string();
    let embed_cache = embed_cache_override(config);
    let spec = model_spec(config)?;
    let chunk_config = config.chunk_config();
    // Search mode is fixed for the process; hybrid loads lazily, lexical never.
    let search_mode = config.index.search_mode;
    // Trust tiers come from local configuration, never a book's self-claim (SEC-2).
    let trust = config.trust_policy();
    // Projects open lazily through this factory (each tool call names its
    // project via `cwd`); stdout is the stdio protocol channel: every
    // diagnostic is stderr.
    let factory: SessionFactory<RuntimeProvider, SqliteVecStore> = Arc::new(move |root| {
        let context = ProjectContext::open_project(root)?;
        let store = SqliteVecStore::open(argosy::pull::project_argosy_dir(root)?.join(&db_name))?;
        // The lazy provider keeps the open instant: the model loads only
        // when something actually needs embedding.
        let provider = match search_mode {
            argosy::config::SearchModeConfig::Hybrid => RuntimeProvider::Hybrid(Box::new(
                argosy::index::tract::LazyTractProvider::new(spec, embed_cache.clone())?,
            )),
            argosy::config::SearchModeConfig::Lexical => {
                RuntimeProvider::Lexical(argosy::index::LexicalOnlyProvider)
            }
        };
        let mut index = Index::new(provider, store).with_chunk_config(chunk_config);
        // A failed reconcile must not fail the open — it degrades search
        // quality, never correctness; mutating tools re-attempt on every
        // write.
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
        Ok(ProjectSession::new(context, index)
            .with_default_k(default_k)
            .with_trust_policy(trust.clone()))
    });
    #[allow(unused_mut)]
    let mut state = McpState::new(factory)
        .with_session_limits(config.session.project_ttl(), config.session.project_max);
    // The decision endpoint is process-global and shared by every session;
    // disabled unless configured.
    #[cfg(feature = "decision")]
    {
        let provider = argosy::decision::provider_from_config(&config.decision)?;
        state = state.with_decision(Arc::from(provider));
        if config.decision.enabled {
            eprintln!(
                "argosy mcp: decision endpoint {} enabled (advisory, fail-open)",
                config.decision.endpoint().unwrap_or("<none>")
            );
        }
    }
    // Code tools are confined to the spawn workspace unless the user opts out.
    #[cfg(feature = "code-tools")]
    let code = {
        #[allow(unused_mut)]
        let mut tools = argosy::codetools::CodeTools::default()
            .with_external_paths(config.code_tools.allow_external_paths)
            .with_review_limits(config.session.review_ttl(), config.session.review_max);
        #[cfg(feature = "decision")]
        {
            tools = tools.with_decision(Arc::clone(state.decision()));
        }
        tools
    };
    let server = ArgosyMcpServer::new(state);
    #[cfg(feature = "code-tools")]
    let server = server.with_code_tools(Arc::new(code));

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
            // `cancel()` would shut the server down immediately; wait for the natural end.
            service.waiting().await.map_err(|e| Error::Transport {
                reason: format!("MCP server task failed: {e}"),
            })?;
            Ok::<(), Error>(())
        })?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(not(all(feature = "mcp", feature = "default-index")))]
pub(super) fn cmd_mcp(_out: &Output, _config: &Config, _args: &McpArgs) -> Result<ExitCode> {
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

/// Resolves the configured embedding model, erroring with the known names on a typo.
#[cfg(feature = "default-index")]
fn model_spec(config: &Config) -> Result<argosy::index::tract::ModelSpec> {
    let name = config.model_name();
    argosy::index::tract::ModelSpec::from_name(name).ok_or_else(|| {
        argosy::error::Error::Validation {
            reason: format!(
                "unknown embedding model `{name}` (known models: {})",
                argosy::index::tract::ModelSpec::ALL
                    .iter()
                    .map(|spec| spec.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    })
}

/// The configured search mode as one concrete provider type, so every
/// index construction site names a single `Index<RuntimeProvider, _>`.
///
/// Hybrid resolves the lazily loaded real model (boxed for size); lexical
/// resolves the zero-model embedder, which never downloads anything.
#[cfg(feature = "default-index")]
enum RuntimeProvider {
    Hybrid(Box<argosy::index::tract::LazyTractProvider>),
    Lexical(argosy::index::LexicalOnlyProvider),
}

#[cfg(feature = "default-index")]
impl RuntimeProvider {
    /// True when the next reconcile/search may load or download the ~90 MB embedding model.
    fn loads_model(&self) -> bool {
        matches!(self, Self::Hybrid(_))
    }
}

#[cfg(feature = "default-index")]
impl argosy::index::EmbeddingProvider for RuntimeProvider {
    fn model_id(&self) -> &str {
        match self {
            Self::Hybrid(provider) => provider.model_id(),
            Self::Lexical(provider) => provider.model_id(),
        }
    }

    fn dimensions(&self) -> usize {
        match self {
            Self::Hybrid(provider) => provider.dimensions(),
            Self::Lexical(provider) => provider.dimensions(),
        }
    }

    fn embed(&self, texts: &[String]) -> argosy::error::Result<Vec<Vec<f32>>> {
        match self {
            Self::Hybrid(provider) => provider.embed(texts),
            Self::Lexical(provider) => provider.embed(texts),
        }
    }

    fn embed_query(&self, texts: &[String]) -> argosy::error::Result<Vec<Vec<f32>>> {
        match self {
            Self::Hybrid(provider) => provider.embed_query(texts),
            Self::Lexical(provider) => provider.embed_query(texts),
        }
    }
}

/// Builds the configured provider for one invocation; hybrid stays lazy
/// (the ~90 MB model loads on first embed), lexical loads nothing.
#[cfg(feature = "default-index")]
fn runtime_provider(config: &Config) -> Result<RuntimeProvider> {
    Ok(match config.index.search_mode {
        argosy::config::SearchModeConfig::Hybrid => {
            RuntimeProvider::Hybrid(Box::new(argosy::index::tract::LazyTractProvider::new(
                model_spec(config)?,
                embed_cache_override(config),
            )?))
        }
        argosy::config::SearchModeConfig::Lexical => {
            RuntimeProvider::Lexical(argosy::index::LexicalOnlyProvider)
        }
    })
}

/// Announces the model-load pause on stderr so it never reads as a hang.
#[cfg(feature = "default-index")]
fn note_model_load(out: &Output, provider: &RuntimeProvider) {
    if provider.loads_model() && !out.quiet {
        eprintln!("argosy: loading embedding model (first run downloads ~90 MB)…");
    }
}

/// Prints the resolved configuration (defaults merged with the user's
/// BarkML files) and the search location it came from.
pub(super) fn cmd_config(out: &Output, _args: &ConfigArgs, config: &Config) -> Result<ExitCode> {
    let location = argosy::config::config_dir();
    if out.json {
        out.json(&serde_json::json!({
            "config_dir": location,
            "config": config,
        }))?;
    } else {
        out.note(&format!(
            "config location: {}",
            location
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<none>".to_string())
        ));
        out.note(&format!(
            "output: quiet={}, json={}",
            config.output.quiet, config.output.json
        ));
        out.note(&format!(
            "paths: embed_cache_dir={}, index_db_name={}",
            config
                .embed_cache_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<default>".to_string()),
            config.index_db_name()
        ));
        out.note(&format!(
            "pull: default_global={}",
            config.pull.default_global
        ));
        out.note(&format!(
            "index: model={}, default_k={}, mcp_default_k={}",
            config.model_name(),
            config.index.default_k,
            config.index.mcp_default_k
        ));
        out.note(&format!(
            "package: format={}, include_index={}",
            match config.package.format {
                argosy::config::PackageFormatConfig::Dir => "dir",
                argosy::config::PackageFormatConfig::TarGz => "tar.gz",
            },
            config.package.include_index
        ));
        out.note(&format!(
            "catalog: redact_home={}",
            config.catalog.redact_home
        ));
        out.note(&format!(
            "code_tools: allow_external_paths={}",
            config.code_tools.allow_external_paths
        ));
        out.note(&format!(
            "session: project_ttl_secs={}, project_max={}, review_ttl_secs={}, review_max={}",
            config.session.project_ttl_secs,
            config.session.project_max,
            config.session.review_ttl_secs,
            config.session.review_max
        ));
    }
    Ok(ExitCode::SUCCESS)
}
