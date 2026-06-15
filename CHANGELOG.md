# Changelog

All notable changes to this project will be documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

## [0.2.0] — 2026-06-15

### Added

- **Pattern regions on the map.** Convex-hull regions drawn around related
  decisions, with hit-testing, hover tooltips, click selection, and panel
  discoverability. LOD-aware labels that truncate at small zoom levels. Warm
  amber hue palette for visual distinction.
- **Edge tooltips.** Hovering an edge shows an instant tooltip with the
  connection kind label, rendered as a background pill for legibility.
- **Resizable detail panel.** Drag handle on the panel edge, back-navigation
  breadcrumb, and a collapse toggle. Tag pills replaced with a collapsible
  popover dropdown.
- **Loading and error states.** The map now shows a spinner during initial graph
  fetch and a clear error message on failure, instead of a blank canvas.
- **`trurlic migrate` CLI command.** Upgrades `.trurlic/` stores across format
  versions. Atomic backup, dry-run preview, symlink-safe traversal, and
  TOCTOU-resistant file operations. Graph hashes rebuilt after migration
  round-trips.
- **Gemini provider.** Google Gemini support via the native
  `generativelanguage.googleapis.com` API. Uses Gemini's native request format
  (not OpenAI-compatible). Default model: `gemini-2.5-flash`. Configure with
  `-p gemini` and `GEMINI_API_KEY`.
- **Ollama provider.** Local LLM support via Ollama's OpenAI-compatible API.
  No API key required — connects to `http://localhost:11434` by default.
  Default model: `llama3.1`. Configure with `-p ollama`.
- **Custom provider.** Any OpenAI-compatible endpoint via `CUSTOM_BASE_URL`
  and `CUSTOM_API_KEY`. Configure with `-p custom`.
- **MCP protocol version `2025-11-25`.** Upgraded from `2024-11-05`.
- **MCP tool annotations.** All tools now include `readOnlyHint`,
  `destructiveHint`, and `openWorldHint` annotations per the MCP 2025-11-25
  specification, enabling clients to make informed decisions about tool
  invocation.
- **`trurlic install --ide <ide>`.** Writes MCP server configuration for 11
  IDEs: Cursor, Claude Code, Windsurf, Cline, GitHub Copilot, Claude Desktop,
  Codex CLI, OpenCode, OpenClaw, Hermes Agent, and Antigravity CLI. Supports
  `--dry-run` to preview config and `--binary-path` to override the embedded
  binary path.
- **Comprehension gates.** `SummaryGate` step added to Feature, Learn, and
  Review workflows — the developer must summarize their understanding before
  the workflow advances. `UserExplains` step added to Learn flow so the user
  describes the component from memory before seeing code.
- **Step evidence.** Gated (interactive) steps now require evidence of user
  involvement (≥20 bytes) instead of bare step names. Prevents agents from
  rubber-stamping design gates with empty strings. `advance()` accepts a
  `step_evidence` object (key → evidence text) replacing the `completed_steps`
  array. `requires_user_input` field in advance responses signals which steps
  need human input.
- **Decision attribution.** `Attribution` enum (`User` | `Agent`) on every
  decision tracks whether it was authored by a human or autonomously.
  Agent-attributed decisions display "(agent — unconfirmed)" in context briefs.
  Store `FORMAT_VERSION` bumped to 0.3.0.

### Fixed

- **Map dark mode.** Shifted from warm brown to neutral blue-gray palette.
  Fixed stale color references, badge contrast, edge label shadows, and
  minimap sizing.
- **Arrowhead alignment.** Edge arrowheads now intersect rectangular node
  borders correctly via ray-rect intersection, instead of pointing at the
  center.
- **Frontend XSS hardening.** `esc()` utility now escapes quotes to prevent
  attribute injection. Extracted into a shared module with test coverage.
- **Command palette crash.** Fixed a crash when opening the command palette
  with no prior selection, along with `removeNode` bugs and a migrate panic.
- **Map layout polish.** Search bar and hint overlay positioned relative to the
  canvas area. Focus-visible styles restored on overlay close. Tag popover
  checkboxes and viewport clamping fixed.
- **Migrate safety.** Closed a TOCTOU race in backup creation, partial backups
  cleaned on failure, symlinks skipped during backup traversal, and dry-run
  reporting for `graph.toml` fixed.
- **Provider security.** The intermediate `Bearer <token>` string in the OpenAI
  client is now wrapped in `Zeroizing<String>`, matching the Anthropic client's
  zero-on-drop guarantee.
- **Bootstrap component targeting.** `ExtractDecisions` step now uses the
  correct inner component field instead of always referencing "project".
