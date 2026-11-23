<p align="center">
  <img src="./banner.png" alt="Trurlic" height="100">
</p>

<p align="center">
  <b>The architecture layer for AI-assisted codebases.</b><br>
  <sub>Named after <a href="https://en.wikipedia.org/wiki/Trurl">Trurl</a> from Stanisław Lem's <b><i>The Cyberiad</i></b>, the constructor who thinks deeply about what he builds before building it.</sub>
</p>

<p align="center">
  <a href="https://github.com/trurlic-labs/trurlic/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/trurlic-labs/trurlic/ci.yml?style=flat-square&label=CI" alt="CI"></a>
  <img src="https://img.shields.io/badge/license-Apache--2.0-blue?style=flat-square" alt="License">
  <img src="https://img.shields.io/badge/rust-1.90%2B-orange?style=flat-square" alt="Rust">
  <br>
  <a href="https://trurlic-docs.pages.dev/"><b>Documentation</b></a> · <a href="SECURITY.md">Report a vulnerability</a> · <a href="CONTRIBUTING.md">Contributing</a> · <a href="CHANGELOG.md">Changelog</a>
</p>

---

AI coding agents ship fast, but each one makes its own architectural choices — silently, differently, every time. The diffs look fine in isolation. Six months later you have a codebase that works but has no coherent design, and no one can explain why anything is the way it is. The system works. **You just don't own it.**

Trurlic makes you the architect again. Architectural decisions live in a typed graph inside your repo, and **every AI agent receives them as hard constraints before generating a single line** — served over the MCP. One decision graph, every agent follows it, nothing slips through.

## MCP-first

Trurlic is an **MCP server**. Your coding agent — Claude Code or any other MCP client — talks to it directly: it pulls architectural context before implementing, records the decisions it makes or you agree on, and checks its changes against them before committing. The graph is the agent's source of truth.

The `trurlic` CLI exists for the human: initialize the graph, sketch components, inspect and maintain it. **The CLI never calls an LLM.** All reasoning runs on your coding agent's own model, through MCP.

```
        ┌──────────────┐                      ┌──────────────────┐
  You   │     CLI      │                      │  AI coding agent │
 ──────▶│ init·inspect │                      │  context·design  │
        │   maintain   │                      │  record·verify   │
        └──────┬───────┘                      └────────┬─────────┘
               │ write                           MCP   │ read + write
               ▼                                        ▼
        ┌──────────────────────────────────────────────────────┐
        │   .trurlic/  —  decision graph, git-tracked TOML     │
        └──────────────────────────────────────────────────────┘
```

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
follow them unless the user explicitly revises them.
Never silently deviate. WHEN UNCERTAIN: STOP and ask.
```

Every decision came from a human decision or a review-flagged agent proposal. The agent generates code; the graph constrains it.

## Who decided

Every decision records who made it.

|  | **`user`** | **`agent`** |
|--|------------|-------------|
| **Recorded when** | You made or confirmed the choice in the conversation | The agent chose on its own while implementing |
| **In the brief** | A plain constraint | Marked `agent — unconfirmed`, with how to confirm or revise it |
| **Leaves the flag** | — | When you confirm it and the agent promotes it (`update_decision`, mode `promote`) |

Agent decisions are never silently trusted: they stay flagged in every context brief until you confirm them, and `trurlic gc --aggressive` reclaims the ones left unreviewed for over 90 days.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/trurlic-labs/trurlic/master/install.sh | bash
```

Or with Rust: `cargo install trurlic` (requires Rust 1.90+).

