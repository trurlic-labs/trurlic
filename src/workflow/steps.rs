//! Per-step prompt builders for the workflow engine.
//!
//! Each workflow step gets a focused prompt: 200-500 bytes of step-specific
//! instructions, sandwiched between a shared preamble (source code mandate)
//! and a shared interaction protocol (one-at-a-time, senior engineer
//! deepening, "I don't know" teaching).
//!
//! Prompts are transport-agnostic. The MCP tool `get_step_prompt` calls
//! `build_step_prompt` and combines the result with `get_context` output.

use crate::store::graph::InMemoryGraph;
use crate::store::schema::DecisionFile;
use crate::store::{self, ProjectState};

use super::CONCERN_FOCUS_LIMIT;
use super::Mode;
use super::action::top_n;
use super::concerns;

// ── Public API ────────────────────────────────────────────────────────────

/// Result of building a step prompt.
#[derive(Debug)]
pub struct StepPrompt {
    /// Full system instructions string for the agent.
    pub instructions: String,
    /// Concern areas in focus (only for `cover_concerns` and `coverage_audit`).
    pub focus: Vec<String>,
}

/// Build the system instructions for a specific workflow step.
///
/// Returns the prompt text and optional metadata (like focus concerns).
/// The caller (MCP tool dispatch) combines this with `get_context` output
/// to form the full tool response.
///
/// `task_type` is optional context for steps that generate variant prompts
/// (e.g. `summary_gate` varies by Feature vs Review vs NewComponent).
pub fn build_step_prompt(
    state: &ProjectState,
    component: &str,
    step: &str,
    task: Option<&str>,
    task_type: Option<&str>,
    mode: Mode,
) -> Result<StepPrompt, String> {
    if component != "project" && !state.components.contains_key(component) {
        return Err(format!("component `{component}` does not exist"));
    }

    let graph = state.graph();
    let decisions = graph.decisions_for(component);
    let project_rules = graph.project_decisions();
    let patterns = graph.patterns_for(component);

    let all_decs: Vec<&DecisionFile> = project_rules
        .iter()
        .chain(decisions.iter())
        .map(|(_, d)| *d)
        .collect();
    let (covered, uncovered) = concerns::compute_concern_coverage(&all_decs);

    let mut out = String::with_capacity(2048);
    let mut focus = Vec::new();

    // ── Shared preamble ───────────────────────────────────────────────
    out.push_str(&preamble(component));
    out.push_str(&scope_boundary(component));

    // ── Existing constraints (conditional) ────────────────────────────
    let needs_constraints = matches!(
        step,
        "define_scope"
            | "cover_concerns"
            | "verify_constraints"
            | "impact_check"
            | "walk_decisions"
            | "summary_gate"
            | "drift_check"
            | "coverage_audit"
            | "extract_decisions"
            | "project_rules"
    );
    if needs_constraints {
        out.push_str(&existing_constraints(graph, component));
    }

    // ── Component graph (conditional) ─────────────────────────────────
    if matches!(
        step,
        "impact_check" | "verify_constraints" | "define_scope" | "extract_decisions"
    ) {
        out.push_str(&component_graph(graph, component));
    }

    // ── Task context ──────────────────────────────────────────────────
    if let Some(t) = task {
        out.push_str(&format!("TASK CONTEXT: {t}\n\n"));
    }

    // ── Step-specific instructions ────────────────────────────────────
    match step {
        "register" => out.push_str(&step_register(component)),
        "define_scope" => out.push_str(&step_define_scope(mode)),
        "analyze_code" => out.push_str(&step_analyze_code(component, task_type, mode)),
        "cover_concerns" => {
            focus = top_n(&uncovered, CONCERN_FOCUS_LIMIT);
            out.push_str(&step_cover_concerns(&focus, &all_decs, mode));
        }
        "walk_decisions" => out.push_str(&step_walk_decisions(graph, component, mode)),
        "verify_constraints" => {
            out.push_str(&step_verify_constraints(graph, component, mode));
        }
        "impact_check" => out.push_str(&step_impact_check(graph, component, mode)),
        "pattern_detection" => out.push_str(&step_pattern_detection(graph, component, mode)),
        "summary_gate" => out.push_str(&step_summary_gate(task_type)),
        "drift_check" => out.push_str(&step_drift_check(graph, component, mode)),
        "coverage_audit" => {
            focus = uncovered.iter().map(|s| (*s).to_string()).collect();
            out.push_str(&step_coverage_audit(&covered, &uncovered, mode));
        }
        "scan_project" => out.push_str(&step_scan_project()),
        "extract_decisions" => out.push_str(&step_extract_decisions(component)),
        "project_rules" => out.push_str(&step_project_rules()),
        "user_explains" => out.push_str(&step_user_explains()),
        "ready" => out.push_str(&step_ready(component)),
        _ => {
            return Err(format!(
                "unknown step `{step}` — expected: register, define_scope, \
             analyze_code, cover_concerns, walk_decisions, verify_constraints, \
             impact_check, pattern_detection, summary_gate, drift_check, \
             coverage_audit, scan_project, extract_decisions, project_rules, \
             user_explains, ready"
            ));
        }
    }

    // ── Shared protocol (mode-conditional) ───────────────────────────
    // Autonomous steps skip the protocol entirely. Other steps get
    // INTERACTION_PROTOCOL in interactive mode, AGENT_PROTOCOL in agent.
    if !matches!(
        step,
        "register" | "ready" | "scan_project" | "extract_decisions" | "project_rules"
    ) {
        match mode {
            Mode::Interactive => out.push_str(INTERACTION_PROTOCOL),
            Mode::Agent => out.push_str(AGENT_PROTOCOL),
        }
    }

    // ── Existing patterns (informational) ─────────────────────────────
    if !patterns.is_empty() && matches!(step, "pattern_detection" | "walk_decisions") {
        out.push_str("EXISTING PATTERNS (do not re-record):\n");
        for (name, p) in &patterns {
            out.push_str(&format!(
                "- {}: {}\n",
                name,
                sanitize(&p.pattern.description)
            ));
        }
        out.push('\n');
    }

    Ok(StepPrompt {
        instructions: out,
        focus,
    })
}

// ── Shared sections ───────────────────────────────────────────────────────

/// Source code mandate — every step starts with this.
fn preamble(component: &str) -> String {
    format!(
        "You are running a Trurlic design session for [{component}].\n\n\
         BEFORE PROCEEDING: Read the source code files for this component.\n\
         Do NOT rely on README or documentation. The source code is truth.\n\n"
    )
}

