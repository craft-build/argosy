//! The `argosy` binary: argument parsing + one library call + output
//! formatting per subcommand. Any real logic here is a bug — it belongs in
//! the library. Exit codes: `0` success, `1` command failure, `2` usage
//! error.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use argosy::error::Result;
#[cfg(feature = "default-index")]
use argosy::index::{Filter, Query};
use argosy::package::{ImportReport, PackageFormat, PackageOptions, PackageReport};
use argosy::{Argosy, LocalArgosy, Namespace, ValidationReport};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;

/// The `argosy` command line.
#[derive(Parser)]
#[command(
    name = "argosy",
    version,
    about = "Create, validate, package, and query OKF knowledge bundles"
)]
struct Cli {
    /// Emit machine-readable JSON on stdout (schema = the library's report
    /// types) instead of human-readable lines. Safety warnings still go to
    /// stderr.
    #[arg(long, global = true)]
    json: bool,

    /// Suppress non-error human output (findings, errors, and safety
    /// warnings still print).
    #[arg(short, long, global = true)]
    quiet: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Init(InitArgs),
    Validate(ValidateArgs),
    Package(PackageArgs),
    Pull(PullArgs),
    Index(IndexArgs),
    Convert(ConvertArgs),
    Mcp(McpArgs),
}

/// Serve the current project over the Model Context Protocol (stdio
/// transport — the channel editor/CLI harnesses spawn this process for)
/// so any MCP-compatible harness can search, read, and write its
/// argosys. The argosy set is the one discovered from the working
/// directory — what `argosy index build` indexes. **stdout is the
/// protocol channel**: diagnostics go to stderr. The embedding model
/// loads only when something needs embedding — a FIRST RUN downloads it
/// (~90 MB) into the fastembed cache; if the model is unavailable
/// (offline), the server still serves every non-search tool, and writes
/// report `indexed: false` until the model can load.
#[derive(Args)]
struct McpArgs {}

/// Clone an external argosy into this project's `.argosy/<name>` checkout
/// — the standard way a project consumes shared bundles (`argosy pull`
/// then `argosy index build`; no `--import` bookkeeping). The clone must
/// itself be a bundle with a manifest; anything else leaves no checkout.
/// The URL is handed to `git clone` verbatim (full history, git's URL
/// semantics — including `ext::` transport helpers): only pull from
/// repositories you trust.
#[derive(Args)]
struct PullArgs {
    /// Git URL or local path of the argosy repository.
    url: String,

    /// Checkout name (`.argosy/<name>`): `[A-Za-z0-9._-]` only. A checkout
    /// with this name must not already exist.
    name: String,

    /// Install into the user-wide argosy store
    /// (`$XDG_STATE_HOME/argosy/<name>`, falling back to
    /// `~/.local/state/argosy/<name>`) instead of this project's `.argosy/`
    /// — shared by every project, automatically included in every index.
    #[arg(long)]
    global: bool,
}

/// Create a new, empty argosy in `<path>`: the root `argosy.md` manifest
/// (version `0.1.0`) plus the four reserved namespace directories. Without
/// `<path>`, initializes this project's local bundle at `.argosy/default`.
/// Fails if the target already contains a manifest — a bundle is
/// initialized exactly once.
#[derive(Args)]
struct InitArgs {
    /// Directory to initialize. Defaults to `<cwd>/.argosy/default`, the
    /// project-local bundle location.
    path: Option<PathBuf>,

    /// The manifest `name`. Defaults to the target directory's basename
    /// (with the implicit `.argosy/default` target: this directory's
    /// basename). Only `[A-Za-z0-9._-]` are allowed — the name appears in
    /// `argosy://` URIs.
    #[arg(long)]
    name: Option<String>,

    /// Initial manifest `description`.
    #[arg(long)]
    description: Option<String>,
}

/// Validate a bundle on disk — lifecycle step 2 made scriptable. Works on
/// directories that are not openable argosys; every problem becomes a
/// finding line like `[ERROR STR-4] path/to/file.md: message`.
#[derive(Args)]
struct ValidateArgs {
    /// Directory to validate.
    path: PathBuf,