- **Advance loop termination.** Added completed-step guards in `deduce_harden`
  and `deduce_new_component` to prevent infinite `CoverConcerns` loops when
  decisions don't match concern keywords. Bootstrap loops bounded to 200
  iterations.
- **Control-char bypass.** Array parameters in MCP write tools (`alternatives`,
  `depends_on`, `tags`) now validate for control characters, closing a bypass
  that scalar fields already blocked.
- **Panic path elimination.** `unreachable!()` calls in workflow action dispatch
  and MCP tool dispatch replaced with safe fallbacks — server no longer crashes
  on classification/dispatch desync.
- **Session hardening.** Deterministic serialization via `BTreeSet` instead of
  `HashSet` for completed steps. Round-trip verification on session save.
  Symlink-safe file traversal. Message count warning at 80 messages. Session
  context refactored out of nested loops.
- **Exhaustive cascade matching.** Wildcard arms in `check_decision_cascade` and
  `check_component_cascade` replaced with explicit `(EdgeKind, Direction)`
  enumeration so the compiler catches new variants.
- **Type-safe parameters.** Opaque `bool` parameters replaced with enums:
  `SessionMode` (`Fresh` | `Continue` | `Revisit`) and `ApiVariant`
  (`Standard` | `OpenRouter`).
- **Advance purity.** Removed `env::var_os()` and `eprintln!()` debug calls
  from `advance()`, restoring its documented pure-function contract.
- Validation messages use `as_str()` instead of `{:?}` Debug formatting.
- `sanitize()` no longer appends ellipsis on control-char removal (only on
  truncation).
- MCP `tool_result()` logs serialization failures to stderr instead of
  silently returning empty JSON.
- Map API validates NaN/Infinity in layout positions, returns 500 on layout
  save failure, and uses atomic temp-file-then-rename for `layout.json`.
- CLI `--continue` and `--revisit` flags marked mutually exclusive.
- Added missing MIME types in map embed (ico, json, map, mjs, wasm, woff).

### Performance

- Cache edge pair set per frame and pattern convex hulls to avoid recomputing
  on every render. Cache theme media-query result instead of polling.
- Pre-compute decision word sets once per decision in concern coverage instead
  of per (decision × concern) pair (~10× fewer allocations).
- Pre-size `HashMap`/`HashSet`/`Vec` collections across store, MCP, and
  workflow modules. Replace `format!()` haystack allocation in `check_pattern`
  with direct field splitting.
- Borrow index strings in the intern pool instead of cloning into `HashMap`
  keys. Move hash strings instead of cloning in `load_graph_index`.
- Fix `HashMap` overallocation in `to_index()` — pre-size by node count
  instead of edge count to avoid sparse tables.
- Replace per-request `serde_json::json!({})` with static `LazyLock` in MCP
  tool dispatch.
- Inline edge lookups in `related_decisions` to avoid intermediate `Vec`s.
- Lazy `ready_action` construction in `advance_project` — JSON only built
  when needed.
- Remove redundant `content-type` headers in provider HTTP clients.

### Changed

- `advance()` signature: `completed_steps: &[&str]` → `step_evidence:
  BTreeMap<&str, &str>`.
- Store `FORMAT_VERSION` bumped from 0.2.0 to 0.3.0 (attribution field).

## [0.1.0] — 2026-06-10

First public release. The decision graph format and MCP tool surface may change
in breaking ways before v1.0. Pin to a specific version for production use.

### Scope

Trurlic is an architecture layer for AI-assisted codebases. You record
decisions in a typed graph, reason through them via Socratic design
conversations, and every AI coding agent gets them as hard constraints via MCP
before generating a single line. One decision graph, every agent follows it,
nothing slips through.

v0.1 ships the full decision lifecycle — define, decide, design, bootstrap,
query, visualize — with MCP integration for Claude Code, Cursor, Windsurf, and
any MCP-compatible coding agent.

### Decision store

- TOML-based typed graph stored in `.trurlic/`, git-tracked. Four node types:
  components, decisions, patterns, and a project root. Five edge types:
  `belongs_to`, `connects_to`, `depends_on`, `constrains`, and `implements`.
- `graph.toml` compiled edge index, rebuilt deterministically from node files.
  Human-readable, machine-queryable. Hand-edit a TOML file, run `trurlic check`,
  the graph reconciles. `--rebuild` reconstructs the full index from node files.
- **Atomic writes.** Serialize → write to temp file → verify round-trip
  parse → rename into place. `graph.toml` renamed last as the commit point.
  Interrupted writes are cleaned up on next startup.
- **Fail-closed validation.** Every mutation validates the full graph before
  touching disk: dangling edge detection, cycle detection, duplicate name
  checks, schema compliance, reserved name enforcement, input size limits.
  Invalid writes are refused with a clear error, never silently committed.