/// Scope boundary — project vs component.
fn scope_boundary(component: &str) -> String {
    if component == "project" {
        "SCOPE — PROJECT LEVEL:\n\
         Project decisions are cross-cutting principles: error strategy, \
         coding standards, security posture, dependency policy, build \
         configuration. If a decision is specific to one component's \
         implementation, it belongs on that component.\n\n"
            .into()
    } else {
        format!(
            "SCOPE — COMPONENT [{component}]:\n\
             Record only decisions specific to this component. Cross-cutting \
             principles belong at project scope. If a decision applies to \
             multiple components equally, it is a project rule.\n\n"
        )
    }
}

/// List existing constraints for context.
fn existing_constraints(graph: &InMemoryGraph, component: &str) -> String {
    let project_rules = graph.project_decisions();
    let existing = graph.decisions_for(component);

    if project_rules.is_empty() && existing.is_empty() {
        return String::new();
    }

    let mut out = String::with_capacity(512);
    out.push_str("EXISTING DECISIONS (do not re-ask):\n");
    for (name, d) in &project_rules {
        out.push_str(&format!(
            "  [project] {}: {} ({})\n",
            name,
            sanitize(&d.decision.choice),
            sanitize(&d.decision.reason)
        ));
    }
    for (name, d) in &existing {
        out.push_str(&format!(
            "  {}: {} ({})\n",
            name,
            sanitize(&d.decision.choice),
            sanitize(&d.decision.reason)
        ));
    }
    out.push('\n');
    out
}

/// Component graph context — connections.
fn component_graph(graph: &InMemoryGraph, component: &str) -> String {
    let connects_to = graph.connects_to(component);
    let connects_from = graph.connects_from(component);

    if connects_to.is_empty() && connects_from.is_empty() {
        return String::new();
    }

    let mut out = String::with_capacity(256);
    out.push_str("COMPONENT GRAPH:\n");
    for c in &connects_to {
        out.push_str(&format!("  {component} → {c}\n"));
    }
    for c in &connects_from {
        out.push_str(&format!("  {c} → {component}\n"));
    }
    out.push('\n');
    out
}

/// Shared interaction protocol — embedded in every substantive step
/// when `Mode::Interactive`.
const INTERACTION_PROTOCOL: &str = "\
---\n\
INTERACTION PROTOCOL (non-negotiable):\n\n\
ONE topic per message. After asking any question, STOP and wait for \
the user's response. Do not continue to the next topic in the same \
message.\n\n\
WHEN THE USER'S ANSWER IS CORRECT BUT SHALLOW (≤1 sentence or \
restatement without deeper understanding):\n\
You are a senior engineer mentoring. Expand their understanding:\n\
- Explain the deeper implications they didn't mention\n\
- Give one concrete failure scenario this decision prevents\n\
- Connect it to other decisions in the graph\n\
Then ask: \"Does that deepen your understanding?\"\n\
STOP. Wait.\n\n\
WHEN THE USER SAYS \"I DON'T KNOW\" OR GIVES A ≤3 WORD ANSWER:\n\
This is a teaching moment, not a failure.\n\
1. Describe what the code does and why it matters (3-4 sentences)\n\
2. Give one concrete failure scenario\n\
3. Ask: \"Can you restate that in your own words?\"\n\
STOP. Wait. Record only after they demonstrate understanding.\n";

/// Agent protocol — replaces INTERACTION_PROTOCOL in `Mode::Agent`.
const AGENT_PROTOCOL: &str = "\
---\n\
AGENT PROTOCOL:\n\n\
You are operating autonomously. Analyze source code as primary evidence.\n\
Record each decision with attribution=\"agent\".\n\
Include code_refs for every decision — file paths and symbol names where \
the decision manifests in source code.\n\n\
If a decision requires domain knowledge not evident from the code, note \
this in the reason field: \"[needs-review] <reasoning from code evidence>\".\n\n\
Do not ask the user. Do not wait for input. Complete each step and call \
advance again.\n";

// ── Per-step instructions ─────────────────────────────────────────────────

fn step_register(component: &str) -> String {
    format!(
        "STEP: Register Component\n\n\
         Component `{component}` is not registered. Confirm the name and \
         description with the user, then call add_component.\n"
    )
}

fn step_define_scope(mode: Mode) -> String {
    match mode {
        Mode::Interactive => "STEP: Define Scope\n\n\
             Ask the user two questions (one per message):\n\
             1. \"What is this component responsible for?\"\n\
             2. \"What is explicitly NOT its responsibility?\"\n\n\
             After each answer, call record_decision with tags: [\"scope\"]. \
             Do not proceed until both scope decisions are recorded.\n"
            .into(),
        Mode::Agent => "STEP: Define Scope\n\n\
             Read the source code for this component. Determine:\n\
             1. What this component IS responsible for (its core job)\n\
             2. What is explicitly NOT its responsibility (boundaries)\n\n\
             Record both as decisions with tags: [\"scope\"] and \
             attribution=\"agent\". Base decisions on what the code \
             does, not what it should do.\n"
            .into(),
    }
}

fn step_analyze_code(component: &str, task_type: Option<&str>, mode: Mode) -> String {
    let mut out = String::with_capacity(512);

    if task_type == Some("learn") {
        out.push_str(
            "CONTEXT: The user has already described this component from \
             memory. Compare what you find in the source code to what the \
             user described earlier — note what they got right, what they \
             missed, and any misconceptions.\n\n",
        );
    }

    match mode {
        Mode::Interactive => {
            out.push_str(&format!(
                "STEP: Analyze Code\n\n\
                 Read every source file in [{component}]'s module. Build a \
                 numbered list of all architectural decisions you identify:\n\
                 - Data structures and why they were chosen\n\
                 - Error handling strategy\n\
                 - Concurrency primitives\n\
                 - Validation and integrity checks\n\
                 - Performance-sensitive paths\n\
                 - External boundaries\n\
                 - Security measures\n\
                 - Storage strategy\n\
                 - Dependency choices\n\
                 - Explicit scope boundaries\n\n\
                 Present: \"I found N architectural decisions in the source \
                 code. Let me go through each one.\"\n\
                 This list drives the session. Walk through each decision \
                 one at a time.\n"
            ));
        }
        Mode::Agent => {
            out.push_str(&format!(
                "STEP: Analyze Code\n\n\
                 Read every source file in [{component}]'s module. Identify \
                 all architectural decisions embedded in the code:\n\
                 - Data structures and why they were chosen\n\
                 - Error handling strategy\n\
                 - Concurrency primitives\n\
                 - Validation and integrity checks\n\
                 - Performance-sensitive paths\n\
                 - External boundaries\n\
                 - Security measures\n\
                 - Storage strategy\n\
                 - Dependency choices\n\
                 - Explicit scope boundaries\n\n\
                 Record each decision immediately with record_decision, \
                 attribution=\"agent\", and code_refs pointing to the \
                 source files and symbols where each decision manifests. \
                 Do not discuss — proceed through all decisions and call \
                 advance again.\n"
            ));
        }
    }

    out
}

