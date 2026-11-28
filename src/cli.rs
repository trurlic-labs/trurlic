use std::fmt::Display;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::builder::PossibleValuesParser;
use clap::{Parser, Subcommand};

use crate::console::diag;
use crate::store::Placement;
use crate::tools::{self, ToolError};
use crate::{Error, Result, commands};

#[derive(Parser, Debug)]
#[command(
    name = "trurlic",
    version,
    propagate_version = true,
    disable_help_subcommand = true,
    about = "Structured architectural decisions that constrain AI code generation.",
    long_about = "Trurlic captures architectural decisions and serves them to AI coding agents\n\
                  via MCP.\n\n\
                  Start with `trurlic init --placement tracked`, then record decisions\n\
                  with `trurlic apply <file>` or the `apply` tool, and run `trurlic check`."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    #[command(flatten)]
    Project(ProjectCommand),

    #[command(flatten)]
    Tool(ToolCommand),

    /// Register the trurlic MCP server with Claude Code, for the current user.
    Install {
        /// The agent to register with.
        #[arg(long, value_enum)]
        ide: InstallIde,

        /// Path to the trurlic binary to embed in the config.
        /// Defaults to the currently running executable.
        #[arg(long)]
        binary_path: Option<PathBuf>,

        /// Print the `claude mcp add` command without running it.
        #[arg(long)]
        dry_run: bool,
    },
}

/// The agents `install` registers with.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum InstallIde {
    /// Claude Code, through `claude mcp add`
    ClaudeCode,
}

/// The commands that act on the project's `.trurlic/`, found from the
/// working directory.
#[derive(Subcommand, Debug)]
enum ProjectCommand {
    /// Initialize a new `.trurlic/` directory in the current project.
    Init {
        /// Whether the project's git versions `.trurlic/` (`tracked`) or
        /// `.trurlic/` stays out of it (`shared`).
        #[arg(long, value_enum)]
        placement: PlacementArg,
    },

    /// Start the MCP server for AI coding agent integration.
    Serve,

    /// Query the decision graph.
    #[command(subcommand)]
    Query(QueryCommand),

    /// Validate `.trurlic/` and report each file not in canonical form.
    Check,

    /// Rewrite every file of `.trurlic/` that is not in canonical form.
    Fmt,

    /// Migrate `.trurlic/` to the current format version.
    Migrate {
        /// Show what would change without writing.
        #[arg(long)]
        dry_run: bool,

        /// The placement to write: `tracked` when the project's git
        /// versions `.trurlic/`, `shared` when it stays out of it. Without
        /// it, git tells: `tracked` when it tracks every file of
        /// `.trurlic/`, `shared` when it tracks none.
        #[arg(long, value_enum)]
        placement: Option<PlacementArg>,
    },
}

/// The commands that run a tool's own code on the project's graph and
/// print its result, `--json` as the tool returns it: the CLI twins of the
/// MCP tools, and `map`, a tool no MCP client lists.
#[derive(Subcommand, Debug)]
enum ToolCommand {
    /// Apply a batch of ops to the graph, all or none, in one commit.
    ///
    /// The file holds the JSON object the `apply` tool takes: `ops`, and an
    /// optional `origin`.
    Apply {
        /// The batch file, or `-` for stdin.
        file: PathBuf,

        /// Print the tool's JSON result instead of a line per op.
        #[arg(long)]
        json: bool,
    },

    /// Show the graph at a glance: counts, issues, history, components.
    Status {
        /// Show this section alone, with the whole result budget.
        #[arg(long, value_parser = PossibleValuesParser::new(tools::section_names()))]
        section: Option<String>,

        /// Count only the decisions recorded under this origin.
        #[arg(long)]
        origin: Option<String>,

        /// With `--section lint`: the quality findings of the decisions
        /// named this or after, where a call whose page ended said to read
        /// on; such a call compares no likely duplicate pair.
        #[arg(long)]
        from: Option<String>,

        /// Print the tool's JSON result.
        #[arg(long)]
        json: bool,
    },

    /// Print the code tree as JSON: one level, the nodes one node holds
    /// with the lines, definitions and decisions in and under each and the
    /// imports between them; with `--frame`, one file; with `--search`,
    /// the nodes and definitions a text finds.
    Map(MapCommand),
}

/// The argument and flags of `trurlic map`: one unit, a level, a frame or
/// a search, and where its page starts.
#[derive(clap::Args, Debug)]
#[command(group(clap::ArgGroup::new("item_unit").args(["frame", "search"])))]
struct MapCommand {
    /// The address of the node whose children to list, as a level gives
    /// it; the crates, and what lies outside every crate, without it.
    #[arg(conflicts_with_all = ["frame", "search"])]
    node: Option<String>,