    /// Run only one namespace's checks. `skill`/`styleguide` run those
    /// namespace contracts standalone (the bundle must be openable for
    /// this); `document`/`memory` run the full structural validation
    /// filtered to findings under that namespace.
    #[arg(long, value_enum)]
    namespace: Option<Ns>,
}

/// Package a validated bundle into a distributable artifact. The bundle is
/// validated first — packaging a non-conformant bundle fails with the
/// validation errors shown. The bundle's `memory/` namespace is NEVER
/// included: local memory stays local.
#[derive(Args)]
struct PackageArgs {
    /// The bundle to package (must be an openable, conformant argosy).
    source: PathBuf,

    /// Destination: a directory path (`--format dir`) or a `.tar.gz` file
    /// (`--format tar.gz`). Materialization is failure-atomic: the artifact
    /// appears at `<dest>` only once it is complete.
    dest: PathBuf,

    /// The artifact format.
    #[arg(long, value_enum, default_value_t = Format::Dir)]
    format: Format,

    /// Also ship the `.argosy/` index cache in the artifact. Off
    /// by default: the index is derivative and rebuildable on demand.
    #[arg(long)]
    include_index: bool,
}

/// Operate on the semantic index of the current project: the local bundle
/// at `.argosy/default`, pulled checkouts at `.argosy/<name>`, and global
/// checkouts from the user store. The index lives at `.argosy/index.db` —
/// deleting it costs one `build`. Membership comes from checkout locations.
#[derive(Args)]
struct IndexArgs {
    #[command(subcommand)]
    verb: IndexVerb,
}

#[derive(Subcommand)]
enum IndexVerb {
    /// Bring the index in line with the bundles (reconcile: embed new or
    /// changed concepts, drop removed ones; O(hashes) when nothing
    /// changed). FIRST RUN downloads the embedding model (~90 MB) into the
    /// fastembed cache; later runs are offline.
    Build,

    /// Read-only status: the store's recorded model identity, unit counts
    /// per argosy/namespace, and a staleness preview — the diff `build`
    /// would apply, computed from content hashes only (no embed calls, no
    /// writes, no model load).
    Status,

    /// Semantic search over the index (reconciles first when the index is
    /// stale). Prints `[score] argosy://<argosy>/<concept-id> — description`
    /// per hit.
    Query(QueryArgs),
}

/// A semantic query. Narrowing flags each constrain one filter
/// facet; repeat `--namespace`/`--argosy`/`--type`/`--tag` to allow several
/// values.
#[derive(Args)]
struct QueryArgs {
    /// The natural-language query text.
    text: String,

    /// Return at most this many hits.
    #[arg(short = 'k', default_value_t = 5)]
    k: usize,

    /// Only hits in these namespaces.
    #[arg(long, value_enum)]
    namespace: Vec<Ns>,

    /// Only hits from these argosies, by manifest name. An unknown name is
    /// an error, not an empty result.
    #[arg(long)]
    argosy: Vec<String>,

    /// Only hits whose frontmatter `language` matches exactly.
    #[arg(long)]
    language: Option<String>,

    /// Only hits whose frontmatter `category` matches exactly.
    #[arg(long)]
    category: Option<String>,

    /// Only hits carrying at least one of these tags.
    #[arg(long)]
    tag: Vec<String>,

    /// Only hits whose frontmatter `type` is one of these.
    #[arg(long = "type")]
    concept_type: Vec<String>,
}

/// Convert external material into argosy concepts.
#[derive(Args)]
struct ConvertArgs {
    #[command(subcommand)]
    format: ConvertFormat,
}

#[derive(Subcommand)]
enum ConvertFormat {
    /// Convert a directory of legacy YAML styleguide rules into OKF
    /// Styleguide Rule concepts. Imports are additive and re-runnable:
    /// rules that already exist are skipped, never overwritten.
    Styleguide(ConvertStyleguideArgs),
}

/// Convert a legacy YAML styleguide directory into OKF rules.
#[derive(Args)]
struct ConvertStyleguideArgs {
    /// Directory of legacy YAML rule files.
    yaml_dir: PathBuf,