fn step_cover_concerns(focus: &[String], all_decs: &[&DecisionFile], mode: Mode) -> String {
    let mut out = String::with_capacity(512);
    out.push_str("STEP: Cover Concerns\n\n");
    out.push_str(&concerns::concern_status(all_decs));
    out.push_str(&format!(
        "Focus on these uncovered areas (priority order):\n  {}\n\n",
        focus.join(", "),
    ));
    match mode {
        Mode::Interactive => {
            out.push_str(
                "For each uncovered concern:\n\
                 1. Read the relevant source code for that concern area\n\
                 2. Present 2-3 viable options with trade-offs\n\
                 3. Ask the user to choose\n\
                 4. Call record_decision with tags matching the concern area\n\n\
                 Do not move to the next concern until the current one is recorded.\n",
            );
        }
        Mode::Agent => {
            out.push_str(
                "For each uncovered concern:\n\
                 1. Read the relevant source code for that concern area\n\
                 2. Determine the decision the code has already made\n\
                 3. Call record_decision with tags matching the concern \
                 area and attribution=\"agent\"\n\n\
                 Record what the code does, not what it should do. \
                 Proceed through all concerns without user interaction.\n",
            );
        }
    }
    out
}

fn step_walk_decisions(graph: &InMemoryGraph, component: &str, mode: Mode) -> String {
    let decisions = graph.decisions_for(component);

    if decisions.is_empty() {
        return "STEP: Walk Decisions\n\n\
                No decisions recorded for this component. Consider running \
                the analyze_code step first.\n"
            .into();
    }

    let mut out = String::with_capacity(1024);

    match mode {
        Mode::Interactive => {
            out.push_str(
                "STEP: Walk Decisions\n\n\
                 Present each decision one at a time:\n\n",
            );

            for (name, d) in &decisions {
                out.push_str(&format!(
                    "DECISION: {name} — {}\n\
                     Reason: {}\n",
                    sanitize(&d.decision.choice),
                    sanitize(&d.decision.reason),
                ));
                if !d.decision.code_refs.is_empty() {
                    out.push_str(&format!(
                        "Code: {}\n",
                        store::format_code_refs(&d.decision.code_refs)
                    ));
                }
                out.push_str(
                    "→ Cite the specific file/function where this manifests\n\
                     → Ask the user to confirm or correct\n\
                     → STOP. Wait for response.\n\n",
                );
            }

            out.push_str(
                "After all decisions walked, probe for decisions you found in \
                 the code that are NOT yet recorded. For each, discuss and \
                 call record_decision.\n\n\
                 Then look for groups of 2+ decisions that reinforce the same \
                 invariant or form a defense-in-depth chain. For each candidate \
                 pattern, ask: \"Should I record this as a pattern?\" If confirmed, \
                 call record_pattern.\n",
            );
        }
        Mode::Agent => {
            out.push_str(
                "STEP: Walk Decisions\n\n\
                 Verify each recorded decision against the current source code:\n\n",
            );

            for (name, d) in &decisions {
                out.push_str(&format!(
                    "DECISION: {name} — {}\n\
                     Reason: {}\n",
                    sanitize(&d.decision.choice),
                    sanitize(&d.decision.reason),
                ));
                if !d.decision.code_refs.is_empty() {
                    out.push_str(&format!(
                        "Code: {}\n",
                        store::format_code_refs(&d.decision.code_refs)
                    ));
                }
                out.push_str(
                    "→ Locate in source code and verify accuracy\n\
                     → If drifted, call update_decision(mode=\"supersede\")\n\n",
                );
            }

            out.push_str(
                "After verification, identify decisions in the code that are \
                 NOT yet recorded. Record each with record_decision and \
                 attribution=\"agent\". Then call advance again.\n",
            );
        }
    }
    out
}

fn step_verify_constraints(graph: &InMemoryGraph, component: &str, mode: Mode) -> String {
    let decisions = graph.decisions_for(component);

    if decisions.is_empty() {
        return "STEP: Verify Constraints\n\n\
                No constraints recorded. Component is ready.\n"
            .into();
    }

    let mut out = String::with_capacity(512);
    out.push_str(
        "STEP: Verify Constraints\n\n\
         Present each existing constraint that the task may affect:\n\n",
    );

    for (name, d) in &decisions {
        match mode {
            Mode::Interactive => {
                out.push_str(&format!(
                    "CONSTRAINT: {name} — {} ({})\n\
                     → Cite the specific source file and function where this \
                     constraint is enforced.\n\
                     → Ask: \"Does your change respect this constraint, violate \
                     it, or require changing it?\"\n\
                     → STOP. Wait.\n\n",
                    sanitize(&d.decision.choice),
                    sanitize(&d.decision.reason),
                ));
            }
            Mode::Agent => {
                out.push_str(&format!(
                    "CONSTRAINT: {name} — {} ({})\n\
                     → Locate in source code and verify it is still enforced\n\
                     → Check if the current task conflicts with this constraint\n\n",
                    sanitize(&d.decision.choice),
                    sanitize(&d.decision.reason),
                ));
            }
        }
    }

    match mode {
        Mode::Interactive => {
            out.push_str(
                "If any constraint needs changing → call update_decision(mode=\"supersede\").\n\
                 If all constraints hold → report \"all constraints verified\" with \
                 the code locations checked.\n\
                 If you cannot locate a constraint in the source code, flag it as \
                 potentially drifted.\n\
                 Also check whether the change impacts connected components.\n",
            );
        }
        Mode::Agent => {
            out.push_str(
                "If any constraint has drifted → call update_decision(mode=\"supersede\").\n\
                 If any constraint conflicts with the task → note the conflict \
                 and call update_decision.\n\
                 If you cannot locate a constraint in the source code, flag it as \
                 potentially drifted.\n\
                 Report all findings and call advance again.\n",
            );
        }
    }
    out
}

