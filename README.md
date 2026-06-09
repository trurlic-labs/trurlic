<p align="center">
  <img src="./banner.png" alt="Trurlic" height="100">
</p>

<p align="center">
  <b>The architecture layer for AI-assisted codebases.</b><br>
  <sub>Named after <a href="https://en.wikipedia.org/wiki/Trurl">Trurl</a> from Stanisław Lem's <b><i>The Cyberiad</i></b>, the constructor who thinks deeply about what he builds before building it.</sub>
</p>

<p align="center">
  <a href="https://github.com/trurlic-labs/trurlic/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/trurlic-labs/trurlic/ci.yml?style=flat-square&label=CI" alt="CI"></a>
  <a href="https://codspeed.io/trurlic-labs/trurlic"><img src="https://img.shields.io/endpoint?url=https://codspeed.io/badge.json&style=flat-square" alt="CodSpeed"></a>
  <img src="https://img.shields.io/badge/license-Apache--2.0-blue?style=flat-square" alt="License">
  <img src="https://img.shields.io/badge/rust-1.88%2B-orange?style=flat-square" alt="Rust">
  <br>
  <a href="SECURITY.md">Report a vulnerability</a> · <a href="CONTRIBUTING.md">Contributing</a>
</p>

---

AI coding agents ship fast, but each one makes its own architectural choices, silently, differently every time. The diffs look fine in isolation. Six months later you have a codebase that works but has no coherent design, and no one can explain why anything is the way it is. The system works. **You just don't own it**.

Trurlic makes you the architect again. You record decisions in a typed graph, reason through them via Socratic design conversations, and every AI agent gets them as hard constraints before generating a single line. One decision graph, every agent follows it, nothing slips through.

## What the agent sees

When an AI coding agent calls `get_context("rate-limiter")`, Trurlic returns:

```
RULES (inviolable — every generated line must respect these):
- ALL error handling MUST use Result<T, AppError>
- ALL persistent state MUST use Redis

COMPONENT: rate-limiter
- Per API key, consistent with auth boundary
- Redis sliding window, 60s buckets
- 429 + retry-after header, RFC 6585 compliant

PATTERNS:
- state-in-redis: shared pool via app state, no per-component connections

RELATED (from connected components):
- auth: JWT with DPoP, rate limiter runs AFTER auth

OVERRIDE POLICY:
RULES are inviolable. Component decisions are strong defaults —
follow them unless the user explicitly revises them via design.
Never silently deviate. WHEN UNCERTAIN: STOP and ask.
```

Every decision came from a human. The agent generates code; the graph constrains it.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/trurlic-labs/trurlic/main/install.sh | bash
```

Or with Rust: `cargo install trurlic` (requires Rust 1.88+).

Pre-built binaries for Linux, macOS, and Windows are on the [Releases](https://github.com/trurlic-labs/trurlic/releases) page.

## Quick start

```bash
# Initialize in your project root
trurlic init

# Define your architecture
trurlic add component auth -d "Authentication and token management"
trurlic add component database -d "Persistence layer"
trurlic add connection auth database

# Record decisions
trurlic decide auth --choice "JWT with DPoP binding" \
  --reason "Stateless, no session store needed"

# Or run a Socratic design conversation — the AI asks, you think
trurlic design auth

# Or bootstrap from existing source code — the AI reads, records, done
trurlic bootstrap

# Start the MCP server for AI coding agents
trurlic serve
```

## How it works

**A decision graph.** `.trurlic/` lives in your repo, git-tracked. Components, decisions, patterns, and their relationships — stored as TOML node files with a compiled edge index. Human-readable, machine-queryable.

**An MCP server.** `trurlic serve` exposes the graph over [MCP](https://modelcontextprotocol.io/) (stdio transport). Coding agents query it for context before implementing, run design conversations when new patterns emerge, and record decisions as they go.

**A visual map.** `trurlic map` opens an interactive graph in the browser. Components, connections, decisions, patterns — explorable, editable, live-synced with the filesystem.

**A CLI.** `trurlic design <component>` runs a guided design conversation. `trurlic decide` records quick decisions. Everything local, everything under 100ms.

## Bootstrap

For existing codebases, `trurlic bootstrap` populates the decision graph autonomously. Your coding agent reads the source code and records components, decisions, and patterns — no interactive dialogue needed.

```bash
trurlic init
trurlic bootstrap     # shows status and agent instructions
trurlic serve         # start MCP server
# then in your coding agent:
#   advance(component="project", task_type="bootstrap")
#   follow each step until ready: true
trurlic map           # review the result
```

Bootstrap a single component: `trurlic bootstrap auth`.

## MCP setup

### Claude Code

```bash
claude mcp add trurlic -- trurlic serve
```

### Cursor / Windsurf / other MCP clients

Add to your MCP configuration:

```json
{
  "mcpServers": {
    "trurlic": {
      "command": "trurlic",
      "args": ["serve"]
    }
  }
}
```

### Agent instructions

Add to your `CLAUDE.md`, `.cursorrules`, or equivalent:

```markdown
## Trurlic

