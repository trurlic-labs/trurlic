# Trurl

Structured architectural decisions that constrain AI code generation.

![License](https://img.shields.io/badge/license-Apache--2.0-blue?style=flat-square)
![Rust](https://img.shields.io/badge/rust-1.95-orange?style=flat-square)

[Report a vulnerability](SECURITY.md) · [Contributing](CONTRIBUTING.md)

---

## The Problem

AI engineering is real leverage - with a real cost.

You're shipping faster than ever, but you've stopped making the decisions that define your architecture - the AI makes them for you, silently, differently each time, and you approve because each diff looks reasonable in isolation. The speed is real. So is the cost: you're losing ownership of your own codebase, trading deep understanding for throughput, and accumulating technical debt that no linter will ever catch.

Six months in, you can't refactor without breaking things you didn't know existed. You can't onboard anyone because there's no design to explain — just a pile of locally-correct code with no global coherence. The system works. **You just don't own it**.

Trurl fixes this. You make the architectural decisions. Trurl records them, forces you to understand them, and feeds them as hard constraints to every AI-generated line of code. One decision graph, every agent follows it, nothing slips through.

## What Trurl Does

Every architectural decision is captured in a queryable graph, understood by the programmer through forced engagement, and used to constrain AI code generation.

**A file format.** `.trurl/` is like `.git/`. It lives in your repo, git-tracked. Contains a typed knowledge graph: components, decisions, patterns, and their relationships. TOML node files for human readability, a compiled edge index for fast traversal.

**An MCP server.** `trurl serve` starts a local MCP server that any AI coding tool queries. The coding agent calls Trurl to get context before implementing, to run design conversations when new patterns are needed, and to record decisions as they're made. You never leave your coding tool.

**A CLI.** `trurl design <component>` runs a Socratic design conversation — the AI asks you questions, you think through tradeoffs, and your answers become recorded decisions. `trurl decide` records quick decisions from the terminal. Everything local, everything under 100ms.

Named after Trurl from Stanisław Lem's *The Cyberiad* — the constructor who thinks deeply about what he builds before building it.

## Install

```
cargo install trurl
```

Requires Rust 1.88+. Toolchain pinned to 1.95 in the repo.

## Quick Start

```bash
trurl init
trurl add component auth --description "Authentication and token management"
trurl add component database --description "Persistence layer"
trurl add connection auth database

# Record decisions directly
trurl decide project --choice "Result<T, AppError> for all errors" --reason "Consistent error propagation"
trurl decide auth --choice "JWT with DPoP binding" --reason "Stateless, no session store"

# Or run a guided design conversation
trurl design auth

# Start the MCP server
trurl serve
```

## MCP Integration

`trurl serve` exposes these tools over MCP (stdio transport):

| Tool | Purpose |
|------|---------|
| `get_context` | Tailored brief for a component: decisions, project rules, related constraints |
| `check_pattern` | Check if an approach is covered by existing decisions |
| `get_architecture` | Full system overview: components, connections, patterns |
| `get_design_prompt` | Structured prompt for design conversations (full/quick/learn/review) |
| `record_decision` | Record a decision with edges, tags, alternatives |
| `record_pattern` | Record a pattern synthesized from multiple decisions |
| `update_decision` | Amend (typo fix) or supersede (substantive change) |
| `remove_decision` | Remove with cascade awareness |
| `validate_consistency` | Full graph integrity check |

### Agent Workflow

Add to your `CLAUDE.md` (or equivalent):

```markdown
## Trurl

This project uses Trurl for architectural decisions.

### Workflow

1. Before implementing any task, call `check_pattern` with a description of what you're about to do.
2. If `covered`: call `get_context` and follow the brief as authoritative constraints.
3. If `not_covered`: call `get_design_prompt` and run the design conversation with the user before implementing.
4. During design conversations, call `record_decision` for each choice the user makes.
5. After design, call `get_context` for the implementation brief.
6. Never introduce a new pattern, dependency, or architectural approach without checking Trurl first.

### Comprehension Gates

When Trurl's design prompt includes comprehension checkpoints, you MUST ask the user to articulate their understanding before proceeding. Do not skip these.
```

### What the Agent Sees

When the agent calls `get_context("rate-limiter")`:

```
RULES:
- ALL error handling MUST use Result<T, AppError>
- ALL persistent state MUST use Redis

COMPONENT: rate-limiter
- Per API key, consistent with auth boundary
- Redis sliding window, 60s buckets
- 429 + retry-after header, RFC 6585 compliant

PATTERNS:
- state-in-redis: shared pool via app state, no per-component connections

RELATED:
- auth: JWT with DPoP, rate limiter runs AFTER auth

WHEN UNCERTAIN:
STOP. This introduces a new pattern. Ask the user to design it first.
```

## Design Modes

`get_design_prompt` supports four modes:

| Mode | When | Depth |
|------|------|-------|
| `full` | New component or major feature | Multi-phase: scope → technical choices → pattern recognition → summary checkpoint. Comprehension gates after each decision. |
| `quick` | Small addition to existing component | 1–3 targeted questions. Single gate only if a new decision was recorded. |
| `learn` | Studying existing design | All decisions with challenge questions. No implementation. |
| `review` | Periodic health check | Decisions sorted oldest-first. "Does this still hold?" |

## The `.trurl/` Directory

```
.trurl/
├── project.toml                  # project metadata, format version
├── graph.toml                    # edge index + node hashes (git-tracked)
├── components/
│   ├── auth.toml
│   └── rate-limiter.toml
├── decisions/
│   ├── error-strategy.toml
│   └── rate-limit-storage.toml
├── patterns/
│   └── state-in-redis.toml
└── .state/                       # machine-local, gitignored
    ├── lock
    ├── tmp/
    └── sessions/
```

Node files are TOML. Edges live in `graph.toml`. Files are truth, index is derived — `trurl check --rebuild` restores the index from node files.

### Graph Edges

| Edge | From → To | Meaning |
|------|-----------|---------|
| `belongs_to` | decision → component | Decision is about this component |
| `connects_to` | component → component | Data/control flow |
| `depends_on` | decision → decision | This decision assumes the target holds |
| `constrains` | decision → decision | Restricts options for the target |
| `supersedes` | decision → decision | Replaces the target (history preserved) |
| `member_of` | pattern → decision | Decision is part of this pattern |
| `applies_to` | pattern → component | Pattern applies to this component |

## CLI Reference

```
trurl init                                  Create .trurl/ in current directory
trurl add component <name> [-d <desc>]      Define a component
trurl add connection <from> <to>            Connect two components
trurl rename component <old> <new>          Rename, updating all references
trurl remove component <name>               Remove (refuses if decisions reference it)
trurl remove decision <name>                Remove (refuses if depended on)
trurl decide <component> --choice "..." --reason "..."
      [--supersede <name>] [-a "..."]       Quick decision recording
trurl design <component>                    Socratic design conversation
      [--continue] [--revisit]
      [-p anthropic|openai|openrouter]
      [-m <model>]
trurl serve                                 Start MCP server (stdio)
trurl status                                Component/decision/pattern counts
trurl check                                 Validate graph integrity
trurl check --rebuild                       Force-rebuild graph.toml from files
```

## API Key Configuration

Environment variables (checked first):
```
ANTHROPIC_API_KEY
OPENAI_API_KEY
OPENROUTER_API_KEY
```

Fallback: `~/.config/trurl/config.toml` (must be `chmod 600`):
```toml
default_provider = "anthropic"
anthropic_api_key = "sk-ant-..."
```

Keys are zeroed from memory on drop and never appear in logs or error output.

## Design Principles

**Fail-closed on writes.** Every write validates the full graph in memory before touching disk. Dangling reference, cycle, schema violation → refuse with a clear error.

**Atomic writes.** Temp file, verify round-trip, rename. `graph.toml` renamed last as the commit point. Interrupted writes cleaned up on next startup.

**Offline-first.** Only `trurl design` calls an LLM API. MCP, CLI, check, status — all local. The MCP server provides prompts; the coding agent's own LLM runs the conversation.

**Files are truth.** `graph.toml` is derived and rebuildable. Hand-edit a TOML file, run `trurl check`, the graph reconciles.

**Live reload.** The MCP server watches `.trurl/` for external changes and reloads automatically.

## Development

```bash
make setup        # git hooks
make ci           # fmt + clippy + test + audit (run before pushing)
make audit        # cargo deny check (advisories, licenses, bans)
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for the full development guide.

## License

Apache-2.0