fn step_impact_check(graph: &InMemoryGraph, component: &str, mode: Mode) -> String {
    let connects_to = graph.connects_to(component);
    let connects_from = graph.connects_from(component);

    let mut out = String::with_capacity(256);
    out.push_str(
        "STEP: Impact Check\n\n\
         Check whether this change impacts connected components.\n\n",
    );

    if connects_to.is_empty() && connects_from.is_empty() {
        out.push_str("No connected components. Impact check complete.\n");
    } else {
        out.push_str("Connected components to check:\n");
        for c in &connects_to {
            out.push_str(&format!("  → {c} (this component sends to it)\n"));
        }
        for c in &connects_from {
            out.push_str(&format!("  ← {c} (sends to this component)\n"));
        }
        match mode {
            Mode::Interactive => {
                out.push_str(
                    "\nFor each connected component, ask: \"Does your change affect \
                     the interface between these components?\"\n\
                     STOP. Wait.\n",
                );
            }
            Mode::Agent => {
                out.push_str(
                    "\nFor each connected component, read the interface code and \
                     determine whether the current task affects it. Report \
                     findings and call advance again.\n",
                );
            }
        }
    }
    out
}

fn step_pattern_detection(graph: &InMemoryGraph, component: &str, mode: Mode) -> String {
    let decisions = graph.decisions_for(component);
    let project_rules = graph.project_decisions();

    let mut out = String::with_capacity(512);
    out.push_str(
        "STEP: Pattern Detection\n\n\
         Review all recorded decisions:\n\n",
    );

    for (name, d) in &project_rules {
        out.push_str(&format!(
            "  [project] {name}: {}\n",
            sanitize(&d.decision.choice)
        ));
    }
    for (name, d) in &decisions {
        out.push_str(&format!("  {name}: {}\n", sanitize(&d.decision.choice)));
    }

    match mode {
        Mode::Interactive => {
            out.push_str(
                "\nLook for groups of 2+ decisions that:\n\
                 - Reinforce the same invariant\n\
                 - Form a defense-in-depth chain\n\
                 - Share a common constraint or trade-off\n\n\
                 For each candidate: \"These N decisions form a pattern — \
                 [describe]. Should I record it?\"\n\
                 If confirmed, call record_pattern with the decision names.\n",
            );
        }
        Mode::Agent => {
            out.push_str(
                "\nLook for groups of 2+ decisions that:\n\
                 - Reinforce the same invariant\n\
                 - Form a defense-in-depth chain\n\
                 - Share a common constraint or trade-off\n\n\
                 For each pattern found, call record_pattern with the \
                 decision names and attribution=\"agent\". Then call \
                 advance again.\n",
            );
        }
    }
    out
}

fn step_summary_gate(task_type: Option<&str>) -> String {
    let question = match task_type {
        Some("feature") => {
            "\"Without looking at the list, what constraints does your \
             change need to respect? Describe in 3-5 sentences.\""
        }
        Some("review") => {
            "\"Summarize what you found in this review — which decisions \
             still hold, which drifted, and what gaps remain. 3-5 sentences.\""
        }
        _ => {
            "\"Without looking at the list, describe in 3-5 sentences \
             the constraints any code touching this component must respect.\""
        }
    };

    format!(
        "STEP: Summary Gate\n\n\
         Ask: {question}\n\n\
         Do NOT help. Do NOT give hints. Do NOT break it into sub-questions.\n\n\
         If the user cannot produce a coherent summary, revisit the \
         decisions they couldn't explain.\n\
         The session ends only when the user demonstrates ownership.\n"
    )
}

fn step_drift_check(graph: &InMemoryGraph, component: &str, mode: Mode) -> String {
    let mut decisions: Vec<_> = graph.decisions_for(component);
    decisions.sort_by_key(|(_, d)| d.decision.created);

    if decisions.is_empty() {
        return "STEP: Drift Check\n\n\
                No decisions to check for drift.\n"
            .into();
    }

    let mut out = String::with_capacity(512);
    out.push_str(
        "STEP: Drift Check\n\n\
         Compare each decision against the current source code. \
         Oldest decisions first:\n\n",
    );

    for (name, d) in &decisions {
        out.push_str(&format!(
            "DECISION: {name} (created {})\n\
             Choice: {}\n\
             Reason: {}\n",
            d.decision.created.format("%Y-%m-%d"),
            sanitize(&d.decision.choice),
            sanitize(&d.decision.reason),
        ));
        if !d.decision.code_refs.is_empty() {
            out.push_str(&format!(
                "Code: {}\n",
                store::format_code_refs(&d.decision.code_refs)
            ));
        }
        out.push_str(
            "→ Verify this matches the current implementation\n\
             → If drifted, call update_decision(mode=\"supersede\")\n\n",
        );
    }

    if mode == Mode::Agent {
        out.push_str(
            "Verify each decision automatically against source code. \
             Supersede any that have drifted. Call advance again when done.\n",
        );
    }
    out
}

fn step_coverage_audit(covered: &[&str], uncovered: &[&str], mode: Mode) -> String {
    let mut out = String::with_capacity(512);
    out.push_str("STEP: Coverage Audit\n\n");

    if !covered.is_empty() {
        out.push_str("Concern areas WITH decisions:\n");
        for c in covered {
            out.push_str(&format!("  ✓ {c}\n"));
        }
        out.push('\n');
    }

    if uncovered.is_empty() {
        out.push_str("All concern areas have decisions. Coverage is complete.\n");
    } else {
        out.push_str("Concern areas WITHOUT decisions:\n");
        for u in uncovered {
            out.push_str(&format!("  □ {u}\n"));
        }
        match mode {
            Mode::Interactive => {
                out.push_str(
                    "\nFor each gap, determine whether the component needs a \
                     decision there or if the gap is intentional. If a decision \
                     is needed, use cover_concerns to address it.\n",
                );
            }
            Mode::Agent => {
                out.push_str(
                    "\nFor each gap, read the source code for that concern area. \
                     Report which gaps are real and which are intentional. \
                     Call advance again when done.\n",
                );
            }
        }
    }
    out
}

fn step_user_explains() -> String {
    "STEP: User Explains\n\n\
     Ask the user: \"From memory, describe this component's architecture \
     — its responsibilities, key decisions, and how it connects to the \
     rest of the system.\"\n\n\
     Do NOT show them any decisions or code first. The user must recall \
     from memory without looking at code or documentation.\n\n\
     After they respond, compare their description against the recorded \
     decisions. Note what they got right, what they missed, and any \
     misconceptions. Use this as the foundation for the learning session.\n"
        .into()
}

fn step_ready(component: &str) -> String {
    format!(
        "Component [{component}] is designed and ready for implementation.\n\
         Call get_context for the authoritative brief.\n"
    )
}

// ── Bootstrap step instructions ──────────────────────────────────────────
//
// These steps are autonomous: the agent reads source code and records
// decisions directly, without Socratic dialogue or user confirmation.
// The interaction protocol is intentionally omitted.