This project uses Trurlic for architectural decisions.

Before implementing any task:
1. Call `advance` with the component name.
2. Follow the returned action exactly.
3. Call `advance` again after completing each action.
4. Repeat until `ready: true`.
5. Implement, constrained by every decision in the context brief.
```

See [CLAUDE.md](CLAUDE.md) for the full recommended instructions.

## MCP tools

| Tool | Purpose |
|------|---------|
| `advance` | Compute workflow step, return next action — the orchestration hub |
| `get_context` | Architectural brief for a component: decisions, rules, related constraints |
| `check_pattern` | Check if an approach is already covered by existing decisions |
| `get_architecture` | Full system overview: components, connections, patterns |
| `get_design_prompt` | Structured prompt for design conversations |
| `add_component` | Add a component to the graph |
| `add_connection` | Connect two components |
| `record_decision` | Record a decision with edges, tags, and rejected alternatives |
| `record_pattern` | Synthesize a pattern from multiple related decisions |
| `update_decision` | Amend (typo fix) or supersede (substantive change) |
| `remove_decision` | Remove with cascade analysis |
| `validate_consistency` | Full graph integrity check |

## CLI reference

```
trurlic init                                  Create .trurlic/
trurlic add component <name> [-d <desc>]      Define a component
trurlic add connection <from> <to>            Connect components (directional)
trurlic rename component <old> <new>          Rename, updating all references
trurlic remove component <name>               Remove (blocked if decisions exist)
trurlic remove decision <name>                Remove (blocked if depended on)
trurlic remove connection <from> <to>         Remove a connection
trurlic decide <component>                    Record a quick decision
      --choice "..." --reason "..."
      [--supersede <name>] [-a "alt"]
trurlic design <component>                    Socratic design conversation
      [--continue] [--revisit] [-t <task>]
      [-p anthropic|openai|openrouter]
      [-m <model>]
trurlic bootstrap [<component>]               Bootstrap status and agent instructions
trurlic serve                                 Start MCP server (stdio)
trurlic map [--port N] [--no-open] [--detach] Interactive graph in browser
trurlic status                                Show counts and health
trurlic check [--rebuild]                     Validate (or rebuild) graph
```

## Configuration

API keys for `trurlic design` (environment variables, checked first):

```
ANTHROPIC_API_KEY
OPENAI_API_KEY
OPENROUTER_API_KEY
```

Fallback config file — `~/.config/trurlic/config.toml` (must be `chmod 600`):

```toml
default_provider = "anthropic"
anthropic_api_key = "sk-ant-..."
```

Keys are zeroed from memory on drop and never appear in logs or error messages.

## Design principles

**Fail-closed on writes.** Every mutation validates the full graph before touching disk. Dangling edges, cycles, schema violations — refused with a clear error, never silently committed.

**Atomic writes.** Serialize → write to temp → verify round-trip → rename. `graph.toml` renamed last as the commit point. Interrupted writes cleaned up on next startup.

**Offline-first.** Only `trurlic design` calls an LLM API. The MCP server, CLI, map — all local, all fast. The MCP server provides prompts and context; the coding agent's own LLM runs the conversation.

**Files are truth.** `graph.toml` is derived. Hand-edit a TOML file, run `trurlic check`, the graph reconciles. `--rebuild` reconstructs the index from node files.

**Live reload.** The MCP server and map watch `.trurlic/` for external changes (CLI, git checkout, manual edits) and reload automatically.

## Development

```bash
make setup        # install frontend deps + git hooks
make ci           # fmt + clippy + test + audit (run before pushing)
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for the full development guide.

## License

Apache-2.0
