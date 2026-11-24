use std::fmt::Display;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::console::diag;
use crate::store::Placement;
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
                  Start with `trurlic init --placement tracked`, then add decision files\n\
                  under .trurlic/decisions/ and run `trurlic check`."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    #[command(flatten)]
    Project(ProjectCommand),

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
pub enum InstallIde {
    /// Claude Code, through `claude mcp add`
    ClaudeCode,
}

/// The commands that act on the project's `.trurlic/`, found from the
/// working directory.
#[derive(Subcommand, Debug)]
pub enum ProjectCommand {
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

    /// Show project status: component count, decision count, issues.
    Status,

    /// Validate `.trurlic/` and report each file not in canonical form.
    Check,

    /// Rewrite every file of `.trurlic/` that is not in canonical form.
    Fmt,

    /// Migrate `.trurlic/` to the current format version.
    Migrate {
        /// Show what would change without writing.
        #[arg(long)]
        dry_run: bool,
    },
}

/// Where the graph is versioned, as `init` takes it.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum PlacementArg {
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
pub enum QueryCommand {
    /// Find decisions that constrain a file or directory.
    ///
    /// Matches decisions anchored on the given path, on any file under it as
    /// a directory, or on a directory that holds it. Use when editing a file
    /// to discover which architectural decisions apply.
    File {
        /// Relative path from project root (e.g. `src/store/write.rs` or `src/store`).
        path: String,
    },
}

/// Parse the arguments and run the command, reporting a failure on stderr.
pub fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Project(command) => report(run(command)),
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
        ProjectCommand::Status => commands::status(&cwd),
        ProjectCommand::Check => commands::check(&cwd),
        ProjectCommand::Fmt => commands::fmt(&cwd),
        ProjectCommand::Migrate { dry_run } => {
            let mode = if dry_run {
                commands::DryRun::Yes
            } else {
                commands::DryRun::No
            };
            commands::migrate(&cwd, mode)
        }
    }
}