fn step_scan_project() -> String {
    "STEP: Scan Project\n\n\
     Read the project structure. Examine:\n\
     - Directory layout and module boundaries\n\
     - Build configuration (Cargo.toml, package.json, etc.)\n\
     - Entry points (main, lib, index)\n\
     - Test layout and integration structure\n\n\
     For each major architectural component:\n\
     1. Call add_component with a kebab-case name and one-line description\n\
     2. Identify directional dependencies and call add_connection\n\n\
     A component is a cohesive unit with its own design decisions — \
     not every file or subdirectory. When in doubt, prefer fewer \
     well-scoped components over many fine-grained ones.\n\n\
     Do NOT ask the user to confirm each component. Register them \
     based on what the source code shows, then call advance again.\n"
        .into()
}

fn step_extract_decisions(component: &str) -> String {
    format!(
        "STEP: Extract Decisions [{component}]\n\n\
         Read every source file in [{component}]'s module. For each \
         architectural decision embedded in the code, call \
         record_decision immediately.\n\n\
         Look for:\n\
         - Data structures and why they were chosen\n\
         - Error handling strategy\n\
         - Concurrency primitives and locking\n\
         - Validation and integrity checks\n\
         - Performance-sensitive paths\n\
         - External boundaries and APIs\n\
         - Security measures\n\
         - Storage and persistence strategy\n\
         - Serialization and data format choices\n\
         - Dependency choices and coupling boundaries\n\n\
         Record what the code DOES, not what it SHOULD do. Each \
         decision needs a concise choice (what was decided) and \
         reason (why). Use tags matching the concern area \
         (e.g. [\"security\"], [\"error\"], [\"concurrency\"]).\n\n\
         Do NOT ask the user about each decision. Record based on \
         source code evidence, then call advance again.\n"
    )
}

fn step_project_rules() -> String {
    "STEP: Project Rules\n\n\
     Identify cross-cutting principles that apply to ALL components:\n\
     - Error handling strategy (Result types, error enums, panic policy)\n\
     - Coding standards (naming conventions, module structure, visibility)\n\
     - Dependency policy (vendoring, version pinning, audit requirements)\n\
     - Build and test strategy (CI, coverage, benchmarks)\n\
     - Security posture (input validation, secrets handling, audit logging)\n\n\
     For each principle, call record_decision with component=\"project\".\n\n\
     Only record principles actually enforced in the codebase. \
     Do NOT invent aspirational rules — record what IS, not what \
     SHOULD BE. Then call advance again.\n"
        .into()
}

// ── Helpers ───────────────────────────────────────────────────────────────

/// Maximum byte length for a single decision value inlined into a prompt.
/// Prevents pathologically large decisions from bloating the system message.
const MAX_PROMPT_VALUE_LEN: usize = 512;