    /// The address of a file, as a level gives it: its frame, its lines,
    /// definitions and the decisions on each line range.
    #[arg(long, value_name = "ADDRESS", conflicts_with = "search")]
    frame: Option<String>,

    /// A text to find in the names and paths of the tree's nodes and
    /// definitions.
    #[arg(long, value_name = "QUERY")]
    search: Option<String>,

    /// The address of the first child of a level to list: a level page's
    /// `next`.
    #[arg(long, value_name = "ADDRESS", conflicts_with = "item_unit")]
    from: Option<String>,

    /// The position of the first item of a frame or a search to list: a
    /// frame or search page's `next`.
    #[arg(long, value_name = "N", requires = "item_unit")]
    from_position: Option<usize>,

    /// Print the tool's JSON result, the one form `map` prints.
    #[arg(long, required = true)]
    json: bool,
}

impl MapCommand {
    /// The unit these name, with where its page starts: clap admits
    /// `--from` with a level alone and `--from-position` with a frame or a
    /// search alone.
    fn unit(&self) -> commands::MapUnit<'_> {
        let from = self.from_position;
        match (self.frame.as_deref(), self.search.as_deref()) {
            (Some(file), _) => commands::MapUnit::Frame { file, from },
            (None, Some(query)) => commands::MapUnit::Search { query, from },
            (None, None) => commands::MapUnit::Level {
                node: self.node.as_deref(),
                from: self.from.as_deref(),
            },
        }
    }
}

/// Where the graph is versioned, as `init` and `migrate` take it.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum PlacementArg {
    /// The project's git versions `.trurlic/`.
    Tracked,
    /// `.trurlic/` stays out of the project's git.
    Shared,
}

impl From<PlacementArg> for Placement {
    fn from(placement: PlacementArg) -> Self {
        match placement {
            PlacementArg::Tracked => Self::Tracked,
            PlacementArg::Shared => Self::Shared,
        }
    }
}

#[derive(Subcommand, Debug)]
enum QueryCommand {
    /// Find decisions that constrain a file or directory.
    ///
    /// Matches decisions anchored on the given path, on any file under it as
    /// a directory, or on a directory that holds it. Use when editing a file
    /// to discover which architectural decisions apply.
    File {
        /// Relative path from project root (e.g. `src/store/commit.rs` or `src/store`).
        path: String,
    },
}

/// Parse the arguments and run the command, reporting a failure on stderr.
pub fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Project(command) => report(run(command)),
        Command::Tool(command) => report(run_tool(command)),
        Command::Install {
            ide: InstallIde::ClaudeCode,
            binary_path,
            dry_run,
        } => {
            let mode = if dry_run {
                commands::DryRun::Yes
            } else {
                commands::DryRun::No
            };
            report(commands::install(binary_path.as_deref(), mode))
        }
    }
}

fn run_tool(command: ToolCommand) -> std::result::Result<(), ToolError> {
    let cwd = std::env::current_dir().map_err(Error::system("read the working directory"))?;
    let format = |json: bool| {
        if json {
            commands::Format::Json
        } else {
            commands::Format::Text
        }
    };
    match command {
        ToolCommand::Apply { file, json } => commands::apply(&cwd, &file, format(json)),
        ToolCommand::Status {
            section,
            origin,
            from,
            json,
        } => commands::status(
            &cwd,
            commands::StatusFlags {
                section: section.as_deref(),
                origin: origin.as_deref(),
                from: from.as_deref(),
            },
            format(json),
        ),
        // `--json` is required: the JSON is the one form `map` prints.
        ToolCommand::Map(map) => commands::map(&cwd, map.unit()),
    }
}

/// The exit status of a finished command, its error printed on stderr. Every
/// command's error type goes through here, so all fail with status 1 and
/// `error: <message>`.
fn report(outcome: std::result::Result<(), impl Display>) -> ExitCode {
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            diag!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(command: ProjectCommand) -> Result<()> {
    let cwd = std::env::current_dir().map_err(Error::system("read the working directory"))?;
    match command {
        ProjectCommand::Init { placement } => commands::init(&cwd, placement.into()),
        ProjectCommand::Query(sub) => match sub {
            QueryCommand::File { path } => commands::query_file(&cwd, &path),
        },
        ProjectCommand::Serve => commands::serve(&cwd),
        ProjectCommand::Check => commands::check(&cwd),
        ProjectCommand::Fmt => commands::fmt(&cwd),
        ProjectCommand::Migrate { dry_run, placement } => {
            let mode = if dry_run {
                commands::DryRun::Yes
            } else {
                commands::DryRun::No
            };
            commands::migrate(&cwd, mode, placement.map(Placement::from))
        }
    }
}
