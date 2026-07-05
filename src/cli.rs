use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::session::SessionMode;
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
        #[arg(long = "continue", conflicts_with = "revisit")]
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

        /// Alternative considered and rejected (repeatable).
        #[arg(long = "alternative", short = 'a')]
        alternatives: Vec<String>,
    },

    /// Reclaim decisions that have lost their anchor: orphaned (component
    /// gone), stale (all referenced files deleted), or long-unreviewed agent
    /// decisions. Reports what would be reclaimed by default; pass `--apply`
    /// to write the removals.
    Gc {
        /// Actually write the removals (default: dry-run).
        #[arg(long)]
        apply: bool,

        /// Also remove stale and long-unreviewed agent decisions, not just
        /// orphans.
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

    /// Query the decision graph.
    #[command(subcommand)]
    Query(QueryCommand),

    /// Show project status: component count, decision count, issues.
    Status,

    /// Validate `.trurlic/` internal consistency.
    Check {
        /// Force-rebuild graph.toml from node files (nuclear recovery).
        /// Non-inferable edges (ConnectsTo, DependsOn, etc.) will be lost.
        #[arg(long)]
        rebuild: bool,
    },

    /// Write MCP server configuration for an IDE.
    Install {
        /// Target IDE.
        #[arg(long, value_enum)]
        ide: InstallIde,

        /// Path to the trurlic binary to embed in the config.
        /// Defaults to the currently running executable.
        #[arg(long)]
        binary_path: Option<PathBuf>,

        /// Print the config snippet to stdout without writing to disk.
        #[arg(long)]
        dry_run: bool,
    },

    /// Migrate `.trurlic/` to the current format version.
    Migrate {
        /// Show what would change without writing.
        #[arg(long)]
        dry_run: bool,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum InstallIde {
    /// Claude Desktop (Anthropic)
    Claude,
    /// Claude Code (Anthropic) — uses `claude mcp add`
    ClaudeCode,
    /// Cursor IDE
    Cursor,
    /// Cline (VS Code extension)
    Cline,
    /// Windsurf (Codeium)
    Windsurf,
    /// GitHub Copilot (VS Code agent mode)
    Copilot,
    /// Codex CLI (OpenAI)
    Codex,
    /// OpenCode
    OpenCode,
    /// OpenClaw (via MCPorter)
    OpenClaw,
    /// Hermes Agent (NousResearch)
    HermesAgent,
    /// Antigravity CLI (Google)
    Antigravity,
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
        },
        Command::Design {
            component,
            continue_session,
            revisit,
            task,
            provider,
            model,
        } => {
            let mode = if continue_session {
                SessionMode::Continue
            } else if revisit {
                SessionMode::Revisit
            } else {
                SessionMode::Fresh
            };
            commands::design(
                &cwd,
                &component,
                mode,
                task.as_deref(),
                provider.as_deref(),
                model.as_deref(),
            )
        }
        Command::Decide {
            component,
            choice,
            reason,
            alternatives,
        } => commands::decide(&cwd, &component, &choice, &reason, &alternatives),
        Command::Install {
            ide,
            binary_path,
            dry_run,
        } => {
            let mode = if dry_run {
                commands::DryRun::Yes
            } else {
                commands::DryRun::No
            };
            commands::install(ide, binary_path.as_deref(), mode)
        }
        Command::Gc {
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
                use std::io::{IsTerminal, Write};
                match commands::resolve_aggressive_confirm(yes, std::io::stdout().is_terminal())? {
                    commands::AggressiveConfirm::Confirmed => {}
                    commands::AggressiveConfirm::PromptUser => {
                        eprint!(
                            "Aggressive gc will permanently remove stale and \
                             unreviewed agent decisions. Continue? [y/N] "
                        );
                        std::io::stderr().flush()?;
                        let mut buf = String::new();
                        std::io::stdin().read_line(&mut buf)?;
                        if !buf.trim().eq_ignore_ascii_case("y") {
                            println!("Aborted.");
                            return Ok(());
                        }
                    }
                }
            }
            commands::gc(&cwd, scope, execution)
        }
        Command::Query(sub) => match sub {
            QueryCommand::File { path } => commands::query_file(&cwd, &path),
        },
        Command::Serve => commands::serve(&cwd),
        Command::Map {
            port,
            no_open,
            detach,
        } => commands::map(&cwd, port, no_open, detach),
        Command::Status => commands::status(&cwd),
        Command::Check { rebuild } => commands::check(&cwd, rebuild),
        Command::Migrate { dry_run } => {
            let mode = if dry_run {
                commands::DryRun::Yes
            } else {
                commands::DryRun::No
            };
            commands::migrate(&cwd, mode)
        }
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
