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

Trurlic is an **MCP server**. Your coding agent — Claude Code or any other MCP client — talks to it directly: it pulls the architectural context of a component or a file before implementing. The graph is the agent's source of truth: one TOML file per decision and per component, which the agent and you read and edit directly.

The `trurlic` CLI exists for the human: initialize the graph, inspect it, check it and keep its files in canonical form. **The CLI never calls an LLM.** All reasoning runs on your coding agent's own model.

```
        ┌──────────────┐                      ┌──────────────────┐
  You   │     CLI      │                      │  AI coding agent │
 ──────▶│ init·inspect │                      │  context·design  │
        │  check·fmt   │                      │  implement       │
        └──────┬───────┘                      └────────┬─────────┘
               │ write                     MCP read,   │ file edits
               ▼                                        ▼
        ┌──────────────────────────────────────────────────────┐
        │   .trurlic/  —  decision graph, git-tracked TOML     │
        └──────────────────────────────────────────────────────┘
```

## What the agent sees

When an AI coding agent calls `get_context("rate-limiter")`, Trurlic returns:

```
RULES (inviolable: every generated line must respect these):
- ALL error handling MUST use Result<T, AppError>
- ALL persistent state MUST use Redis

COMPONENT: rate-limiter
- Per API key, consistent with auth boundary (...)
- Redis sliding window, 60s buckets (...)
- 429 + retry-after header, RFC 6585 compliant (...) (proposed)
  Code: src/limiter/window.rs::SlidingWindow

RELATED:
- auth: JWT with DPoP, rate limiter runs AFTER auth

OVERRIDE POLICY:
RULES are inviolable. Component decisions are strong defaults:
follow them unless the user explicitly revises them.
Never silently deviate from either.
```

Every decision came from a human decision or an agent proposal marked for review. The agent generates code; the graph constrains it.

## Who decided

Every decision states whether you accepted it.

|  | **`accepted`** | **`proposed`** |
|--|------------|-------------|
| **Recorded when** | You made or confirmed the choice | The agent chose on its own while implementing |
| **In the brief** | A plain constraint | Marked `(proposed)` until you accept it |

## The files

```toml
# .trurlic/decisions/sign-tokens.toml
component = "auth"
choice = "Sign tokens with Ed25519"
reason = "Verification needs no session store."
alternatives = [
    "HMAC with a shared secret: every verifier could forge tokens",
]
status = "accepted"
created = "2025-06-01T10:30:00Z"
anchors = [
    { file = "src/auth/token.rs", symbol = "sign" },
]
depends_on = [
    "keys-in-the-vault",
]
```

The file stem is the decision's name. A component file holds `description` and `connects_to`. Edit them by hand or through your agent: every load validates them, `trurlic check` reports what is wrong with the graph, and `trurlic fmt` puts each file in its canonical form (sorted sets, one element per line) so branches merge cleanly.

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
1. Call `get_context` for the component, or `get_decisions_for_file` for
   the files you will edit, and read every decision: they constrain the
   code you write.
2. Implement within them. For a choice no decision covers, search
   `.trurlic/decisions/`; if none covers it, write a decision file there
   (status `proposed` when you chose alone, `accepted` when the user decided).
3. Before committing, run `trurlic fmt` and `trurlic check`.
```

See [CLAUDE.md](CLAUDE.md) for the full recommended instructions.

## Quick start

Two commands set it up; everything else is a conversation with your agent.

```bash
trurlic init --placement tracked   # create the graph in your project root, versioned by its git
trurlic install --ide claude-code  # wire up your coding agent over MCP
```

**Designing something new?** Talk it through:

> Let's design the rate limiter, and write each decision we agree on to Trurlic.

The agent reasons it out with you, writes each decision you agree on as `accepted`, then implements against the constraints it just captured.

**Implementing within what already exists?** Just say what you want:

> Add per-key rate limiting to the auth service.

Before writing a line, the agent pulls the component's brief and generates code that respects every decision.

```bash
trurlic status   # the graph at a glance: components, decisions, edges, issues
trurlic check    # every issue, naming its file; fails when there is one
trurlic fmt      # rewrite the files that are not in canonical form
```

## Documentation

Full documentation lives at **[trurlic-docs.pages.dev](https://trurlic-docs.pages.dev/)**:

- **[Start here](https://trurlic-docs.pages.dev/)** — overview, setup, and the core concepts
- **[Architecture](https://trurlic-docs.pages.dev/)** — system design and the integrity model
- **[Guides](https://trurlic-docs.pages.dev/)** — the MCP server, configuration, and design workflows
- **[Reference](https://trurlic-docs.pages.dev/)** — full CLI reference, MCP tools, and troubleshooting
- **[Integrations](https://trurlic-docs.pages.dev/)** — connecting Claude Code and other MCP clients

## How the agent works with the graph

Every task starts from the component's brief. The agent reads it, implements within it, and writes down any choice the graph does not cover yet.

```
get_context(component)                →  decisions + rules + related constraints
get_decisions_for_file(path)          →  the decisions anchored on a file or directory
  … agent implements within the brief …
.trurlic/decisions/<name>.toml        →  a choice the graph did not cover yet
trurlic fmt && trurlic check          →  canonical files, no issue
```

## MCP tools

| Tool | Purpose |
|------|---------|
| `get_context` | Architectural brief for a component: decisions, rules, related constraints, health |
| `get_decisions_for_file` | Every decision anchored on a file, on a file under a directory, or on a directory that holds the file |

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