- Cross-platform file locking (`fs2`) prevents concurrent CLI + MCP + map
  mutations from corrupting state.
- Parallel file I/O via `rayon` for `load_state` — node files loaded
  concurrently, fast even for large graphs.
- BLAKE3 content hashing with tamper detection. `trurlic check` verifies
  every node file hash against the graph index.
- Cascade analysis for safe removals: `remove_decision` checks for downstream
  dependencies (`constrains`, `implements` edges) and refuses removal if other
  nodes depend on it. `remove_component` refuses if decisions reference it.
- Live filesystem watcher (`notify` crate) detects external changes to
  `.trurlic/` (CLI writes, manual edits, `git checkout`) and reloads state
  automatically. Shared by the MCP server and map.

### MCP server

- Twelve tools exposed over MCP (stdio transport, protocol version
  `2024-11-05`):

  | Tool | Purpose |
  |------|---------|
  | `advance` | Compute workflow step, return next action — the orchestration hub |
  | `get_context` | Architectural brief: decisions, rules, related constraints |
  | `check_pattern` | Check if an approach is already covered |
  | `get_architecture` | Full system overview: components, connections, patterns |
  | `get_design_prompt` | Structured prompt for design conversations |
  | `add_component` | Add a component to the graph |
  | `add_connection` | Connect two components |
  | `record_decision` | Record a decision with edges, tags, and rejected alternatives |
  | `record_pattern` | Synthesize a pattern from multiple related decisions |
  | `update_decision` | Amend (typo fix) or supersede (substantive change) |
  | `remove_decision` | Remove with cascade analysis |
  | `validate_consistency` | Full graph integrity check |

- `Arc<RwLock<ProjectState>>` shared with a file watcher thread. Tool calls
  hold the write lock only for pointer swaps (microseconds). Read queries never
  block the watcher or other reads.
- Write tools (`record_decision`, `record_pattern`, `update_decision`,
  `remove_decision`, `add_component`, `add_connection`) acquire an exclusive
  file lock and run full graph validation before committing.
- Context assembly uses authoritative language (MUST / DO NOT) — constraints,
  not suggestions. Related decisions from connected components are included
  transitively.

### Workflow engine

- Seven task types, each with a distinct step sequence:

  | Type | Flow |
  |------|------|
  | `new_component` | DefineScope → CoverConcerns → PatternDetection → SummaryGate → Ready |
  | `feature` | VerifyConstraints → CoverConcerns (focused) → PatternDetection → Ready |
  | `fix` | VerifyConstraints → ImpactCheck → Ready |
  | `learn` | AnalyzeCode → WalkDecisions → PatternDetection → Ready |
  | `review` | WalkDecisions → DriftCheck → CoverageAudit → PatternDetection → Ready |
  | `harden` | CoverageAudit → CoverConcerns (gaps) → PatternDetection → Ready |
  | `bootstrap` | ScanProject → ExtractDecisions → ProjectRules → PatternDetection → Ready |

- Step-by-step orchestration via `advance`: each call returns instructions for
  exactly one step. The graph is the primary state — the state machine inspects
  it and deduces which step comes next.
- Concern tracking and pattern detection across decisions. Pattern
  opportunities surfaced automatically when multiple decisions share a theme.
- Staleness detection for decisions older than 90 days during review workflows.
- Comprehension gates: Socratic checkpoints where the developer articulates
  understanding before the workflow proceeds.
- Transport-agnostic prompt generation — workflow logic lives in one module,
  usable from MCP, CLI sessions, or future transports.

### CLI

- `trurlic init` — create `.trurlic/` with project metadata and directory
  structure.
- `trurlic add component <name> [-d <desc>]` — define a component.
- `trurlic add connection <from> <to>` — connect two components (directional).
- `trurlic rename component <old> <new>` — rename, updating all references
  atomically (node files, graph index, edge targets).
- `trurlic remove component|decision|connection` — remove with safety checks.
- `trurlic decide <component> --choice "..." --reason "..."` — record a quick
  decision without the full Socratic flow. Supports `--supersede` and
  `--alternative`.
- `trurlic design <component>` — Socratic design conversation with
  `--continue` (resume), `--revisit` (challenge existing decisions), and
  `--task` (focused mode). Provider selection via `-p` and model override
  via `-m`.
- `trurlic bootstrap [<component>]` — show bootstrap progress and agent
  instructions for autonomous architecture extraction. Direct mode with
  `-p`/`-m` runs the bootstrap via the LLM API.
- `trurlic serve` — start the MCP server on stdio.
- `trurlic map [--port N] [--no-open] [--detach]` — open the interactive graph
  in the browser.
- `trurlic status` — show component count, decision count, and health.
- `trurlic check [--rebuild]` — validate `.trurlic/` internal consistency.
  `--rebuild` reconstructs the graph index from node files.

