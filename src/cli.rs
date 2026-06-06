//! CLI argument parsing and command dispatch.
//!
//! All subcommand types are defined here with `clap::derive`. The binary
//! parses arguments and calls [`run`] to dispatch.

use clap::{Parser, Subcommand};

use crate::{Error, Result, commands};

/// Trurl — structured architectural decisions that constrain AI code generation.
#[derive(Parser, Debug)]
#[command(
    name = "trurl",
    version,
    propagate_version = true,
    disable_help_subcommand = true,
    about = "Structured architectural decisions that constrain AI code generation.",
    long_about = "Trurl captures architectural decisions, serves them to AI coding agents\n\
                  via MCP, and provides an interactive map for visual exploration.\n\n\
                  Start with `trurl init`, then `trurl add component <name>`."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Initialize a new `.trurl/` directory in the current project.
    Init,

    /// Add a component or connection.
    #[command(subcommand)]
    Add(AddCommand),

    /// Rename a component (updates all references atomically).
    #[command(subcommand)]
    Rename(RenameCommand),

    /// Remove a component or decision.
    #[command(subcommand)]
    Remove(RemoveCommand),

    /// Start a Socratic design conversation for a component.
    Design {
        /// Component to design (must already exist via `trurl add component`).
        component: String,

        /// Resume a previously interrupted design session.
        #[arg(long = "continue")]
        continue_session: bool,

        /// Revisit and potentially revise existing decisions.
        #[arg(long)]
        revisit: bool,
    },

    /// Record a quick decision without the full Socratic flow.
    Decide {
        /// Component this decision belongs to.
        component: String,

        /// What was decided.
        #[arg(long)]
        choice: String,

        /// Why — your reasoning.
        #[arg(long)]
        reason: String,

        /// Decision this supersedes (filename without `.toml`).
        #[arg(long = "supersede")]
        supersedes: Option<String>,
    },

    /// Start the MCP server for AI coding agent integration.
    Serve,

    /// Open the interactive architecture map in your browser.
    Map,

    /// Show project status: component count, decision count, issues.
    Status,

    /// Validate `.trurl/` internal consistency.
    Check,
}

#[derive(Subcommand, Debug)]
pub enum AddCommand {
    /// Define a new component.
    Component {
        /// Kebab-case component name (e.g. `auth`, `rate-limiter`).
        name: String,
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

    /// Remove a decision.
    Decision {
        /// Decision filename (without `.toml`).
        name: String,
    },
}

/// Dispatch a parsed CLI invocation to the appropriate handler.
pub fn run(cli: Cli) -> Result<()> {
    let cwd = std::env::current_dir()?;
    match cli.command {
        Command::Init => commands::init(&cwd),
        Command::Add(sub) => match sub {
            AddCommand::Component { name } => commands::add_component(&cwd, &name),
            AddCommand::Connection { from, to } => commands::add_connection(&cwd, &from, &to),
        },
        Command::Rename(sub) => match sub {
            RenameCommand::Component { .. } => not_implemented("rename component"),
        },
        Command::Remove(sub) => match sub {
            RemoveCommand::Component { .. } => not_implemented("remove component"),
            RemoveCommand::Decision { .. } => not_implemented("remove decision"),
        },
        Command::Design { .. } => not_implemented("design"),
        Command::Decide { .. } => not_implemented("decide"),
        Command::Serve => not_implemented("serve"),
        Command::Map => not_implemented("map"),
        Command::Status => commands::status(&cwd),
        Command::Check => commands::check(&cwd),
    }
}

fn not_implemented(command: &str) -> Result<()> {
    Err(Error::NotImplemented(format!(
        "`trurl {command}` is not yet implemented"
    )))
}