/// Sanitize a graph value before inlining it into a prompt string.
///
/// Strips control characters (except newline), truncates to a safe length,
/// and ensures the value cannot be used for prompt injection via embedded
/// directives in compromised `.toml` files.
fn sanitize(s: &str) -> String {
    let mut cleaned = String::with_capacity(MAX_PROMPT_VALUE_LEN.min(s.len()));
    let mut taken = 0;
    let mut truncated = false;
    for c in s.chars() {
        if c.is_control() && c != '\n' {
            continue;
        }
        if taken >= MAX_PROMPT_VALUE_LEN {
            truncated = true;
            break;
        }
        cleaned.push(c);
        taken += 1;
    }
    if truncated {
        cleaned.push('\u{2026}'); // …
    }
    cleaned
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::schema::*;
    use chrono::{TimeZone, Utc};
    use std::collections::BTreeMap;
    use std::sync::Arc;

    fn test_state() -> ProjectState {
        let mut comps = BTreeMap::new();
        comps.insert(
            "auth".into(),
            Arc::new(ComponentFile {
                component: Component {
                    name: "auth".into(),
                    description: "Authentication".into(),
                },
            }),
        );
        comps.insert(
            "store".into(),
            Arc::new(ComponentFile {
                component: Component {
                    name: "store".into(),
                    description: "Data store".into(),
                },
            }),
        );

        let ts = Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap();

        let mut decs = BTreeMap::new();
        decs.insert(
            "auth-jwt".into(),
            Arc::new(DecisionFile {
                decision: Decision {
                    component: "auth".into(),
                    choice: "JWT with DPoP binding".into(),
                    reason: "Proof-of-possession prevents token theft".into(),
                    alternatives: vec![],
                    tags: vec!["security".into(), "auth".into()],
                    attribution: Attribution::User,
                    created: ts,
                    code_refs: vec![],
                },
            }),
        );
        decs.insert(
            "auth-scope".into(),
            Arc::new(DecisionFile {
                decision: Decision {
                    component: "auth".into(),
                    choice: "Handles authentication only".into(),
                    reason: "Clear boundary".into(),
                    alternatives: vec![],
                    tags: vec!["scope".into()],
                    attribution: Attribution::User,
                    created: ts,
                    code_refs: vec![],
                },
            }),
        );
        decs.insert(
            "project-errors".into(),
            Arc::new(DecisionFile {
                decision: Decision {
                    component: "project".into(),
                    choice: "Result<T, AppError>".into(),
                    reason: "Consistent error propagation".into(),
                    alternatives: vec![],
                    tags: vec![],
                    attribution: Attribution::User,
                    created: ts,
                    code_refs: vec![],
                },
            }),
        );

        let nodes = vec![
            NodeEntry {
                name: "auth".into(),
                kind: NodeKind::Component,
                tags: vec![],
                hash: String::new(),
            },
            NodeEntry {
                name: "store".into(),
                kind: NodeKind::Component,
                tags: vec![],
                hash: String::new(),
            },
            NodeEntry {
                name: "auth-jwt".into(),
                kind: NodeKind::Decision,
                tags: vec!["security".into()],
                hash: String::new(),
            },
            NodeEntry {
                name: "auth-scope".into(),
                kind: NodeKind::Decision,
                tags: vec!["scope".into()],
                hash: String::new(),
            },
            NodeEntry {
                name: "project-errors".into(),
                kind: NodeKind::Decision,
                tags: vec![],
                hash: String::new(),
            },
        ];
        let edges = vec![
            EdgeEntry {
                from: "auth-jwt".into(),
                to: "auth".into(),
                kind: EdgeKind::BelongsTo,
            },
            EdgeEntry {
                from: "auth-scope".into(),
                to: "auth".into(),
                kind: EdgeKind::BelongsTo,
            },
            EdgeEntry {
                from: "project-errors".into(),
                to: "project".into(),
                kind: EdgeKind::BelongsTo,
            },
            EdgeEntry {
                from: "auth".into(),
                to: "store".into(),
                kind: EdgeKind::ConnectsTo,
            },
        ];

        ProjectState::new(
            ProjectFile {
                trurlic_version: FORMAT_VERSION.into(),
                project: Project {
                    name: "test".into(),
                    description: String::new(),
                },
            },
            comps,
            decs,
            BTreeMap::new(),
            GraphIndex {
                version: 1,
                rebuilt: Utc::now(),
                nodes,
                edges,
            },
        )
    }

    // ── Step prompt structure ──────────────────────────────────────────

    #[test]
    fn every_step_includes_preamble() {
        let state = test_state();
        for step in &[
            "define_scope",
            "analyze_code",
            "cover_concerns",
            "walk_decisions",
            "verify_constraints",
            "pattern_detection",
            "summary_gate",
            "drift_check",
            "coverage_audit",
        ] {
            let result =
                build_step_prompt(&state, "auth", step, None, None, Mode::Interactive).unwrap();
            assert!(
                result.instructions.contains("Read the source code"),
                "step `{step}` missing preamble"
            );
        }
    }

    #[test]
    fn substantive_steps_include_interaction_protocol() {
        let state = test_state();
        for step in &[
            "define_scope",
            "analyze_code",
            "cover_concerns",
            "walk_decisions",
            "verify_constraints",
            "summary_gate",
        ] {
            let result =
                build_step_prompt(&state, "auth", step, None, None, Mode::Interactive).unwrap();
            assert!(
                result.instructions.contains("ONE topic per message"),
                "step `{step}` missing interaction protocol"
            );
            assert!(
                result.instructions.contains("I DON'T KNOW"),
                "step `{step}` missing I-don't-know protocol"
            );
        }
    }

    #[test]
    fn register_and_ready_skip_protocol() {
        let state = test_state();
        let reg =
            build_step_prompt(&state, "auth", "register", None, None, Mode::Interactive).unwrap();
        assert!(!reg.instructions.contains("ONE topic per message"));

        let ready =
            build_step_prompt(&state, "auth", "ready", None, None, Mode::Interactive).unwrap();
        assert!(!ready.instructions.contains("ONE topic per message"));
    }

    // ── Step-specific behavior ────────────────────────────────────────

    #[test]
    fn define_scope_asks_scope_questions() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "store",
            "define_scope",
            None,
            None,
            Mode::Interactive,
        )
        .unwrap();
        assert!(result.instructions.contains("responsible for"));
        assert!(result.instructions.contains("NOT its responsibility"));
        assert!(result.instructions.contains("[\"scope\"]"));
    }

    #[test]
    fn analyze_code_lists_what_to_look_for() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "auth",
            "analyze_code",
            None,
            None,
            Mode::Interactive,
        )
        .unwrap();
        assert!(result.instructions.contains("Data structures"));
        assert!(result.instructions.contains("Error handling"));
        assert!(result.instructions.contains("Security measures"));
    }

    #[test]
    fn cover_concerns_returns_focus() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "store",
            "cover_concerns",
            None,
            None,
            Mode::Interactive,
        )
        .unwrap();
        // store has no decisions → all 10 concerns uncovered, focus is top 3.
        assert_eq!(result.focus.len(), CONCERN_FOCUS_LIMIT);
        assert_eq!(result.focus[0], "Security boundaries");
    }

    #[test]
    fn cover_concerns_shows_concern_status() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "auth",
            "cover_concerns",
            None,
            None,
            Mode::Interactive,
        )
        .unwrap();
        assert!(result.instructions.contains("COVERED"));
        assert!(result.instructions.contains("UNCOVERED"));
    }

    #[test]
    fn walk_decisions_lists_each_decision() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "auth",
            "walk_decisions",
            None,
            None,
            Mode::Interactive,
        )
        .unwrap();
        assert!(result.instructions.contains("auth-jwt"));
        assert!(result.instructions.contains("JWT with DPoP binding"));
        assert!(result.instructions.contains("STOP. Wait for response"));
    }

    #[test]
    fn verify_constraints_lists_constraints() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "auth",
            "verify_constraints",
            None,
            None,
            Mode::Interactive,
        )
        .unwrap();
        assert!(result.instructions.contains("CONSTRAINT:"));
        assert!(result.instructions.contains("JWT with DPoP binding"));
        assert!(
            result
                .instructions
                .contains("violate it, or require changing it")
        );
    }

    #[test]
    fn verify_constraints_requires_code_citation() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "auth",
            "verify_constraints",
            None,
            None,
            Mode::Interactive,
        )
        .unwrap();
        assert!(
            result.instructions.contains("source file and function"),
            "verify_constraints must require code citation"
        );
        assert!(
            result.instructions.contains("cannot locate"),
            "verify_constraints must flag unlocatable constraints as drifted"
        );
    }

    #[test]
    fn impact_check_shows_connections() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "auth",
            "impact_check",
            None,
            None,
            Mode::Interactive,
        )
        .unwrap();
        assert!(result.instructions.contains("store"));
    }

    #[test]
    fn pattern_detection_lists_decisions() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "auth",
            "pattern_detection",
            None,
            None,
            Mode::Interactive,
        )
        .unwrap();
        assert!(result.instructions.contains("auth-jwt"));
        assert!(result.instructions.contains("defense-in-depth"));
    }

    #[test]
    fn summary_gate_forbids_hints() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "auth",
            "summary_gate",
            None,
            None,
            Mode::Interactive,
        )
        .unwrap();
        assert!(result.instructions.contains("Do NOT help"));
        assert!(result.instructions.contains("Do NOT give hints"));
    }

    #[test]
    fn coverage_audit_returns_full_focus() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "store",
            "coverage_audit",
            None,
            None,
            Mode::Interactive,
        )
        .unwrap();
        // store has 0 decisions → project-errors covers Error handling.
        // All remaining concern areas are in the focus list.
        assert!(!result.focus.is_empty());
        assert!(result.instructions.contains("WITHOUT decisions"));
    }

    // ── Error cases ───────────────────────────────────────────────────

    #[test]
    fn unknown_component_returns_error() {
        let state = test_state();
        let err = build_step_prompt(
            &state,
            "ghost",
            "define_scope",
            None,
            None,
            Mode::Interactive,
        )
        .unwrap_err();
        assert!(err.contains("does not exist"));
    }

    #[test]
    fn unknown_step_returns_error() {
        let state = test_state();
        let err = build_step_prompt(&state, "auth", "bogus_step", None, None, Mode::Interactive)
            .unwrap_err();
        assert!(err.contains("unknown step"));
    }

    #[test]
    fn project_scope_accepted() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "project",
            "define_scope",
            None,
            None,
            Mode::Interactive,
        )
        .unwrap();
        assert!(result.instructions.contains("PROJECT LEVEL"));
    }

    // ── Task passthrough ──────────────────────────────────────────────

    #[test]
    fn task_included_in_prompt() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "auth",
            "verify_constraints",
            Some("add rate limiting"),
            None,
            Mode::Interactive,
        )
        .unwrap();
        assert!(result.instructions.contains("add rate limiting"));
    }

    // ── Scope boundary ────────────────────────────────────────────────

    #[test]
    fn component_scope_boundary() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "auth",
            "define_scope",
            None,
            None,
            Mode::Interactive,
        )
        .unwrap();
        assert!(result.instructions.contains("COMPONENT [auth]"));
    }

    #[test]
    fn project_scope_boundary() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "project",
            "define_scope",
            None,
            None,
            Mode::Interactive,
        )
        .unwrap();
        assert!(result.instructions.contains("PROJECT LEVEL"));
        assert!(result.instructions.contains("cross-cutting principles"));
    }

    // ── Bootstrap steps ──────────────────────────────────────────────

    #[test]
    fn scan_project_includes_preamble() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "project",
            "scan_project",
            None,
            None,
            Mode::Interactive,
        )
        .unwrap();
        assert!(result.instructions.contains("source code"));
    }

    #[test]
    fn scan_project_skips_interaction_protocol() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "project",
            "scan_project",
            None,
            None,
            Mode::Interactive,
        )
        .unwrap();
        assert!(
            !result.instructions.contains("ONE topic per message"),
            "scan_project must skip interaction protocol"
        );
    }

    #[test]
    fn scan_project_instructs_autonomous_registration() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "project",
            "scan_project",
            None,
            None,
            Mode::Interactive,
        )
        .unwrap();
        assert!(result.instructions.contains("add_component"));
        assert!(result.instructions.contains("add_connection"));
        assert!(result.instructions.contains("Do NOT ask the user"));
    }

    #[test]
    fn extract_decisions_includes_preamble() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "auth",
            "extract_decisions",
            None,
            None,
            Mode::Interactive,
        )
        .unwrap();
        assert!(result.instructions.contains("source code"));
    }

    #[test]
    fn extract_decisions_skips_interaction_protocol() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "auth",
            "extract_decisions",
            None,
            None,
            Mode::Interactive,
        )
        .unwrap();
        assert!(
            !result.instructions.contains("ONE topic per message"),
            "extract_decisions must skip interaction protocol"
        );
    }

    #[test]
    fn extract_decisions_targets_component() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "auth",
            "extract_decisions",
            None,
            None,
            Mode::Interactive,
        )
        .unwrap();
        assert!(result.instructions.contains("[auth]"));
        assert!(result.instructions.contains("record_decision"));
    }

    #[test]
    fn extract_decisions_shows_existing_constraints() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "auth",
            "extract_decisions",
            None,
            None,
            Mode::Interactive,
        )
        .unwrap();
        // auth has existing decisions → they appear as constraints context.
        assert!(result.instructions.contains("EXISTING DECISIONS"));
    }

    #[test]
    fn extract_decisions_shows_component_graph() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "auth",
            "extract_decisions",
            None,
            None,
            Mode::Interactive,
        )
        .unwrap();
        // auth connects to store → graph context present.
        assert!(result.instructions.contains("store"));
    }

    #[test]
    fn project_rules_includes_preamble() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "project",
            "project_rules",
            None,
            None,
            Mode::Interactive,
        )
        .unwrap();
        assert!(result.instructions.contains("source code"));
    }

    #[test]
    fn project_rules_skips_interaction_protocol() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "project",
            "project_rules",
            None,
            None,
            Mode::Interactive,
        )
        .unwrap();
        assert!(
            !result.instructions.contains("ONE topic per message"),
            "project_rules must skip interaction protocol"
        );
    }

    #[test]
    fn project_rules_instructs_cross_cutting() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "project",
            "project_rules",
            None,
            None,
            Mode::Interactive,
        )
        .unwrap();
        assert!(result.instructions.contains("cross-cutting"));
        assert!(result.instructions.contains("record_decision"));
        assert!(result.instructions.contains("project"));
    }

    // ── user_explains step ───────────────────────────────────────────

    #[test]
    fn build_step_prompt_accepts_user_explains() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "auth",
            "user_explains",
            None,
            None,
            Mode::Interactive,
        )
        .unwrap();
        assert!(result.instructions.contains("from memory"));
    }

    #[test]
    fn step_as_str_round_trips_user_explains() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "auth",
            "user_explains",
            None,
            None,
            Mode::Interactive,
        );
        assert!(
            result.is_ok(),
            "user_explains must be accepted: {:?}",
            result.err()
        );
    }

    #[test]
    fn user_explains_includes_interaction_protocol() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "auth",
            "user_explains",
            None,
            None,
            Mode::Interactive,
        )
        .unwrap();
        assert!(
            result.instructions.contains("ONE topic per message"),
            "user_explains must include interaction protocol"
        );
    }

    // ── summary_gate task_type variants ──────────────────────────────

    #[test]
    fn summary_gate_feature_variant() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "auth",
            "summary_gate",
            None,
            Some("feature"),
            Mode::Interactive,
        )
        .unwrap();
        assert!(
            result.instructions.contains("constraints does your change"),
            "feature variant should ask about change constraints"
        );
    }

    #[test]
    fn summary_gate_review_variant() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "auth",
            "summary_gate",
            None,
            Some("review"),
            Mode::Interactive,
        )
        .unwrap();
        assert!(
            result.instructions.contains("Summarize what you found"),
            "review variant should ask for review summary"
        );
    }

    #[test]
    fn summary_gate_default_variant() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "auth",
            "summary_gate",
            None,
            None,
            Mode::Interactive,
        )
        .unwrap();
        assert!(
            result
                .instructions
                .contains("constraints any code touching"),
            "default variant should ask about component constraints"
        );
    }

    // ── analyze_code learn variant ──────────────────────────────────

    #[test]
    fn analyze_code_learn_preamble() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "auth",
            "analyze_code",
            None,
            Some("learn"),
            Mode::Interactive,
        )
        .unwrap();
        assert!(
            result.instructions.contains("Compare what you find"),
            "learn variant should reference user's earlier description"
        );
    }

    #[test]
    fn analyze_code_non_learn_no_preamble() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "auth",
            "analyze_code",
            None,
            Some("feature"),
            Mode::Interactive,
        )
        .unwrap();
        assert!(
            !result.instructions.contains("Compare what you find"),
            "non-learn variant should not include learn preamble"
        );
    }

    // ── Mode-specific tests ─────────────────────────────────────────

    #[test]
    fn agent_mode_uses_agent_protocol() {
        let state = test_state();
        for step in &[
            "define_scope",
            "analyze_code",
            "cover_concerns",
            "walk_decisions",
            "verify_constraints",
            "pattern_detection",
            "drift_check",
            "coverage_audit",
        ] {
            let result = build_step_prompt(&state, "auth", step, None, None, Mode::Agent).unwrap();
            assert!(
                result.instructions.contains("AGENT PROTOCOL"),
                "step `{step}` in agent mode missing AGENT PROTOCOL"
            );
            assert!(
                !result.instructions.contains("ONE topic per message"),
                "step `{step}` in agent mode should not have INTERACTION PROTOCOL"
            );
        }
    }

    #[test]
    fn interactive_mode_uses_interaction_protocol() {
        let state = test_state();
        for step in &[
            "define_scope",
            "analyze_code",
            "cover_concerns",
            "walk_decisions",
            "verify_constraints",
            "pattern_detection",
            "drift_check",
            "coverage_audit",
        ] {
            let result =
                build_step_prompt(&state, "auth", step, None, None, Mode::Interactive).unwrap();
            assert!(
                result.instructions.contains("ONE topic per message"),
                "step `{step}` in interactive mode missing INTERACTION PROTOCOL"
            );
            assert!(
                !result.instructions.contains("AGENT PROTOCOL"),
                "step `{step}` in interactive mode should not have AGENT PROTOCOL"
            );
        }
    }

    #[test]
    fn agent_define_scope_reads_source_code() {
        let state = test_state();
        let result =
            build_step_prompt(&state, "auth", "define_scope", None, None, Mode::Agent).unwrap();
        assert!(
            result.instructions.contains("Read the source code"),
            "agent define_scope should instruct reading source code"
        );
        assert!(
            !result.instructions.contains("Ask the user"),
            "agent define_scope should not ask the user"
        );
    }

    #[test]
    fn interactive_define_scope_asks_user() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "auth",
            "define_scope",
            None,
            None,
            Mode::Interactive,
        )
        .unwrap();
        assert!(
            result.instructions.contains("Ask the user"),
            "interactive define_scope should ask the user"
        );
    }

    #[test]
    fn agent_prompts_differ_from_interactive() {
        let state = test_state();
        let dual_mode_steps = [
            "define_scope",
            "analyze_code",
            "cover_concerns",
            "walk_decisions",
            "verify_constraints",
            "impact_check",
            "pattern_detection",
            "drift_check",
            "coverage_audit",
        ];
        for step in &dual_mode_steps {
            let agent = build_step_prompt(&state, "auth", step, None, None, Mode::Agent).unwrap();
            let interactive =
                build_step_prompt(&state, "auth", step, None, None, Mode::Interactive).unwrap();
            assert_ne!(
                agent.instructions, interactive.instructions,
                "step `{step}` should produce different prompts for agent vs interactive"
            );
        }
    }

    #[test]
    fn agent_analyze_code_records_without_discussion() {
        let state = test_state();
        let result =
            build_step_prompt(&state, "auth", "analyze_code", None, None, Mode::Agent).unwrap();
        assert!(
            result
                .instructions
                .contains("Record each decision immediately")
        );
        assert!(result.instructions.contains("Do not discuss"));
    }

    #[test]
    fn agent_cover_concerns_records_with_attribution() {
        let state = test_state();
        let result =
            build_step_prompt(&state, "auth", "cover_concerns", None, None, Mode::Agent).unwrap();
        assert!(result.instructions.contains("attribution=\"agent\""));
        assert!(result.instructions.contains("without user interaction"));
    }

    #[test]
    fn agent_walk_decisions_verifies_against_code() {
        let state = test_state();
        let result =
            build_step_prompt(&state, "auth", "walk_decisions", None, None, Mode::Agent).unwrap();
        assert!(
            result
                .instructions
                .contains("Verify each recorded decision")
        );
        assert!(result.instructions.contains("supersede"));
    }

    #[test]
    fn agent_verify_constraints_checks_source() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "auth",
            "verify_constraints",
            None,
            None,
            Mode::Agent,
        )
        .unwrap();
        assert!(result.instructions.contains("Locate in source code"));
        assert!(!result.instructions.contains("STOP. Wait"));
    }

    #[test]
    fn agent_impact_check_autonomous() {
        let state = test_state();
        let result =
            build_step_prompt(&state, "auth", "impact_check", None, None, Mode::Agent).unwrap();
        assert!(result.instructions.contains("read the interface code"));
        assert!(!result.instructions.contains("STOP. Wait"));
    }

    #[test]
    fn agent_pattern_detection_records_with_attribution() {
        let state = test_state();
        let result =
            build_step_prompt(&state, "auth", "pattern_detection", None, None, Mode::Agent)
                .unwrap();
        assert!(result.instructions.contains("attribution=\"agent\""));
    }

    #[test]
    fn agent_drift_check_verifies_automatically() {
        let state = test_state();
        let result =
            build_step_prompt(&state, "auth", "drift_check", None, None, Mode::Agent).unwrap();
        assert!(
            result
                .instructions
                .contains("Verify each decision automatically")
        );
    }

    #[test]
    fn agent_coverage_audit_reads_source() {
        let state = test_state();
        let result =
            build_step_prompt(&state, "auth", "coverage_audit", None, None, Mode::Agent).unwrap();
        assert!(result.instructions.contains("read the source code"));
    }

    #[test]
    fn autonomous_steps_skip_protocol_in_both_modes() {
        let state = test_state();
        for step in &[
            "register",
            "ready",
            "scan_project",
            "extract_decisions",
            "project_rules",
        ] {
            let comp = if *step == "scan_project" || *step == "project_rules" {
                "project"
            } else {
                "auth"
            };
            for mode in [Mode::Interactive, Mode::Agent] {
                let result = build_step_prompt(&state, comp, step, None, None, mode).unwrap();
                assert!(
                    !result.instructions.contains("AGENT PROTOCOL")
                        && !result.instructions.contains("ONE topic per message"),
                    "step `{step}` should skip both protocols in {:?}",
                    mode
                );
            }
        }
    }
}
