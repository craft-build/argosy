//! Subcommand argument definitions.

use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};

use argosy::Namespace;
use argosy::package::PackageFormat;

#[derive(Args)]
/// Serve argosys over the Model Context Protocol (stdio; diagnostics on
/// stderr). Tools select the project per call via `cwd`; each project's
/// first use opens it and downloads the embedding model (~90 MB).
pub(super) struct McpArgs {}

/// Clone an external argosy into this project's argosy store (under the
/// user state dir, outside the project tree).
#[derive(Args)]
pub(super) struct PullArgs {
    /// Git URL or local path of the argosy repository.
    pub(super) url: String,

    /// Checkout name (under the project's argosy store).
    pub(super) name: String,

    /// Install into the user-wide global argosy store instead of this
    /// project's.
    #[arg(long)]
    pub(super) global: bool,
}

/// Create a new, empty argosy: the `argosy.md` manifest plus the reserved
/// namespace directories.
#[derive(Args)]
pub(super) struct InitArgs {
    /// Directory to initialize (default: this project's `default` argosy
    /// under the user state dir).
    pub(super) path: Option<PathBuf>,

    /// Manifest name (default: the target directory's basename).
    #[arg(long)]
    pub(super) name: Option<String>,

    /// Initial manifest description.
    #[arg(long)]
    pub(super) description: Option<String>,
}

/// Validate a bundle on disk and print findings.
#[derive(Args)]
pub(super) struct ValidateArgs {
    /// Directory to validate.
    pub(super) path: PathBuf,

    /// Run only one namespace's checks.
    #[arg(long, value_enum)]
    pub(super) namespace: Option<Ns>,
}

/// Package a bundle into a distributable artifact (validated first). The
/// `memory/` namespace is NEVER included.
#[derive(Args)]
pub(super) struct PackageArgs {
    /// The bundle to package.
    pub(super) source: PathBuf,

    /// Destination directory or `.tar.gz` file.
    pub(super) dest: PathBuf,

    /// The artifact format.
    #[arg(long, value_enum, default_value_t = Format::Dir)]
    pub(super) format: Format,

    /// Also ship the `.argosy/` index cache in the artifact.
    #[arg(long)]
    pub(super) include_index: bool,
}

/// Operate on the project's semantic index (`index.db` under the user
/// state dir, outside the project tree).
#[derive(Args)]
pub(super) struct IndexArgs {
    #[command(subcommand)]
    pub(super) verb: IndexVerb,
}

#[derive(Subcommand)]
pub(super) enum IndexVerb {
    /// Bring the index in line with the bundles. The first run downloads
    /// the embedding model (~90 MB).
    Build,

    /// Read-only index status and staleness preview.
    Status,

    /// Semantic search over the index.
    Query(QueryArgs),
}

/// A semantic query; repeatable flags allow several values.
#[derive(Args)]
pub(super) struct QueryArgs {
    /// The natural-language query text.
    pub(super) text: String,

    /// Return at most this many hits.
    #[arg(short = 'k', default_value_t = 5)]
    pub(super) k: usize,

    /// Only hits in these namespaces.
    #[arg(long, value_enum)]
    pub(super) namespace: Vec<Ns>,

    /// Only hits from these argosies, by manifest name.
    #[arg(long)]
    pub(super) argosy: Vec<String>,

    /// Only hits whose frontmatter `language` matches exactly.
    #[arg(long)]
    pub(super) language: Option<String>,

    /// Only hits whose frontmatter `category` matches exactly.
    #[arg(long)]
    pub(super) category: Option<String>,

    /// Only hits carrying at least one of these tags.
    #[arg(long)]
    pub(super) tag: Vec<String>,

    /// Only hits whose frontmatter `type` is one of these.
    #[arg(long = "type")]
    pub(super) concept_type: Vec<String>,
}

/// Convert external material into argosy concepts.
#[derive(Args)]
pub(super) struct ConvertArgs {
    #[command(subcommand)]
    pub(super) format: ConvertFormat,
}

#[derive(Subcommand)]
pub(super) enum ConvertFormat {
    /// Convert legacy YAML styleguide rules into OKF Styleguide Rule
    /// concepts; existing rules are skipped, never overwritten.
    Styleguide(ConvertStyleguideArgs),
}

/// Convert a legacy YAML styleguide directory into OKF rules.
#[derive(Args)]
pub(super) struct ConvertStyleguideArgs {
    /// Directory of legacy YAML rule files.
    pub(super) yaml_dir: PathBuf,

    /// Argosy that receives the rules (default: this project's `default`
    /// argosy under the user state dir).
    pub(super) argosy_path: Option<PathBuf>,
}

/// Install agent definitions for a coding harness.
#[derive(Args)]
pub(super) struct AgentArgs {
    #[command(subcommand)]
    pub(super) verb: AgentVerb,
}

#[derive(Subcommand)]
pub(super) enum AgentVerb {
    /// Write the read-only `reviewer` subagent into the current project's
    /// harness agent directory.
    Reviewer(ReviewerArgs),
}

/// Install the reviewer subagent for one harness.
#[derive(Args)]
pub(super) struct ReviewerArgs {
    /// The coding harness to install the reviewer for.
    pub(super) harness: HarnessOpt,

    /// Replace an existing reviewer definition instead of failing.
    #[arg(long)]
    pub(super) force: bool,
}

/// CLI-only selector for the four reserved namespaces (clap-parseable;
/// custom namespaces are not addressable here).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(super) enum Ns {
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
pub(super) enum Format {
    /// A plain directory tree.
    Dir,
    /// A gzipped tar archive.
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

/// A coding harness argosy can install agent definitions into. Explicit
/// value names: clap's default kebab-casing would spell OpenCode
/// `open-code`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(super) enum HarnessOpt {
    /// OpenCode (`.opencode/agents/`).
    #[value(name = "opencode")]
    OpenCode,
    /// Claude Code (`.claude/agents/`).
    Claude,
    /// Kiro IDE or kiro-cli (`.kiro/agents/`).
    #[value(name = "kiro-cli")]
    KiroCli,
}

impl From<HarnessOpt> for argosy::Harness {
    fn from(opt: HarnessOpt) -> Self {
        match opt {
            HarnessOpt::OpenCode => Self::OpenCode,
            HarnessOpt::Claude => Self::Claude,
            HarnessOpt::KiroCli => Self::KiroCli,
        }
    }
}
