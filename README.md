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
  <a href="SECURITY.md">Report a vulnerability</a> · <a href="CONTRIBUTING.md">Contributing</a> · <a href="CHANGELOG.md">Changelog</a>
</p>

---

AI coding agents ship fast, but each one makes its own architectural choices — silently, differently, every time. The diffs look fine in isolation. Six months later you have a codebase that works but has no coherent design, and no one can explain why anything is the way it is. The system works. **You just don't own it.**

Trurlic makes you the architect again. Architectural decisions live in a typed graph inside your repo, and **every AI agent receives them as hard constraints before generating a single line** — served over the MCP. One decision graph, every agent follows it, nothing slips through.

## MCP-first

Trurlic is an **MCP server**. Your coding agent — Claude Code, Cursor, Copilot, Codex, and others — talks to it directly: it pulls architectural context before implementing, runs design conversations when new territory appears, and records decisions as it goes. The graph is the agent's source of truth.

The `trurlic` CLI exists for the human: initialize the graph, sketch components, inspect and maintain it, open the visual map. **The CLI never calls an LLM.** All reasoning runs on your coding agent's own model, through MCP.

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
follow them unless the user explicitly revises them via design.
Never silently deviate. WHEN UNCERTAIN: STOP and ask.
```

Every decision came from a human decision or a review-flagged agent proposal. The agent generates code; the graph constrains it.

## Two modes

Trurlic runs the same workflow in two modes. The agent picks one when it calls `advance` — or asks you when it's unsure.

|  | **Agent mode** | **Interactive mode** |
|--|----------------|----------------------|
| **Who decides** | The AI reads the source and decides autonomously | You and the AI reason it out together |
| **Speed** | Fast, no interruptions | Slower, deliberate |
| **Comprehension gates** | Off | On — the AI must confirm shared understanding |
| **Attribution** | Recorded as `agent`, flagged ⚠ for your review | Recorded as `user` |
| **Use it for** | "implement X", "fix Y", "add feature Z", bootstrapping an existing codebase | "design", "architect", "let's think about…" |

Agent-mode decisions are never silently trusted — they land marked `agent · unreviewed` and surface in every context brief until you promote or revise them. Interactive mode is the Socratic path: the AI asks, you think, and the shared understanding becomes a decision. Some task types are pinned — bootstrapping an existing codebase is always autonomous (agent); a learning walkthrough is always interactive.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/trurlic-labs/trurlic/master/install.sh | bash
```

Or with Rust: `cargo install trurlic` (requires Rust 1.88+).

Pre-built binaries for Linux, macOS, and Windows are on the [Releases](https://github.com/trurlic-labs/trurlic/releases) page.

## Connect your IDE

`trurlic install` writes the MCP server configuration for your coding agent:

```bash
trurlic install --ide claude-code    # Claude Code
trurlic install --ide cursor         # Cursor
trurlic install --ide windsurf       # Windsurf
trurlic install --ide cline          # Cline
trurlic install --ide copilot        # GitHub Copilot
trurlic install --ide codex          # Codex CLI (OpenAI)
trurlic install --ide claude         # Claude Desktop
trurlic install --ide open-code      # OpenCode
trurlic install --ide open-claw      # OpenClaw
trurlic install --ide hermes-agent   # Hermes Agent
trurlic install --ide antigravity    # Antigravity CLI (Google)
```

Prefer manual configuration? Add this to your MCP config:

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

Then tell your agent how to use it — add to `CLAUDE.md`, `.cursorrules`, or equivalent:

```markdown
## Trurlic

This project uses Trurlic for architectural decisions.

Before implementing any task:
1. Call `advance` with the component name and a mode (agent | interactive).
2. Follow the returned action exactly.
3. Call `advance` again after completing each action.
4. Repeat until `ready: true`, then implement — constrained by every
   decision in the context brief.
```

See [CLAUDE.md](CLAUDE.md) for the full recommended instructions.

## Quick start

Two commands set it up; everything else is a conversation with your agent.

```bash
trurlic init                       # create the graph in your project root
trurlic install --ide claude-code  # wire up your coding agent over MCP
```

**Have an existing codebase?** Ask your agent to map it:

> Bootstrap the architecture with Trurlic.

It reads your source, then records components, decisions, and patterns autonomously — each flagged `agent · unreviewed` for you to promote or revise later.

**Designing something new?** Talk it through:

> Let's design the rate limiter. Walk me through it with Trurlic.

The agent runs the Socratic flow in interactive mode — asking, reasoning, and recording each decision as you agree on it — then implements against the constraints it just captured.

**Implementing within what already exists?** Just say what you want:

> Add per-key rate limiting to the auth service.

Before writing a line, the agent pulls the component's brief, generates code that respects every decision, and verifies compliance before it commits.

```bash
trurlic map      # review the graph anytime — interactive, in your browser
```

## How the agent works with the graph

Every task starts at `advance`, the orchestration hub. It reads the graph, computes the next step, and returns a concrete action — define scope, cover a security concern, verify a constraint, record a decision. The agent acts, calls `advance` again, and repeats until `ready: true`. Then it pulls the full brief and implements.

```
advance(component, task_type, mode)   →  next action
  … agent acts, advance again …       →  ready: true
get_context(component)                →  decisions + rules + related constraints
  … agent implements within the brief …
verify_against_decisions(component, changed_files)
                                      →  per-decision verdicts; fix violations before commit
```

`advance` is a **pure function** of the graph — same graph in, same step out, no clock, no I/O, no LLM. Seven task types (new component, feature, fix, learn, review, harden, bootstrap) each drive a distinct step sequence with preconditions and postconditions, so the workflow can't skip a security concern or a constraint check.

## MCP tools

| Tool | Purpose |
|------|---------|
| `advance` | Compute the next workflow step and return the next action — the orchestration hub |
| `get_context` | Architectural brief for a component: decisions, rules, related constraints, health |
| `get_architecture` | Full system overview: components, connections, patterns |
| `check_pattern` | Check whether an approach is already covered by existing decisions |
| `get_step_prompt` | Structured prompt for a specific workflow step (mode-aware) |
| `get_decisions_for_file` | Find every decision whose code references touch a file or directory |
| `get_decision_history` | A decision's current state plus its full chronological revision history |
| `verify_against_decisions` | After writing code, return the decisions that apply to the changed files so the agent can check compliance before committing |
| `add_component` | Add a component to the graph |
| `add_connection` | Connect two components |
| `record_decision` | Record a decision with edges, tags, rejected alternatives, and code references |
| `record_pattern` | Synthesize a pattern from multiple related decisions |
| `update_decision` | Revise a decision in place (with history) or promote an agent decision to reviewed |
| `remove_decision` | Remove a decision with cascade and coverage-impact analysis |

All tools carry [annotations](https://modelcontextprotocol.io/specification/2025-11-25/server/tools#annotations) (`readOnlyHint`, `destructiveHint`, `openWorldHint`) so MCP clients can reason about each invocation.

## Development

```bash
make setup        # install frontend deps + git hooks
make ci           # fmt + clippy + test + audit (run before pushing)
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for the full development guide.

## License

Apache-2.0