### Map (interactive visualization)

- Canvas-based graph renderer with force-directed layout, level-of-detail
  scaling, and viewport culling for large graphs.
- WebSocket live sync — changes from the CLI, MCP server, or manual file edits
  appear in the map instantly via the filesystem watcher.
- Interactive features: drag nodes, hover for details, click to select,
  multi-select, search across components and decisions, undo/redo, keyboard
  shortcuts, command palette, breadcrumb navigation, filtering by node type.
- Embedded frontend assets compiled into the binary via `rust-embed` — no
  external files needed at runtime.
- Token-based authentication for the local web server (cryptographic random
  token via `rand`, passed as a query parameter on browser launch). Security
  headers via `tower-http`.
- Diff-based WebSocket updates — only changed nodes and edges are pushed,
  not the full graph.

### Design conversations

- Socratic design flow powered by `trurlic design`: the LLM asks probing
  questions, the developer thinks and answers, decisions are recorded
  immediately after each answer — crash-safe by design.
- Three LLM providers: Anthropic (Claude), OpenAI (GPT), and OpenRouter.
  Auto-detection from available API keys.
- Session persistence to `.trurlic/state/sessions/` — resume interrupted
  sessions with `--continue`, revisit existing decisions with `--revisit`.
- API keys sourced from environment variables (`ANTHROPIC_API_KEY`,
  `OPENAI_API_KEY`, `OPENROUTER_API_KEY`) or a config file at
  `~/.config/trurlic/config.toml` (must be `chmod 600`).
- Keys zeroed from memory on drop (`zeroize` crate) and never appear in logs,
  error messages, or debug output. Display and Debug impls show only the last
  4 characters.
- SSE streaming for real-time response rendering in the terminal.

### Bootstrap

- Autonomous architecture extraction from existing codebases via
  `trurlic bootstrap`. The coding agent reads the source code and records
  components, decisions, and patterns without interactive dialogue.
- Agent-driven workflow via MCP `advance` tool with `task_type="bootstrap"`:
  scan the project, extract decisions per component, synthesize project rules,
  detect patterns.
- Direct mode (`-p anthropic`) runs the bootstrap via the LLM API without
  a separate coding agent.
- Single-component bootstrap: `trurlic bootstrap auth` to extract decisions
  for one component only.

### Security

- `#![deny(unsafe_code)]` — no unsafe Rust anywhere in the codebase.
- `#![deny(clippy::unwrap_used, clippy::expect_used)]` outside test code —
  a naked `unwrap`/`expect` in production code is a compile error.
  `panic = "abort"` in the release profile.
- API keys wrapped in `Zeroizing<String>` at the boundary; zeroed from memory
  on drop, never logged or displayed.
- Config file permissions enforced: `~/.config/trurlic/config.toml` must be
  `chmod 600` or the key is rejected.
- Map server: cryptographic random token authentication, CORS configuration
  via `tower-http`, security headers on all responses.
- `cargo deny` for advisory, license, ban, and source auditing. npm supply
  chain checks (cve-lite-cli, npm audit, lockfile-lint) for the frontend.
- `overflow-checks = true` in the release profile.

### Build & distribution

- Installer script (`install.sh`) with platform detection, minisign signature
  verification (fail-closed when `minisign` is absent, bypassable with
  `TRURLIC_SKIP_SIGNATURE_CHECK=1`), and support for version pinning,
  custom install directories, and target triple overrides.
- Release binary: `strip = true`, fat LTO, `codegen-units = 1`,
  `panic = "abort"`.
- Cross-compilation support via `Cross.toml` for `aarch64-unknown-linux-gnu`
  (image override to Ubuntu 20.04 for glibc ≥2.27).
- Makefile targets: `install`, `setup`, `build`, `build-release`, `test`,
  `check`, `fmt`, `audit`, `ci`, `clean`, plus frontend-specific variants.
- Git hooks installed via `make setup`.
- Conventional commit conventions with scoped types.

### Testing

- 609 unit tests covering the store (schemas, validation, writes, cascade,
  graph, queries, state, watcher), MCP server (protocol, tools, context
  assembly, writes, updates), workflow engine (advance, steps, concerns,
  task types), session (persistence, extraction, files), CLI commands
  (init, component, decision, design, query, bootstrap), map (diff, layout,
  token), and providers (SSE streaming).
- TypeScript frontend tests (force layout, camera, culling, edges, geometry,
  level-of-detail, graph state, drag, hover, selection, search).
- CodSpeed benchmarks for store operations (via `criterion` /
  `codspeed-criterion-compat`).

[Unreleased]: https://github.com/trurlic-labs/trurlic/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/trurlic-labs/trurlic/releases/tag/v0.1.0
