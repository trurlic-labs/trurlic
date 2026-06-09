use clap::{Parser, Subcommand};

use crate::{Result, commands};

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
    /// Initialize a new `.trurlic/` directory in the current project.
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
        /// Component to design (must already exist via `trurlic add component`).
        component: String,

        /// Resume a previously interrupted design session.
        #[arg(long = "continue")]
        continue_session: bool,

        /// Revisit and potentially revise existing decisions.
        #[arg(long)]
        revisit: bool,

        /// Task description (e.g. "add caching", "fix auth race condition").
        /// Focuses the workflow on concerns relevant to the task.
        #[arg(long, short = 't')]
        task: Option<String>,

        /// LLM provider: anthropic, openai, openrouter (auto-detected if omitted).
        #[arg(long, short = 'p')]
        provider: Option<String>,

        /// Model override (default per provider: claude-sonnet-4, gpt-4o, etc.).
        #[arg(long, short = 'm')]
        model: Option<String>,
    },

    /// Record a quick decision without the full Socratic flow.
    Decide {
        /// Component this decision belongs to (or "project" for project-wide).
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

        /// Alternative considered and rejected (repeatable).
        #[arg(long = "alternative", short = 'a')]
        alternatives: Vec<String>,
    },

    /// Start the MCP server for AI coding agent integration.
    Serve,

    /// Open the interactive map in the browser.
    Map {
        /// Bind to a specific port (default: OS-assigned).
        #[arg(long)]
        port: Option<u16>,

        /// Start the server without opening the browser.
        #[arg(long)]
        no_open: bool,

        /// Background the server; print the URL and exit.
        #[arg(long)]
        detach: bool,
    },

    /// Show project status: component count, decision count, issues.
    Status,

    /// Validate `.trurlic/` internal consistency.
    Check {
        /// Force-rebuild graph.toml from node files (nuclear recovery).
        /// Non-inferable edges (ConnectsTo, DependsOn, etc.) will be lost.
        #[arg(long)]
        rebuild: bool,
    },

    /// Show bootstrap progress and agent instructions for autonomous
    /// architecture extraction from an existing codebase.
    /// With -p/--provider, runs the bootstrap directly using the LLM API.
    Bootstrap {
        /// Bootstrap a single component instead of the full project.
        component: Option<String>,

        /// LLM provider for direct mode: anthropic, openai, openrouter.
        /// When set, runs the bootstrap using the LLM instead of printing
        /// agent instructions.
        #[arg(long, short = 'p')]
        provider: Option<String>,

        /// Model override (default per provider: claude-sonnet-4, gpt-4o, etc.).
        #[arg(long, short = 'm')]
        model: Option<String>,
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

    /// Remove a decision.
    Decision {
        /// Decision filename (without `.toml`).
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

pub fn run(cli: Cli) -> Result<()> {
    let cwd = std::env::current_dir()?;
    match cli.command {
        Command::Init => commands::init(&cwd),
        Command::Add(sub) => match sub {
            AddCommand::Component { name, description } => {
                commands::add_component(&cwd, &name, description.as_deref())
            }
            AddCommand::Connection { from, to } => commands::add_connection(&cwd, &from, &to),
        },
        Command::Rename(sub) => match sub {
            RenameCommand::Component { old, new } => commands::rename_component(&cwd, &old, &new),
        },
        Command::Remove(sub) => match sub {
            RemoveCommand::Component { name } => commands::remove_component(&cwd, &name),
            RemoveCommand::Decision { name } => commands::remove_decision(&cwd, &name),
            RemoveCommand::Connection { from, to } => commands::remove_connection(&cwd, &from, &to),
        },
        Command::Design {
            component,
            continue_session,
            revisit,
            task,
            provider,
            model,
        } => commands::design(
            &cwd,
            &component,
            continue_session,
            revisit,
            task.as_deref(),
            provider.as_deref(),
            model.as_deref(),
        ),
        Command::Decide {
            component,
            choice,
            reason,
            supersedes,
            alternatives,
        } => commands::decide(
            &cwd,
            &component,
            &choice,
            &reason,
            supersedes.as_deref(),
            &alternatives,
        ),
        Command::Serve => commands::serve(&cwd),
        Command::Map {
            port,
            no_open,
            detach,
        } => commands::map(&cwd, port, no_open, detach),
        Command::Status => commands::status(&cwd),
        Command::Check { rebuild } => commands::check(&cwd, rebuild),
        Command::Bootstrap {
            component,
            provider,
            model,
        } => {
            if provider.is_some() {
                commands::bootstrap_direct(
                    &cwd,
                    component.as_deref(),
                    provider.as_deref(),
                    model.as_deref(),
                )
            } else {
                match component {
                    Some(ref c) => commands::bootstrap_component(&cwd, c),
                    None => commands::bootstrap(&cwd),
                }
            }
        }
    }
}
