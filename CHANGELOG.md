# Changelog

All notable changes to this project will be documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

### Fixed

- **A command whose output pipe closes fails instead of aborting.** CLI output went through `println!`, which panics when stdout is closed, and release builds abort on a panic: `trurlic status | head -0` died with SIGABRT. Output now goes through one writer that returns the error, and the command exits with status 1 and `error: I/O error: Broken pipe`.

## [0.3.2] — 2026-10-03

### Fixed

- **A second writer on Windows waits for the lock.** File locking went through `fs2`, and a writer retried only on `ErrorKind::WouldBlock`. On Windows `fs2` reports a held lock as the raw `ERROR_LOCK_VIOLATION`, which std does not map to that kind, so a second writer failed at once with an I/O error instead of waiting. Locking now uses std's `File::try_lock` and `File::try_lock_shared`, which report a held lock the same way on every platform; the 5 s timeout stays. `fs2` and `winapi` are dropped.
- **Concurrent writers no longer lose each other's writes.** MCP and the map validated a write against the state they last loaded, before taking the file lock, and then committed the whole graph index from it, so a commit another process made in between was silently dropped (an edge, a node, a revision), and two processes could give two decisions the same file stem. Every write now takes the file lock, reloads the graph from disk and validates against that. A commit counter in `.trurlic/.state/generation` is raised by every commit, and a commit built on a state older than the store is refused. The map now takes its state lock before the file lock, like every other writer, and waits for a busy lock instead of answering 503 at once.
- **Watchers serve what is on disk.** The MCP and map watchers reloaded without a lock, so they could read a commit halfway through; dropped the events of a write that landed during a reload, so they kept serving removed nodes; and swapped a load in even when it predated the server's own last write. They now reload under a shared file lock, keep every event, check the format version, log failures with the store's path, and skip a load only when the server's own write overtook it, so deleting `.trurlic/.state/` does not freeze them. The map diffs and swaps under one lock, so its WebSocket events arrive in commit order.
- **A commit lands whole, or not at all.** A commit renamed its files one by one with `graph.toml` last, and nothing recorded it as a whole: a rename that failed midway left the earlier ones on disk, and an abort after `graph.toml` during `remove decision` left the decision file behind, so `status` counted the removed decision while `check` passed. Each commit now writes a journal, `.trurlic/.state/txn.toml`, naming every staged file before it renames anything. Once the journal is in place the commit lands: the next write, a command that finds the lock free, or a server's watcher applies it if the writer stopped. A write that fails after that point reports the commit as recorded but not applied (`CommitPending`).
- **Commits survive a power loss.** Nothing was flushed to disk, so a power loss could keep a rename and lose the data it pointed at, leaving an empty node file. Staged files, the journal and the directories they change are now flushed. Measured on WSL2 ext4, where one flush takes about 5 ms, an MCP `add_component` went from 3.5 ms to 40 ms median.
- **Commands no longer delete another process's commit.** Every command removed `.trurlic/.state/tmp/` on start without the lock, so a `status`, `check`, `serve` or `map` started during a commit deleted its staged files and the commit failed. Temp files are now removed only under the exclusive lock, and a command skips that cleanup while another process holds it. Staged files are named per process.
- **`check` reports a dependency cycle once, by its members, the same way on every run.** The cycle check named the decision its search started from, which hash order chose, and after finding one cycle it reported a second, false one at any decision that depended into the first, so the error count changed between runs of the same graph. It now finds each cycle as a strongly connected component and names its members, sorted; a decision that only depends into a cycle is not reported. Every validation issue is sorted, so `check` prints the same bytes on every run. The search keeps its stack on the heap, so a long dependency chain no longer overflows the stack.
- **An invalid graph no longer blocks every write.** A write was refused while the graph had any error, so one cycle left by a hand edit froze the store, and `gc`, which tolerated errors that were already there by comparing their messages, failed on a store with a cycle in about one run of six. Every write, `gc` included, is now refused only for an error it adds, matched on the error's kind and subject.
- **Every tool result fits 24 KiB.** On a seeded graph of 600 decisions, 1033 of 2305 tool calls returned more than 24 KB, up to 1.07 MB, so Claude Code warned or moved the result to a file the agent read in part. Every result, errors and writes included, now holds at most 24 KiB. A payload that fits is sent as before. A larger one keeps its shape: each list and string shrinks to a common level, short lists stay whole, the longest lose their tails, and a `truncated` array at the root names each cut by JSON pointer with the items or bytes it omitted. An error message is cut at a character and says how many bytes it lost.
- **Step prompts fit the budget and keep their instructions.** A step prompt lists the decisions the step works on, and on a component of 200 decisions it reached 330 KB. Its listings now shrink to fit, each ending with a line counting what it left out, while the step's instructions and the protocol stay whole. A step that walks the component's decisions no longer lists them a second time under EXISTING DECISIONS, and `cover_concerns` lists each area's covering decisions one per line.
- **Every server answers a read with the same bytes.** `get_context`, and `get_step_prompt`, which appends it, listed the decisions of connected components in an order each process seeded afresh; `record_decision` listed the components of a suggested pattern the same way, and the map sent the events of one change in hash order. All three now come in name order, so two servers on one graph answer a call identically and every map client sees one sequence of events.
- **`graph.toml` changes only with the graph.** Its `rebuilt` timestamp was rewritten by every commit, which changed the file on every write and conflicted across branches. The stamp is now kept as it was; files without it are read too. After a write, the in-memory index is the sorted one on disk.