    /// The local argosy that receives the rules (under `styleguide/`).
    /// Defaults to the current project's `.argosy/default`.
    argosy_path: Option<PathBuf>,
}

/// CLI-only selector for the four reserved namespaces (clap-parseable;
/// custom namespaces are not addressable here).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Ns {
    Document,
    Skill,
    Memory,
    Styleguide,
}

impl From<Ns> for Namespace {
    fn from(ns: Ns) -> Self {
        match ns {
            Ns::Document => Self::Document,
            Ns::Skill => Self::Skill,
            Ns::Memory => Self::Memory,
            Ns::Styleguide => Self::Styleguide,
        }
    }
}

/// Packaging artifact format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Format {
    /// A plain directory tree — the artifact you would commit to git.
    Dir,
    /// A gzipped tar archive (`.tar.gz`).
    #[value(name = "tar.gz")]
    TarGz,
}

impl From<Format> for PackageFormat {
    fn from(format: Format) -> Self {
        match format {
            Format::Dir => Self::Directory,
            Format::TarGz => Self::TarGz,
        }
    }
}

/// Output control shared by all subcommands.
struct Output {
    json: bool,
    quiet: bool,
}

impl Output {
    /// Non-error human output; suppressed by `--quiet` and `--json`.
    fn note(&self, line: &str) {
        if !self.quiet && !self.json {
            println!("{line}");
        }
    }

    /// A safety warning: prints even under `--quiet` and `--json` — stderr
    /// is not part of the JSON contract, and the safeguard must never be
    /// silent.
    fn warn(&self, line: &str) {
        eprintln!("warning: {line}");
    }

    /// JSON stdout for machine consumers. A serialization failure is a
    /// command failure (exit 1) — never empty stdout plus a success code.
    fn json<T: Serialize>(&self, value: &T) -> Result<()> {
        match serde_json::to_string_pretty(value) {
            Ok(text) => {
                println!("{text}");
                Ok(())
            }
            Err(e) => Err(argosy::error::Error::Validation {
                reason: format!("failed to serialize JSON output: {e}"),
            }),
        }
    }
}

/// The library `Error` type of one subcommand execution; business failures
/// (non-conformant bundle, import findings) are the returned `ExitCode`.
fn cmd_result(out: &Output, command: &Command) -> Result<ExitCode> {
    match command {
        Command::Init(args) => cmd_init(out, args),
        Command::Validate(args) => cmd_validate(out, args),
        Command::Package(args) => cmd_package(out, args),
        Command::Pull(args) => cmd_pull(out, args),
        Command::Index(args) => cmd_index(out, args),
        Command::Convert(args) => cmd_convert(out, args),
        Command::Mcp(args) => cmd_mcp(out, args),
    }
}