Pre-built binaries for Linux, macOS, and Windows are on the [Releases](https://github.com/trurlic-labs/trurlic/releases) page.

## Connect your agent

`trurlic install` registers the MCP server with Claude Code for your user, through `claude mcp add` (`--dry-run` prints that command instead of running it):

```bash
trurlic install --ide claude-code
```

Any other MCP client: add this to its MCP config:

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

Then tell your agent how to use it — add to `CLAUDE.md` or your agent's equivalent:

```markdown
## Trurlic

This project uses Trurlic for architectural decisions.

Before implementing any task:
1. Call `get_context` for the component and read every decision in the
   brief: they constrain the code you write.
2. Implement within the brief. For a choice no decision covers, call
   `check_pattern`; if it is uncovered, record it with `record_decision`
   (attribution `agent` when you chose alone, `user` when the user decided).
3. Before committing, call `verify_against_decisions` with the files you
   changed, and fix every decision it reports VIOLATED.
```

See [CLAUDE.md](CLAUDE.md) for the full recommended instructions.

## Quick start

Two commands set it up; everything else is a conversation with your agent.

```bash
trurlic init                       # create the graph in your project root
trurlic install --ide claude-code  # wire up your coding agent over MCP
```

**Have an existing codebase?** Ask your agent to map it:

> Record this codebase's components and architectural decisions in Trurlic.

It reads your source and records components and decisions through the MCP tools, each attributed `agent` and flagged for you to promote or revise later.

**Designing something new?** Talk it through:

> Let's design the rate limiter, and record each decision we agree on in Trurlic.

The agent reasons it out with you, records each decision you agree on as `user`, then implements against the constraints it just captured.

**Implementing within what already exists?** Just say what you want:

> Add per-key rate limiting to the auth service.

Before writing a line, the agent pulls the component's brief, generates code that respects every decision, and verifies compliance before it commits.

```bash
trurlic status   # the graph at a glance: components, decisions, patterns, issues
```

## Documentation

Full documentation lives at **[trurlic-docs.pages.dev](https://trurlic-docs.pages.dev/)**:

- **[Start here](https://trurlic-docs.pages.dev/)** — overview, setup, and the core concepts
- **[Architecture](https://trurlic-docs.pages.dev/)** — system design and the integrity model
- **[Guides](https://trurlic-docs.pages.dev/)** — the MCP server, configuration, and design workflows
- **[Reference](https://trurlic-docs.pages.dev/)** — full CLI reference, MCP tools, and troubleshooting
- **[Integrations](https://trurlic-docs.pages.dev/)** — connecting Claude Code and other MCP clients

## How the agent works with the graph

Every task starts from the component's brief. The agent reads it, implements within it, records any choice the graph does not cover yet, and checks its changes against the decisions before committing.

```
get_context(component)                →  decisions + rules + related constraints
  … agent implements within the brief …
check_pattern(description)            →  covered by a decision, or not
record_decision(component, choice, reason, attribution)
                                      →  a choice the graph did not cover yet
verify_against_decisions(component, changed_files)
                                      →  the decisions to check; fix violations before commit
```

## MCP tools

| Tool | Purpose |
|------|---------|
| `get_context` | Architectural brief for a component: decisions, rules, related constraints, health |
| `get_architecture` | Full system overview: components, connections, patterns |
| `check_pattern` | Check whether an approach is already covered by existing decisions |
| `get_decisions_for_file` | Find every decision whose code references touch a file or directory |
| `get_decision_history` | A decision's current state plus its full chronological revision history |
| `verify_against_decisions` | After writing code, return the decisions that apply to the changed files so the agent can check compliance before committing |
| `add_component` | Add a component to the graph |
| `add_connection` | Connect two components |
| `record_decision` | Record a decision with edges, tags, rejected alternatives, and code references |
| `record_pattern` | Synthesize a pattern from multiple related decisions |
| `update_decision` | Revise a decision in place (with history) or promote an agent decision to reviewed |
| `remove_decision` | Remove a decision with cascade analysis: refused while another decision depends on it |
| `remove_pattern` | Remove a pattern and its edges; its member decisions are kept |

Every result fits 24 KiB, which Claude Code shows inline. A larger answer keeps its shape: its longest lists and strings lose their tails, and a `truncated` array names each cut with what it omitted.

All tools carry [annotations](https://modelcontextprotocol.io/specification/2025-11-25/server/tools#annotations) (`readOnlyHint`, `destructiveHint`, `openWorldHint`) so MCP clients can reason about each invocation.

## Development

```bash
make setup        # install the git hooks
make ci           # fmt + clippy + test + audit (run before pushing)
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for the full development guide.

## License

Apache-2.0