### Added

- **Pattern removal: `remove_pattern` MCP tool and `trurlic remove pattern`.** A pattern could be recorded but never removed, and the cascade refuses to shrink a pattern below two members, so a pattern's last members could not be removed either. Both paths call one `Store::remove_pattern`, which deletes the pattern file with its `member_of` and `applies_to` edges in a single validated commit; member decisions are kept. An unknown name is a `PatternNotFound` error.

### Changed

- **MSRV is 1.90.** std's file locks are stable since 1.89 and tree-sitter 0.27 needs 1.90. A CI job now checks every target and feature with 1.90, so code that needs a newer compiler than `rust-version` claims fails the build.
- **`get_step_prompt` no longer returns `context`.** It appended the full `get_context` output to every step, although the prompt already carries the decisions the step works on; on the same seeded graph that copy made 960 of 1025 step prompts overflow. Call `get_context` for the brief.

### Removed

- **`trurlic check --rebuild`.** It deleted `graph.toml` before loading, so a load that failed left the store with no index, and it then committed without validating and dropped every edge but `belongs_to`. Every load already rebuilds the node list, hashes and `belongs_to` edges of a missing `graph.toml` from the node files, and the next write stores it; the other edges live only in `graph.toml`, so a damaged one is restored from version control.

## [0.3.1] — 2026-07-10

### Added

- **`verify_against_decisions` MCP tool.** Closes the implementation feedback loop: after an agent writes code, this read-only tool returns the architectural decisions that apply to the changed files, plus instructions to check the code respects them before committing. Given a `component` and a `changed_files` array, it partitions the component's decisions into affected and unaffected — a decision with `code_refs` is affected when any ref matches a changed file (bidirectional exact-or-directory-prefix match, so a directory-valued ref like `src/auth` matches a changed `src/auth/token.rs` and vice versa); a decision without `code_refs` is affected when it carries a `scope` or `boundary` tag and then applies to every changed file. Each affected decision reports the `affected_files` that triggered it. Project-wide rules are always returned in full. No file contents are read, no LLM is called, nothing is mutated — the same read path as `get_context`. Changed-file paths are normalized and validated (leading `./` stripped; backslashes, `..` traversal, and absolute paths rejected); a missing/empty `changed_files`, a nonexistent component, or an invalid path is a tool error, never a silent skip.

### Removed

- **`trurlic design` and `trurlic bootstrap` subcommands.** The CLI is no longer a design interface — Socratic design conversations, decision recording, and the advance loop now happen exclusively through MCP. The LLM subsystem that existed solely to serve those commands is gone: the `session/` module (design driver, bootstrap driver, LLM response extraction, session persistence), the `provider/` module (Anthropic/OpenAI/OpenRouter/Gemini/Ollama/Custom clients and SSE streaming), and the `config/` module (provider resolution, API-key handling). The `Error::ProviderConfig` and `Error::Api` variants are removed with them. The now-orphaned `reqwest` (HTTP client) and `zeroize` (API-key zeroing) dependencies are dropped from the manifest. The map, query tools, and component/decision management CLI are unchanged.
- **`.trurlic/.state/sessions/` directory.** `trurlic init` no longer creates this directory — it only ever held the design-session persistence and lock files written by the removed `session/` module. The `.state/tmp/` directory (used by atomic writes) is still created.

## [0.3.0] — 2026-07-05

### Added