/// Entry point: parse, dispatch, and map library errors to exit code 1.
pub fn run() -> ExitCode {
    let cli = Cli::parse();
    let out = Output {
        json: cli.json,
        quiet: cli.quiet,
    };
    match cmd_result(&out, &cli.command) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_init(out: &Output, args: &InitArgs) -> Result<ExitCode> {
    let default_project_path = args.path.is_none();
    let path = args.path.clone().unwrap_or_else(|| {
        Path::new(argosy::pull::PROJECT_ARGOSY_DIR).join(argosy::pull::LOCAL_ARGOSY_NAME)
    });
    // With the implicit `.argosy/default` target, the bundle is named after
    // the project directory, not literally "default".
    let name = args.name.clone().or_else(|| {
        default_project_path.then(|| {
            std::env::current_dir()
                .ok()
                .and_then(|cwd| cwd.file_name().map(|n| n.to_string_lossy().into_owned()))
        })?
    });
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

fn cmd_pull(out: &Output, args: &PullArgs) -> Result<ExitCode> {
    let root = if args.global {
        argosy::pull::global_argosy_dir()?
    } else {
        PathBuf::from(argosy::pull::PROJECT_ARGOSY_DIR)
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

fn cmd_validate(out: &Output, args: &ValidateArgs) -> Result<ExitCode> {
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

fn cmd_package(out: &Output, args: &PackageArgs) -> Result<ExitCode> {
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

fn cmd_convert(out: &Output, args: &ConvertArgs) -> Result<ExitCode> {
    match &args.format {
        ConvertFormat::Styleguide(imp) => {
            // Implicit target is the current project's `.argosy/default`,
            // the same path `argosy init` creates when none is given.
            let argosy_path = imp.argosy_path.clone().unwrap_or_else(|| {
                Path::new(argosy::pull::PROJECT_ARGOSY_DIR).join(argosy::pull::LOCAL_ARGOSY_NAME)
            });
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
            Ok(if report.findings.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
    }
}

#[cfg(feature = "default-index")]
fn cmd_index(out: &Output, args: &IndexArgs) -> Result<ExitCode> {
    use argosy::context::ProjectContext;
    use argosy::index::fastembed::FastembedProvider;
    use argosy::index::sqlite::SqliteVecStore;
    use argosy::index::{Index, VectorStore, staleness_report};

    // The project root is the working directory: discovery walks
    // `.argosy/<dirs>` plus the global user store from there.
    let root = std::env::current_dir().map_err(|source| argosy::error::Error::Io {
        path: ".".into(),
        source,
    })?;
    let db = root.join(".argosy/index.db");

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
        IndexVerb::Build | IndexVerb::Query(_) => {
            let context = ProjectContext::open_project(&root)?;
            let store = SqliteVecStore::open(&db)?;
            // Loading the model takes a moment (and a ~90 MB download on a
            // cold cache): say so on stderr so the pause never reads as a
            // hang. stdout stays the machine-readable channel.
            eprintln!("argosy: loading embedding model (first run downloads ~90 MB)…");
            let provider = FastembedProvider::new_default()?;
            let mut index = Index::new(provider, store);
            let report = index.reconcile(&context)?;
            match &args.verb {
                IndexVerb::Build => {
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
                IndexVerb::Status => unreachable!("handled above"),
            }
        }
    }
}

#[cfg(not(feature = "default-index"))]
fn cmd_index(_out: &Output, _args: &IndexArgs) -> Result<ExitCode> {
    eprintln!(
        "error: this `argosy` binary was built without the `default-index` feature; \
         rebuild with default features to use the index subcommand"
    );
    Ok(ExitCode::FAILURE)
}

#[cfg(all(feature = "mcp", feature = "default-index"))]
fn cmd_mcp(_out: &Output, _args: &McpArgs) -> Result<ExitCode> {
    use argosy::context::ProjectContext;
    use argosy::error::Error;
    use argosy::index::Index;
    use argosy::index::fastembed::LazyFastembedProvider;
    use argosy::index::sqlite::SqliteVecStore;
    use argosy::mcp::{ArgosyMcpServer, McpState};
    use rmcp::ServiceExt;

    // Startup runs to completion before the transport starts; the argosy
    // set is discovered from the working directory, exactly like the
    // index verbs. stdout is the stdio protocol channel: every
    // diagnostic is stderr.
    let root = std::env::current_dir().map_err(|source| argosy::error::Error::Io {
        path: ".".into(),
        source,
    })?;
    let context = ProjectContext::open_project(&root)?;
    let db = root.join(".argosy/index.db");
    let store = SqliteVecStore::open(&db)?;
    // The lazy provider makes startup instant and offline-tolerant: the
    // model (and its ~90 MB first-run download) loads only when something
    // actually needs embedding.
    let mut index = Index::new(LazyFastembedProvider::new_default()?, store);
    // A failed reconcile degrades retrieval, it must not kill the server
    // (spec §11: an out-of-date index degrades search quality, never
    // correctness) — warn on stderr and keep serving; mutating tools
    // re-attempt reconciliation on every write.
    match index.reconcile(&context) {
        Ok(report) => eprintln!(
            "argosy mcp: index reconciled ({} upserted, {} removed, {} unchanged)",
            report.upserted, report.removed, report.unchanged
        ),
        Err(err) => eprintln!(
            "argosy mcp: warning: index reconcile failed ({err:#}); serving degraded — \
             search may error or miss changes until `argosy index build` succeeds"
        ),
    }

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::Transport {
            reason: format!("failed to start the tokio runtime: {e}"),
        })?
        .block_on(async move {
            let state = McpState::new(context, index);
            eprintln!("argosy mcp: serving on stdio");
            let service = ArgosyMcpServer::new(state)
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
fn cmd_mcp(_out: &Output, _args: &McpArgs) -> Result<ExitCode> {
    eprintln!(
        "error: this `argosy` binary was built without the `mcp` feature; \
         rebuild with default features to use the mcp subcommand"
    );
    Ok(ExitCode::FAILURE)
}

/// Maps query flags 1:1 onto the library's [`Filter`].
#[cfg(feature = "default-index")]
fn build_filter(q: &QueryArgs) -> Filter {
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

#[cfg(all(test, feature = "default-index"))]
mod tests {
    use super::*;

    fn parse_query(argv: &[&str]) -> QueryArgs {
        let cli = Cli::try_parse_from(argv).expect("argv parses");
        let Command::Index(IndexArgs {
            verb: IndexVerb::Query(q),
            ..
        }) = cli.command
        else {
            panic!("expected `index query` argv, got a different command");
        };
        q
    }

    #[test]
    fn query_flags_map_to_filter_fields() {
        let q = parse_query(&[
            "argosy",
            "index",
            "query",
            "oauth refresh tokens",
            "-k",
            "3",
            "--namespace",
            "skill",
            "--namespace",
            "skill",
            "--namespace",
            "styleguide",
            "--argosy",
            "acme-billing",
            "--language",
            "rust",
            "--category",
            "naming",
            "--tag",
            "auth",
            "--tag",
            "api",
            "--type",
            "Styleguide Rule",
        ]);
        let filter = build_filter(&q);
        assert_eq!(q.k, 3);
        assert_eq!(
            filter.namespaces,
            Some(vec![
                Namespace::Skill,
                Namespace::Skill,
                Namespace::Styleguide
            ])
        );
        assert_eq!(
            filter.argosies.as_deref(),
            Some(&["acme-billing".to_string()][..])
        );
        assert_eq!(filter.language.as_deref(), Some("rust"));
        assert_eq!(filter.category.as_deref(), Some("naming"));
        assert_eq!(
            filter.tags,
            Some(vec!["auth".to_string(), "api".to_string()])
        );
        assert_eq!(
            filter.concept_types,
            Some(vec!["Styleguide Rule".to_string()])
        );
    }

    #[test]
    fn unscoped_query_leaves_every_filter_field_none() {
        let q = parse_query(&["argosy", "index", "query", "anything"]);
        assert_eq!(q.k, 5, "default k");
        let filter = build_filter(&q);
        // 1:1 flag mapping: no flags means no constraints anywhere.
        assert!(filter.namespaces.is_none());
        assert!(filter.argosies.is_none());
        assert!(filter.concept_types.is_none());
        assert!(filter.tags.is_none());
        assert!(filter.language.is_none());
        assert!(filter.category.is_none());
    }
}

#[cfg(test)]
mod mcp_parse_tests {
    use super::*;

    #[test]
    fn mcp_parses_with_no_flags_and_stdio_is_the_only_transport() {
        let cli = Cli::try_parse_from(["argosy", "mcp"]).expect("argv parses");
        let Command::Mcp(_args) = cli.command else {
            panic!("expected mcp argv");
        };

        // The HTTP transport was removed (unauthenticated network exposure
        // of destructive tools): neither flag may parse at all.
        for flag in ["--transport", "--bind"] {
            assert!(
                Cli::try_parse_from(["argosy", "mcp", flag, "x"]).is_err(),
                "`{flag}` must not parse"
            );
        }
    }

    #[test]
    fn mcp_takes_no_argosy_selection_flags() {
        // Membership comes from `.argosy/` + global-store discovery, not from
        // a hand-maintained flag list: neither flag may parse at all.
        for flag in ["--project-root", "--import"] {
            assert!(
                Cli::try_parse_from(["argosy", "mcp", flag, "x"]).is_err(),
                "`{flag}` must not parse"
            );
        }
    }
}
