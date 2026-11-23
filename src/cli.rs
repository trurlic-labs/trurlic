use std::fmt::Display;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::console::{self, diag, out};
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
                  Start with `trurlic init`, then `trurlic add component <name>`."
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
    Init,

    /// Add a component or connection.
    #[command(subcommand)]
    Add(AddCommand),

    /// Rename a component (updates all references atomically).
    #[command(subcommand)]
    Rename(RenameCommand),

    /// Remove a component, decision, pattern or connection.
    #[command(subcommand)]
    Remove(RemoveCommand),

    /// Record a user-attributed decision on a component or `project`.
    Decide {
        /// Component this decision belongs to (or "project" for project-wide).
        component: String,

        /// What was decided.
        #[arg(long)]
        choice: String,

        /// Why — your reasoning.
        #[arg(long)]
        reason: String,

        /// Alternative considered and rejected (repeatable).
        #[arg(long = "alternative", short = 'a')]
        alternatives: Vec<String>,

        /// Source code location where this decision manifests (repeatable).
        /// Format: `file` or `file::symbol`. Split on the first `::` —
        /// Rust-path-like symbols should use the terminal segment (e.g.
        /// `Store::lock` → `lock`).
        #[arg(long = "ref")]
        refs: Vec<String>,
    },

    /// Reclaim decisions that have lost their anchor: orphaned (component
    /// gone), orphaned-ref (all referenced files deleted), or long-unreviewed
    /// agent decisions. Reports what would be reclaimed by default; pass
    /// `--apply` to write the removals.
    Gc {
        /// Actually write the removals (default: dry-run).
        #[arg(long)]
        apply: bool,

        /// Also remove orphaned-ref and long-unreviewed agent decisions, not
        /// just orphans.
        #[arg(long)]
        aggressive: bool,

        /// Skip the interactive confirmation prompt for `--aggressive --apply`.
        /// Required in non-interactive environments (CI, piped output).
        #[arg(long)]
        yes: bool,

        /// Deprecated no-op — dry-run is now the default.
        #[arg(long, hide = true)]
        dry_run: bool,
    },

    /// Start the MCP server for AI coding agent integration.
    Serve,

    /// Query the decision graph.
    #[command(subcommand)]
    Query(QueryCommand),

    /// Show project status: component count, decision count, issues.
    Status,

    /// Validate `.trurlic/` internal consistency.
    Check,

    /// Migrate `.trurlic/` to the current format version.
    Migrate {
        /// Show what would change without writing.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum AddCommand {
    /// Define a new component.
    Component {
        /// Kebab-case component name (e.g. `auth`, `rate-limiter`).
        name: String,

        /// One-line description of what this component does.
        #[arg(long, short = 'd')]
        description: Option<String>,
    },

    /// Connect two components (directional: from → to).
    Connection {
        /// Source component name.
        from: String,

        /// Target component name.
        to: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum RenameCommand {
    /// Rename a component, updating all references atomically.
    Component {
        /// Current component name.
        old: String,

        /// New component name (kebab-case).
        new: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum RemoveCommand {
    /// Remove a component (refuses if decisions reference it).
    Component {
        /// Component name to remove.
        name: String,
    },

    /// Remove a decision, or every agent-recorded decision in a component.
    Decision {
        /// Decision filename (without `.toml`). Omit when using `--agent`.
        name: Option<String>,

        /// Remove every agent-recorded decision in a component at once. The
        /// batch is atomic — if any removal is cascade-blocked, none happen.
        #[arg(long, requires = "component", conflicts_with = "name")]
        agent: bool,

        /// Component to scope `--agent` bulk removal to.
        #[arg(long, requires = "agent")]
        component: Option<String>,
    },

    /// Remove a pattern and its edges; its member decisions are kept.
    Pattern {
        /// Pattern name (without `.toml`).
        name: String,
    },

    /// Remove a connection between two components.
    Connection {
        /// Source component name.
        from: String,

        /// Target component name.
        to: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum QueryCommand {
    /// Find decisions that constrain a file or directory.
    ///
    /// Matches decisions whose code_refs reference the given path (exact match)
    /// or any file under it (directory prefix). Use when editing a file to
    /// discover which architectural decisions apply.
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
        ProjectCommand::Init => commands::init(&cwd),
        ProjectCommand::Add(sub) => match sub {
            AddCommand::Component { name, description } => {
                commands::add_component(&cwd, &name, description.as_deref())
            }
            AddCommand::Connection { from, to } => commands::add_connection(&cwd, &from, &to),
        },
        ProjectCommand::Rename(sub) => match sub {
            RenameCommand::Component { old, new } => commands::rename_component(&cwd, &old, &new),
        },
        ProjectCommand::Remove(sub) => match sub {
            RemoveCommand::Component { name } => commands::remove_component(&cwd, &name),
            RemoveCommand::Decision {
                name,
                agent,
                component,
            } => match (name, agent, component) {
                (Some(name), false, _) => commands::remove_decision(&cwd, &name),
                (None, true, Some(component)) => commands::remove_agent_decisions(&cwd, &component),
                _ => Err(Error::Validation(
                    "specify a decision name, or `--agent --component <name>` to bulk-remove \
                     agent decisions"
                        .into(),
                )),
            },
            RemoveCommand::Connection { from, to } => commands::remove_connection(&cwd, &from, &to),
            RemoveCommand::Pattern { name } => commands::remove_pattern(&cwd, &name),
        },
        ProjectCommand::Decide {
            component,
            choice,
            reason,
            alternatives,
            refs,
        } => {
            let code_refs: Vec<_> = refs
                .iter()
                .map(|r| commands::parse_code_ref_arg(r))
                .collect();
            commands::decide(
                &cwd,
                &component,
                &choice,
                &reason,
                &alternatives,
                &code_refs,
            )
        }
        ProjectCommand::Gc {
            apply,
            aggressive,
            yes,
            dry_run: _,
        } => {
            let scope = if aggressive {
                commands::GcScope::Aggressive
            } else {
                commands::GcScope::Safe
            };
            let execution = if apply {
                commands::GcExecution::Apply
            } else {
                commands::GcExecution::DryRun
            };
            if aggressive && apply {
                match commands::resolve_aggressive_confirm(yes, console::stdout_is_terminal())? {
                    commands::AggressiveConfirm::Confirmed => {}
                    commands::AggressiveConfirm::PromptUser => {
                        let question = "Aggressive gc will permanently remove orphaned-ref \
                                        and unreviewed agent decisions. Continue?";
                        if !console::confirm(question)? {
                            out!("Aborted.")?;
                            return Ok(());
                        }
                    }
                }
            }
            commands::gc(&cwd, scope, execution)
        }
        ProjectCommand::Query(sub) => match sub {
            QueryCommand::File { path } => commands::query_file(&cwd, &path),
        },
        ProjectCommand::Serve => commands::serve(&cwd),
        ProjectCommand::Status => commands::status(&cwd),
        ProjectCommand::Check => commands::check(&cwd),
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