- **`trurlic decide --ref` flag.** Code references can now be attached from the CLI via repeatable `--ref <file[::symbol]>` flags. The argument is split on the first `::` only — the file part is everything before it, the symbol part everything after. Validation flows through the existing `store::validate_code_refs` at the store trust boundary.
- **`get_decisions_for_file` query tool.** New read-only MCP tool and CLI command (`trurlic query file <path>`) that finds all decisions whose `code_refs` reference a given file (exact match) or any file under a directory (prefix match). Normalizes input paths (strips `./`, collapses `//`, rejects absolute paths and traversals). Results sorted deterministically by (component, name).
- **Agent / interactive mode separation.** `advance()` and `get_step_prompt` accept a `mode` parameter (`agent` | `interactive`). When omitted, `advance` returns `requires_mode: true` so the caller can present the choice. Agent mode removes comprehension gates (`SummaryGate`, `UserExplains`), skips step evidence validation, sets `requires_user_input: false` on every step, and uses autonomous prompt variants that instruct the AI to read source code and record decisions with `attribution="agent"`. Interactive mode preserves the existing Socratic design flow unchanged. Mode × task-type validation enforces that `learn` requires interactive (it builds user understanding) and `bootstrap` requires agent (it's autonomous extraction).
- **Agent protocol.** `AGENT_PROTOCOL` block replaces `INTERACTION_PROTOCOL` in agent mode — instructs the AI to analyze source code as primary evidence, record with `attribution="agent"`, flag domain-knowledge gaps with `[needs-review]`, and never wait for user input.
- **Mode-aware step prompts.** Every substantive step (`define_scope`, `analyze_code`, `cover_concerns`, `walk_decisions`, `verify_constraints`, `impact_check`, `pattern_detection`, `drift_check`, `coverage_audit`) now generates mode-specific instructions. Agent variants emphasize code reading and autonomous recording; interactive variants preserve the existing ask-then-record dialogue.
- **Code references on decisions.** Decisions carry an optional `code_refs` list — `{ file, symbol? }` entries pinpointing where a decision manifests in source (no line numbers, which go stale). Plumbed through `record_decision`, `update_decision` (revise replaces the refs), the map revise endpoint, and every context brief and step prompt. Paths are validated syntactically at the store trust boundary (relative, no `..` segment, forward slashes, no control characters) with counts capped by `MAX_CODE_REFS`; agent-mode prompts now instruct the AI to attach refs to every decision. Non-array or empty-symbol input is rejected, never silently dropped.
- **In-place decision revision with history.** A decision is a single document that evolves in place: `update_decision(mode="revise")` edits `choice`, `reason`, `tags`, or `code_refs` and, when a substantive field (`choice` or `reason`) changes, pushes the pre-edit values to a `history` list on the same node. History is chronological (oldest first) and ring-buffered to `MAX_HISTORY_ENTRIES` (20); the oldest entry drops when the limit is exceeded. The decision keeps its name and every incident edge across revisions.
- **Decision promotion.** `update_decision(mode="promote")` flips a decision's attribution from `agent` to `user`, marking it human-reviewed. Rejects a decision that is already `user`. Leaves history untouched.
- **`get_decision_history` MCP tool.** Returns a decision's current `choice`, `reason`, `attribution`, and `created`, its full chronological `history`, and a `revision_count`.
- **Decision health in `get_context`.** The full context brief carries a `health` object (`total`, `agent_unreviewed`, `stale`, `warning`). The warning fires on the first matching condition: more than 20 decisions (consolidate), more than 5 unreviewed agent decisions (pending review), or any stale decision (references deleted files). Unreviewed agent decisions carry a promote/revise call-to-action in the brief.
- **Staleness detection.** A decision whose `code_refs` all point to files missing on disk is flagged `⚠ STALE` in the brief and counted in `health.stale`. Decisions with no refs, or with at least one live ref, are never stale.
- **Coverage feedback on removal.** `remove_decision` reports a `coverage_impact` object naming concern areas that lost their last covering decision, plus the remaining covered/uncovered counts. The CLI prints a `⚠ [component] lost coverage: …` line.
- **Unreviewed-decision count in `advance`.** The ready response carries `agent_decisions_unreviewed` and a hint when agent decisions await promotion.
- **`trurlic gc`.** Reclaims decision debt. Safe mode removes decisions orphaned by a deleted component and reports (without removing) stale decisions and agent decisions older than 90 days that were never promoted; `--aggressive` also removes the stale and old-agent candidates; `--dry-run` reports only. Cascade-blocked candidates are skipped and reported — never silently dropped — and coverage impact is shown per component. CLI only.
- **Bulk agent-decision removal.** `trurlic remove decision --component <c> --agent` removes every agent-attributed decision in a component atomically: cascade is pre-flighted across all candidates, and if any is blocked, none are removed.
- **Hardened duplicate detection.** `record_decision` hard-errors on a choice identical (case-insensitive) to an existing decision in the same component, pointing at `update_decision(mode="revise")`, and warns on high word overlap (Jaccard > 0.7) with a same-component decision.
- **Promote guardrails.** The `update_decision` MCP tool description now explicitly prohibits autonomous promotion — agents must never call `mode="promote"` without the user's explicit review and confirmation. `AGENT_PROTOCOL` carries the same prohibition. The ready-response hint and context-brief call-to-action now address the user, not the agent.
- **Map decision panel shows code refs, attribution, and revision count.** The decision detail view now renders `code_refs` as monospace chips in `file::symbol` format, an amber "agent · unreviewed" attribution badge for agent-authored decisions, and a "Revised N×" indicator when a decision has revision history. The REST API response includes `attribution` and `revision_count` fields. TypeScript types updated with `CodeRefData` interface.

### Changed

- **`get_context` health field renamed: `stale` → `orphaned_refs`.** The term "stale" previously described two unrelated concepts — workflow staleness (decisions older than the threshold, driving `DriftCheck`) and reference orphaning (all `code_refs` point at deleted files). The health payload field, brief marker (`⚠ ORPHANED REFS`), gc report section title, and CLI help text now use "orphaned refs" for the reference-deletion concept, reserving "stale" exclusively for age-based workflow staleness.
- `advance()` signature: added `mode: Option<Mode>` parameter.
- `build_step_prompt()` signature: added `mode: Mode` parameter.
- `build_step_prompt()` signature: added `now: DateTime<Utc>` parameter. Prompt generation is now a pure function — callers at system boundaries (MCP, session) inject `Utc::now()` rather than the function reading the clock internally.
- `get_step_prompt` MCP tool: `mode` is now a required parameter.
- `advance` MCP tool: `mode` is optional — omitting it triggers the mode gate.
- Response JSON includes `mode` field alongside `step`, `ready`, and `requires_user_input`.
- `update_decision` modes are now `revise` and `promote`. The map `PUT /api/decision/:name` endpoint maps its request body to a revise.
- Loading a design session drops any recorded-decision names whose decisions no longer exist in the graph.
- **`trurlic gc` defaults to dry-run.** `gc` now reports what would be reclaimed without writing by default. Pass `--apply` to perform removals. The former `--dry-run` flag is accepted as a hidden no-op for one release. `--aggressive --apply` requires `--yes` in non-interactive environments (piped output, CI); on a TTY it prompts for confirmation.

### Removed

- **`validate_consistency` MCP tool.** Every MCP write already validates the full graph fail-closed server-side, so an agent calling `validate_consistency` in normal operation learns nothing a write wouldn't tell it. Its one niche — diagnosing external file edits — is served by `trurlic check` (human) and the file watcher (server). Each tool schema costs context tokens in every agent session, on every client, forever. The underlying `graph().validate()` remains — it is the write-path validator and `trurlic check`'s engine.
- **`Supersedes` edge type.** In-place revision keeps prior versions inside the decision file, so a superseding edge to an old node no longer exists. Every read path returns exactly the active decisions with no filtering.
- **`amend` and `supersede` update modes.** Replaced by `revise` (which always records history) and `promote`. `update_decision(mode="amend")` and `update_decision(mode="supersede")` now return a clear invalid-mode error.

### Fixed

- **Review no longer loops forever on DriftCheck.** Staleness is now computed from `Decision::last_touched()` — the latest revision timestamp if revised, otherwise `created` — instead of the immutable `created` date. A revised decision clears staleness. Additionally, `deduce_review` now gates `DriftCheck` on `completed_steps`, matching the project-scope behavior and allowing progression when stale decisions are confirmed unchanged. The stale-decisions assessment payload now includes both `created` and `last_touched` for UI transparency.
- **Batch decision removal is now cascade-safe.** `gc` and `remove decision --agent` pre-flight the whole set with a batch-aware cascade that reaches a fixed point: co-removed dependents and pattern members no longer block one another, while removing two members of a shared pattern can no longer silently drop it below its two-member minimum. Previously each candidate was checked against the pre-batch graph, so a batch could commit a graph that `trurlic check` rejects.
- **`supersedes` edges are migrated away, not left to break loading.** The on-disk format is bumped to `0.4.0`. A store written by 0.2.0 could carry a `supersedes` edge in `graph.toml`; a typed read now rejects that whole file, so `trurlic migrate` strips retired edge kinds before rebuilding the index. Without the version bump, `migrate` reported "already up to date" and the store stayed unloadable.
- **Lost-coverage reports account for project-wide rules.** `remove_decision`, `remove decision --agent`, and `gc` compute erased concern coverage against the same baseline as `get_context` (component decisions **plus** project rules), so a concern a project rule still covers is never falsely reported as lost.
- **Project-level `advance` ready responses carry the agent-review hint.** A ready `component="project"` response now includes `agent_decisions_unreviewed` and `hint`, matching component-level responses, so unreviewed agent-authored project rules aren't silently exempt from review.
- **Duplicate-decision detection is whitespace-insensitive.** The hard block on restating an existing decision's choice normalizes whitespace as well as case, so a trailing or doubled space can't slip a near-duplicate past it.
- **Map `PUT /api/decision/:name` returns 404**, not 500, for an unknown decision, and 400 for invalid input.
- **Agent-mode inference no longer dead-ends on `Learn`.** `infer_task_type` is now mode-aware: a registered component with zero decisions and no task infers `Bootstrap` in agent mode (autonomous extraction) instead of `Learn` (which requires interactive mode). Interactive mode behavior is unchanged.
- **Feature and review workflows no longer loop on `CoverConcerns` / `CoverageAudit`.** `deduce_feature` now gates both `CoverConcerns` branches (task-relevant and majority-threshold) on `completed_steps`, and `deduce_review` gates its `CoverageAudit` branch the same way. Previously, if a recorded decision's text didn't hit the concern keyword list, the uncovered count never shrank and the state machine returned the same step forever.
- **`advance` is pure again — the wall clock is injected, not read.** Staleness (decisions older than the threshold) is now computed from a `now` the caller supplies, so `advance` is a deterministic function of `(graph, now)`. Reading `Utc::now()` internally made the same graph return different steps as the calendar advanced (a healthy component silently flipping to `review` on day 90), violating the purity invariant and the determinism the engine promises.
- **Batch decision removal is fail-closed at the store boundary.** `remove_decisions` now compares the graph's error set before and after removal and refuses any removal that introduces a *new* violation (e.g. dropping a pattern below its two-member minimum), while still tolerating pre-existing errors so the collector can repair an already-broken store. Safety no longer rests solely on each caller's pre-flight.
- **`revise` cannot manufacture a duplicate decision.** Revising a choice into a restatement of another decision in the same component is now rejected at the store layer — the same guard `record_decision` enforces — so neither the MCP nor the map transport can fork two nodes onto identical choice text.
- **`revise` of a missing decision is a 404, not a 400.** The existence check now precedes body-shape validation, so an empty-bodied revise of an absent decision surfaces `DecisionNotFound` (→ 404) rather than a validation error.
- **`trurlic migrate --dry-run` no longer crashes on a retired edge.** The preview parses `graph.toml` loosely (like the apply path) instead of a typed read that a `supersedes` edge — the very thing migration repairs — would reject, and it now reports how many retired edges would be stripped.
- **`migrate` is crash-safer and its backup is consistent.** The recovery backup is copied under the store lock (no torn snapshot from a concurrent writer), and retired edges are stripped before the version bump so an interrupted run leaves a loadable store rather than one bricked on an unknown edge kind. The apply path reports the count of edges it removed.
- **Staleness / orphaned-reference detection is deduplicated and I/O-robust.** The "all code references deleted" predicate lives once in `store` (`decision_refs_all_missing`), shared by the context health report and `gc`, and uses `try_exists` so a permission or mount error never misreports a live file as deleted and drives a decision to be flagged or collected.
- **`CodeRef` path validation rejects Windows drive-letter paths.** `C:/Users/x`, `c:/file.rs`, and bare `D:` forms are now rejected by `validate_code_ref` on all platforms, preventing `Path::join` from yielding an absolute path that escapes the project root on Windows. The string-level check (first segment ending with `:`) catches drive letters even on Unix builds, which matters because stores are git-shared across OSes. A colon in a later path segment (`src/a:b.rs`) remains valid.
- **Unknown `step_evidence` keys are now rejected with a clear error.** Previously a typo in a step name (e.g. `designcheck` instead of `design_check`) was silently accepted, matched nothing, and caused the state machine to return the same step indefinitely. Now `advance()` validates all `step_evidence` keys against `Step::ALL_NAMES` in both agent and interactive modes, returning an error that names the offending key and lists all valid step names.
- **Walk-decisions prompt no longer falsely labels `history[0]` as "original".** History is a ring buffer capped at 20 entries; after eviction, `history[0]` is the earliest *retained* version, not the original. The label is now `earliest recorded:` unconditionally, which is truthful regardless of buffer state.

### Changed (internal)

- `ReviseDecisionParams` no longer carries a `writes_history` flag — whether a revision versions history is derived from whether `choice`/`reason` changed.
- `get_context` computes decision staleness (a per-`code_ref` filesystem stat) only for the full brief, not the lightweight `constraints` depth.

## [0.2.0] — 2026-06-15

### Added

- **Pattern regions on the map.** Convex-hull regions drawn around related decisions, with hit-testing, hover tooltips, click selection, and panel discoverability. LOD-aware labels that truncate at small zoom levels. Warm amber hue palette for visual distinction.
- **Edge tooltips.** Hovering an edge shows an instant tooltip with the connection kind label, rendered as a background pill for legibility.
- **Resizable detail panel.** Drag handle on the panel edge, back-navigation breadcrumb, and a collapse toggle. Tag pills replaced with a collapsible popover dropdown.
- **Loading and error states.** The map now shows a spinner during initial graph fetch and a clear error message on failure, instead of a blank canvas.
- **`trurlic migrate` CLI command.** Upgrades `.trurlic/` stores across format versions. Atomic backup, dry-run preview, symlink-safe traversal, and TOCTOU-resistant file operations. Graph hashes rebuilt after migration round-trips.
- **Gemini provider.** Google Gemini support via the native `generativelanguage.googleapis.com` API. Uses Gemini's native request format (not OpenAI-compatible). Default model: `gemini-2.5-flash`. Configure with `-p gemini` and `GEMINI_API_KEY`.
- **Ollama provider.** Local LLM support via Ollama's OpenAI-compatible API. No API key required — connects to `http://localhost:11434` by default. Default model: `llama3.1`. Configure with `-p ollama`.
- **Custom provider.** Any OpenAI-compatible endpoint via `CUSTOM_BASE_URL` and `CUSTOM_API_KEY`. Configure with `-p custom`.
- **MCP protocol version `2025-11-25`.** Upgraded from `2024-11-05`.
- **MCP tool annotations.** All tools now include `readOnlyHint`, `destructiveHint`, and `openWorldHint` annotations per the MCP 2025-11-25 specification, enabling clients to make informed decisions about tool invocation.
- **`trurlic install --ide <ide>`.** Writes MCP server configuration for 11 IDEs: Cursor, Claude Code, Windsurf, Cline, GitHub Copilot, Claude Desktop, Codex CLI, OpenCode, OpenClaw, Hermes Agent, and Antigravity CLI. Supports `--dry-run` to preview config and `--binary-path` to override the embedded binary path.
- **Comprehension gates.** `SummaryGate` step added to Feature, Learn, and Review workflows — the developer must summarize their understanding before the workflow advances. `UserExplains` step added to Learn flow so the user describes the component from memory before seeing code.
- **Step evidence.** Gated (interactive) steps now require evidence of user involvement (≥20 bytes) instead of bare step names. Prevents agents from rubber-stamping design gates with empty strings. `advance()` accepts a `step_evidence` object (key → evidence text) replacing the `completed_steps` array. `requires_user_input` field in advance responses signals which steps need human input.
- **Decision attribution.** `Attribution` enum (`User` | `Agent`) on every decision tracks whether it was authored by a human or autonomously. Agent-attributed decisions display "(agent — unconfirmed)" in context briefs. Store `FORMAT_VERSION` bumped to 0.3.0.

### Fixed

- **Map dark mode.** Shifted from warm brown to neutral blue-gray palette. Fixed stale color references, badge contrast, edge label shadows, and minimap sizing.
- **Arrowhead alignment.** Edge arrowheads now intersect rectangular node borders correctly via ray-rect intersection, instead of pointing at the center.
- **Frontend XSS hardening.** `esc()` utility now escapes quotes to prevent attribute injection. Extracted into a shared module with test coverage.
- **Command palette crash.** Fixed a crash when opening the command palette with no prior selection, along with `removeNode` bugs and a migrate panic.
- **Map layout polish.** Search bar and hint overlay positioned relative to the canvas area. Focus-visible styles restored on overlay close. Tag popover checkboxes and viewport clamping fixed.
- **Migrate safety.** Closed a TOCTOU race in backup creation, partial backups cleaned on failure, symlinks skipped during backup traversal, and dry-run reporting for `graph.toml` fixed.
- **Provider security.** The intermediate `Bearer <token>` string in the OpenAI client is now wrapped in `Zeroizing<String>`, matching the Anthropic client's zero-on-drop guarantee.
- **Bootstrap component targeting.** `ExtractDecisions` step now uses the correct inner component field instead of always referencing "project".
- **Advance loop termination.** Added completed-step guards in `deduce_harden` and `deduce_new_component` to prevent infinite `CoverConcerns` loops when decisions don't match concern keywords. Bootstrap loops bounded to 200 iterations.
- **Control-char bypass.** Array parameters in MCP write tools (`alternatives`, `depends_on`, `tags`) now validate for control characters, closing a bypass that scalar fields already blocked.
- **Panic path elimination.** `unreachable!()` calls in workflow action dispatch and MCP tool dispatch replaced with safe fallbacks — server no longer crashes on classification/dispatch desync.
- **Session hardening.** Deterministic serialization via `BTreeSet` instead of `HashSet` for completed steps. Round-trip verification on session save. Symlink-safe file traversal. Message count warning at 80 messages. Session context refactored out of nested loops.
- **Exhaustive cascade matching.** Wildcard arms in `check_decision_cascade` and `check_component_cascade` replaced with explicit `(EdgeKind, Direction)` enumeration so the compiler catches new variants.
- **Type-safe parameters.** Opaque `bool` parameters replaced with enums: `SessionMode` (`Fresh` | `Continue` | `Revisit`) and `ApiVariant` (`Standard` | `OpenRouter`).
- **Advance purity.** Removed `env::var_os()` and `eprintln!()` debug calls from `advance()`, restoring its documented pure-function contract.
- Validation messages use `as_str()` instead of `{:?}` Debug formatting.
- `sanitize()` no longer appends ellipsis on control-char removal (only on truncation).
- MCP `tool_result()` logs serialization failures to stderr instead of silently returning empty JSON.
- Map API validates NaN/Infinity in layout positions, returns 500 on layout save failure, and uses atomic temp-file-then-rename for `layout.json`.
- CLI `--continue` and `--revisit` flags marked mutually exclusive.
- Added missing MIME types in map embed (ico, json, map, mjs, wasm, woff).

### Performance

- Cache edge pair set per frame and pattern convex hulls to avoid recomputing on every render. Cache theme media-query result instead of polling.
- Pre-compute decision word sets once per decision in concern coverage instead of per (decision × concern) pair (~10× fewer allocations).
- Pre-size `HashMap`/`HashSet`/`Vec` collections across store, MCP, and workflow modules. Replace `format!()` haystack allocation in `check_pattern` with direct field splitting.
- Borrow index strings in the intern pool instead of cloning into `HashMap` keys. Move hash strings instead of cloning in `load_graph_index`.
- Fix `HashMap` overallocation in `to_index()` — pre-size by node count instead of edge count to avoid sparse tables.
- Replace per-request `serde_json::json!({})` with static `LazyLock` in MCP tool dispatch.
- Inline edge lookups in `related_decisions` to avoid intermediate `Vec`s.
- Lazy `ready_action` construction in `advance_project` — JSON only built when needed.
- Remove redundant `content-type` headers in provider HTTP clients.

### Changed

- `advance()` signature: `completed_steps: &[&str]` → `step_evidence: BTreeMap<&str, &str>`.
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

- TOML-based typed graph stored in `.trurlic/`, git-tracked. Four node types: components, decisions, patterns, and a project root. Five edge types: `belongs_to`, `connects_to`, `depends_on`, `constrains`, and `implements`.
- `graph.toml` compiled edge index, rebuilt deterministically from node files. Human-readable, machine-queryable. Hand-edit a TOML file, run `trurlic check`, the graph reconciles. `--rebuild` reconstructs the full index from node files.
- **Atomic writes.** Serialize → write to temp file → verify round-trip parse → rename into place. `graph.toml` renamed last as the commit point. Interrupted writes are cleaned up on next startup.
- **Fail-closed validation.** Every mutation validates the full graph before touching disk: dangling edge detection, cycle detection, duplicate name checks, schema compliance, reserved name enforcement, input size limits. Invalid writes are refused with a clear error, never silently committed.
- Cross-platform file locking (`fs2`) prevents concurrent CLI + MCP + map mutations from corrupting state.
- Parallel file I/O via `rayon` for `load_state` — node files loaded concurrently, fast even for large graphs.
- BLAKE3 content hashing with tamper detection. `trurlic check` verifies every node file hash against the graph index.
- Cascade analysis for safe removals: `remove_decision` checks for downstream dependencies (`constrains`, `implements` edges) and refuses removal if other nodes depend on it. `remove_component` refuses if decisions reference it.
- Live filesystem watcher (`notify` crate) detects external changes to `.trurlic/` (CLI writes, manual edits, `git checkout`) and reloads state automatically. Shared by the MCP server and map.

### MCP server

- Twelve tools exposed over MCP (stdio transport, protocol version `2024-11-05`):

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

- `Arc<RwLock<ProjectState>>` shared with a file watcher thread. Tool calls hold the write lock only for pointer swaps (microseconds). Read queries never block the watcher or other reads.
- Write tools (`record_decision`, `record_pattern`, `update_decision`, `remove_decision`, `add_component`, `add_connection`) acquire an exclusive file lock and run full graph validation before committing.
- Context assembly uses authoritative language (MUST / DO NOT) — constraints, not suggestions. Related decisions from connected components are included transitively.

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

- Step-by-step orchestration via `advance`: each call returns instructions for exactly one step. The graph is the primary state — the state machine inspects it and deduces which step comes next.
- Concern tracking and pattern detection across decisions. Pattern opportunities surfaced automatically when multiple decisions share a theme.
- Staleness detection for decisions older than 90 days during review workflows.
- Comprehension gates: Socratic checkpoints where the developer articulates understanding before the workflow proceeds.
- Transport-agnostic prompt generation — workflow logic lives in one module, usable from MCP, CLI sessions, or future transports.

### CLI

- `trurlic init` — create `.trurlic/` with project metadata and directory structure.
- `trurlic add component <name> [-d <desc>]` — define a component.
- `trurlic add connection <from> <to>` — connect two components (directional).
- `trurlic rename component <old> <new>` — rename, updating all references atomically (node files, graph index, edge targets).
- `trurlic remove component|decision|connection` — remove with safety checks.
- `trurlic decide <component> --choice "..." --reason "..."` — record a quick decision without the full Socratic flow. Supports `--supersede` and `--alternative`.
- `trurlic design <component>` — Socratic design conversation with `--continue` (resume), `--revisit` (challenge existing decisions), and `--task` (focused mode). Provider selection via `-p` and model override via `-m`.
- `trurlic bootstrap [<component>]` — show bootstrap progress and agent instructions for autonomous architecture extraction. Direct mode with `-p`/`-m` runs the bootstrap via the LLM API.
- `trurlic serve` — start the MCP server on stdio.
- `trurlic map [--port N] [--no-open] [--detach]` — open the interactive graph in the browser.
- `trurlic status` — show component count, decision count, and health.
- `trurlic check [--rebuild]` — validate `.trurlic/` internal consistency. `--rebuild` reconstructs the graph index from node files.

### Map (interactive visualization)

- Canvas-based graph renderer with force-directed layout, level-of-detail scaling, and viewport culling for large graphs.
- WebSocket live sync — changes from the CLI, MCP server, or manual file edits appear in the map instantly via the filesystem watcher.
- Interactive features: drag nodes, hover for details, click to select, multi-select, search across components and decisions, undo/redo, keyboard shortcuts, command palette, breadcrumb navigation, filtering by node type.
- Embedded frontend assets compiled into the binary via `rust-embed` — no external files needed at runtime.
- Token-based authentication for the local web server (cryptographic random token via `rand`, passed as a query parameter on browser launch). Security headers via `tower-http`.
- Diff-based WebSocket updates — only changed nodes and edges are pushed, not the full graph.

### Design conversations

- Socratic design flow powered by `trurlic design`: the LLM asks probing questions, the developer thinks and answers, decisions are recorded immediately after each answer — crash-safe by design.
- Three LLM providers: Anthropic (Claude), OpenAI (GPT), and OpenRouter. Auto-detection from available API keys.
- Session persistence to `.trurlic/state/sessions/` — resume interrupted sessions with `--continue`, revisit existing decisions with `--revisit`.
- API keys sourced from environment variables (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `OPENROUTER_API_KEY`) or a config file at `~/.config/trurlic/config.toml` (must be `chmod 600`).
- Keys zeroed from memory on drop (`zeroize` crate) and never appear in logs, error messages, or debug output. Display and Debug impls show only the last 4 characters.
- SSE streaming for real-time response rendering in the terminal.

### Bootstrap

- Autonomous architecture extraction from existing codebases via `trurlic bootstrap`. The coding agent reads the source code and records components, decisions, and patterns without interactive dialogue.
- Agent-driven workflow via MCP `advance` tool with `task_type="bootstrap"`: scan the project, extract decisions per component, synthesize project rules, detect patterns.
- Direct mode (`-p anthropic`) runs the bootstrap via the LLM API without a separate coding agent.
- Single-component bootstrap: `trurlic bootstrap auth` to extract decisions for one component only.

### Security

- `#![deny(unsafe_code)]` — no unsafe Rust anywhere in the codebase.
- `#![deny(clippy::unwrap_used, clippy::expect_used)]` outside test code — a naked `unwrap`/`expect` in production code is a compile error. `panic = "abort"` in the release profile.
- API keys wrapped in `Zeroizing<String>` at the boundary; zeroed from memory on drop, never logged or displayed.
- Config file permissions enforced: `~/.config/trurlic/config.toml` must be `chmod 600` or the key is rejected.
- Map server: cryptographic random token authentication, CORS configuration via `tower-http`, security headers on all responses.
- `cargo deny` for advisory, license, ban, and source auditing. npm supply chain checks (cve-lite-cli, npm audit, lockfile-lint) for the frontend.
- `overflow-checks = true` in the release profile.

### Build & distribution

- Installer script (`install.sh`) with platform detection, minisign signature verification (fail-closed when `minisign` is absent, bypassable with `TRURLIC_SKIP_SIGNATURE_CHECK=1`), and support for version pinning, custom install directories, and target triple overrides.
- Release binary: `strip = true`, fat LTO, `codegen-units = 1`, `panic = "abort"`.
- Cross-compilation support via `Cross.toml` for `aarch64-unknown-linux-gnu` (image override to Ubuntu 20.04 for glibc ≥2.27).
- Makefile targets: `install`, `setup`, `build`, `build-release`, `test`, `check`, `fmt`, `audit`, `ci`, `clean`, plus frontend-specific variants.
- Git hooks installed via `make setup`.
- Conventional commit conventions with scoped types.

### Testing

- 609 unit tests covering the store (schemas, validation, writes, cascade, graph, queries, state, watcher), MCP server (protocol, tools, context assembly, writes, updates), workflow engine (advance, steps, concerns, task types), session (persistence, extraction, files), CLI commands (init, component, decision, design, query, bootstrap), map (diff, layout, token), and providers (SSE streaming).
- TypeScript frontend tests (force layout, camera, culling, edges, geometry, level-of-detail, graph state, drag, hover, selection, search).
- CodSpeed benchmarks for store operations (via `criterion` / `codspeed-criterion-compat`).

[Unreleased]: https://github.com/trurlic-labs/trurlic/compare/v0.3.2...HEAD
[0.3.2]: https://github.com/trurlic-labs/trurlic/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/trurlic-labs/trurlic/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/trurlic-labs/trurlic/releases/tag/v0.3.0
[0.2.0]: https://github.com/trurlic-labs/trurlic/releases/tag/v0.2.0
[0.1.0]: https://github.com/trurlic-labs/trurlic/releases/tag/v0.1.0
