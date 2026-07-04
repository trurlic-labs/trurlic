//! Per-step prompt builders for the workflow engine.
//!
//! Each workflow step gets a focused prompt: 200-500 bytes of step-specific
//! instructions, sandwiched between a shared preamble (source code mandate)
//! and a shared protocol (interaction or agent, depending on mode).
//!
//! Prompts are transport-agnostic. The MCP tool `get_step_prompt` calls
//! `build_step_prompt` and combines the result with `get_context` output.

use chrono::{DateTime, Utc};

use crate::store::graph::InMemoryGraph;
use crate::store::schema::{Decision, DecisionFile};
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
/// (e.g. `design_check` varies by Feature vs Review vs NewComponent).
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
            | "design_check"
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
            out.push_str(&step_cover_concerns(&focus, &all_decs, task_type, mode));
        }
        "walk_decisions" => {
            out.push_str(&step_walk_decisions(graph, component, task_type, mode));
        }
        "verify_constraints" => {
            out.push_str(&step_verify_constraints(graph, component, task_type, mode));
        }
        "impact_check" => out.push_str(&step_impact_check(graph, component, task_type, mode)),
        "pattern_detection" => out.push_str(&step_pattern_detection(graph, component, mode)),
        "design_check" | "summary_gate" => out.push_str(&step_design_check(task_type)),
        "drift_check" => out.push_str(&step_drift_check(graph, component, mode)),
        "coverage_audit" => {
            focus = uncovered.iter().map(|s| (*s).to_string()).collect();
            out.push_str(&step_coverage_audit(&covered, &uncovered, mode));
        }
        "scan_project" => out.push_str(&step_scan_project()),
        "extract_decisions" => out.push_str(&step_extract_decisions(component)),
        "project_rules" => out.push_str(&step_project_rules()),
        "warm_up" | "user_explains" => out.push_str(&step_warm_up()),
        "ready" => out.push_str(&step_ready(component)),
        _ => {
            return Err(format!(
                "unknown step `{step}` — expected: register, define_scope, \
             analyze_code, cover_concerns, walk_decisions, verify_constraints, \
             impact_check, pattern_detection, design_check, drift_check, \
             coverage_audit, scan_project, extract_decisions, project_rules, \
             warm_up, ready"
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
INTERACTION PROTOCOL:\n\n\
You are a senior engineer in a design discussion with a peer. Your job \
is to help them think through decisions, not quiz them on facts.\n\n\
ONE topic per exchange. After posing a question, STOP and wait.\n\n\
HOW TO ASK:\n\
Probe trade-offs, not recall.\n\
  Good: \"Why this approach over X? What's the trade-off?\"\n\
  Good: \"What happens to this when traffic is 10x higher?\"\n\
  Good: \"I noticed the code does Z — was that deliberate?\"\n\
  Avoid: \"Can you explain what this component does?\"\n\
  Avoid: \"What does this NOT do?\"\n\n\
HOW TO DEEPEN:\n\
When the user gives a correct but surface-level answer, go deeper \
on what they said — don't repeat the question differently:\n\
- \"That's the what — what drove that choice? What did you reject?\"\n\
- Share a concrete scenario the code handles because of this decision\n\
- Connect to another decision: \"This interacts with [X] — have you \
thought about what happens when both are in play?\"\n\n\
HOW TO TEACH:\n\
When the user doesn't know something, that's the most valuable \
moment — not a failure.\n\
1. Walk through the specific code path (cite the file and function)\n\
2. Explain what problem it solves with a concrete scenario\n\
3. Ask a forward-looking question: \"Now that you see this, does \
this approach still make sense for where the project is heading?\"\n\
Don't ask them to restate what you just said.\n\n\
WHEN THE USER PUSHES BACK:\n\
Engage with their reasoning — pushback means they're thinking.\n\
- Present the counter-argument from the code's perspective\n\
- If they have a better idea: \"How would that change the implementation?\"\n\
- Record what they actually decide, not what the code currently does\n";

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
             Start with: \"In one sentence, what's this component's job?\"\n\
             STOP. Wait.\n\n\
             Based on their answer, probe the boundary:\n\
             - If scope sounds too broad: \"That sounds like it could include \
             [adjacent concern] — is that intentional, or does that belong \
             somewhere else?\"\n\
             - If scope is too narrow: \"What about [thing the code actually \
             does]? I see that in the source.\"\n\n\
             Record TWO decisions with tags [\"scope\"]:\n\
             1. What the component IS responsible for\n\
             2. What is explicitly NOT its responsibility\n\n\
             The boundary decision is the more important one — it prevents \
             scope creep. Spend more time on it.\n"
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

    match mode {
        Mode::Interactive => {
            if task_type == Some("learn") {
                out.push_str(
                    "CONTEXT: The user described this component in the warm-up. \
                     Now read the source code. Focus on:\n\
                     - Where their mental model matches the code (reinforce)\n\
                     - Gaps they didn't mention (learning opportunities)\n\
                     - Things they described differently from the code — frame \
                     as discussion: \"Interesting — the code actually does X here. \
                     What's your take on that?\"\n\n\
                     Frame discrepancies as discussion points, not corrections.\n\n",
                );
            }

            out.push_str(&format!(
                "STEP: Analyze Code\n\n\
                 Read every source file in [{component}]. Identify architectural \
                 decisions in the code:\n\
                 - Data structures and why they were chosen\n\
                 - Error handling strategy\n\
                 - Concurrency, performance-sensitive paths\n\
                 - Validation, integrity checks, security measures\n\
                 - External boundaries and dependency choices\n\n\
                 Walk through each one with the user. For each:\n\
                 - Share what the code does and why it matters\n\
                 - Ask: \"Does this match how you think about it?\"\n\
                 - STOP. Wait. Discuss. Then record_decision.\n\n\
                 Don't dump a numbered list. One decision at a time.\n"
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

fn step_cover_concerns(
    focus: &[String],
    all_decs: &[&DecisionFile],
    task_type: Option<&str>,
    mode: Mode,
) -> String {
    let mut out = String::with_capacity(512);
    out.push_str("STEP: Cover Concerns\n\n");
    out.push_str(&concerns::concern_status(all_decs));
    match mode {
        Mode::Interactive => match task_type {
            Some("feature") => {
                out.push_str(&format!(
                    "Uncovered concerns relevant to this feature: {}\n\n\
                         For each concern that the feature touches:\n\
                         1. Read the code the feature will change\n\
                         2. Ask: \"How does [concern area] affect your feature? \
                         Have you thought about this?\"\n\
                         3. STOP. Wait.\n\
                         4. If they have a plan — validate or challenge it\n\
                         5. If they haven't thought about it — share what the code \
                         currently does, ask if the feature needs to change that\n\
                         6. Record the decision together\n\n\
                         Focus only on concerns the feature actually impacts. \
                         Don't force discussion on irrelevant concern areas.\n",
                    focus.join(", "),
                ));
            }
            _ => {
                out.push_str(&format!(
                    "Uncovered areas (priority order): {}\n\n\
                         For each uncovered concern:\n\
                         1. Read the relevant source code\n\
                         2. Ask: \"How are you thinking about [concern] for this \
                         component?\"\n\
                         3. STOP. Wait.\n\
                         4. If they have a clear opinion — validate or challenge \
                         with code evidence. Discuss trade-offs.\n\
                         5. If they haven't thought about it — share what the code \
                         does (or doesn't do), ask if that's intentional\n\
                         6. If they're stuck — then offer 2-3 options as a starting \
                         point\n\
                         7. Arrive at a decision together. Record it.\n\n\
                         Start with their thinking, not a menu of options.\n",
                    focus.join(", "),
                ));
            }
        },
        Mode::Agent => {
            out.push_str(&format!(
                "Focus on these uncovered areas (priority order):\n  {}\n\n",
                focus.join(", "),
            ));
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

fn step_walk_decisions(
    graph: &InMemoryGraph,
    component: &str,
    task_type: Option<&str>,
    mode: Mode,
) -> String {
    let decisions = graph.decisions_for(component);

    if decisions.is_empty() {
        return "STEP: Walk Decisions\n\nNo decisions recorded.\n".into();
    }

    let mut out = String::with_capacity(1024);

    match mode {
        Mode::Interactive => {
            match task_type {
                Some("review") => {
                    out.push_str(
                        "STEP: Walk Decisions\n\n\
                         Review each decision against the current code. Focus on \
                         freshness \u{2014} has the code evolved past this decision?\n\n",
                    );
                }
                Some("learn") => {
                    out.push_str(
                        "STEP: Walk Decisions\n\n\
                         Walk through each decision as a design discussion. The \
                         goal is understanding why, not confirming what.\n\n",
                    );
                }
                _ => {
                    out.push_str(
                        "STEP: Walk Decisions\n\n\
                         Discuss each decision \u{2014} don\u{2019}t just present and ask for \
                         confirmation.\n\n",
                    );
                }
            }

            for (name, d) in &decisions {
                let code_line = format_code_refs_line(&d.decision);
                let history_note = if !d.decision.history.is_empty() {
                    format!(
                        "Revised {} time(s) \u{2014} original: \"{}\"\n",
                        d.decision.history.len(),
                        sanitize_short(&d.decision.history[0].choice, 60),
                    )
                } else {
                    String::new()
                };

                let question = match task_type {
                    Some("review") => {
                        format!(
                            "\u{2192} Read the code at these locations\n\
                             \u{2192} Ask: \"This decision is from {}. Does the code \
                             still match? Has anything drifted?\"\n",
                            d.decision.created.format("%Y-%m-%d"),
                        )
                    }
                    Some("learn") => "\u{2192} Read the code where this lives\n\
                         \u{2192} Ask: \"Why was this approach chosen over the \
                         alternatives? What\u{2019}s the trade-off?\"\n"
                        .into(),
                    _ => {
                        format!(
                            "\u{2192} Read the code where this lives\n\
                             \u{2192} Ask: \"This was decided because of {reason} \u{2014} is \
                             that still the right trade-off?\"\n",
                            reason = sanitize_short(&d.decision.reason, 60),
                        )
                    }
                };

                out.push_str(&format!(
                    "DECISION: {name}\n\
                     Choice: {choice}\n\
                     Reason: {reason}\n\
                     {code_line}\
                     {history_note}\
                     {question}\
                     \u{2192} STOP. Wait.\n\n",
                    choice = sanitize(&d.decision.choice),
                    reason = sanitize(&d.decision.reason),
                ));
            }

            match task_type {
                Some("review") => {
                    out.push_str(
                        "After walking all decisions, ask: \"Are there decisions \
                         in the code that should be recorded but aren\u{2019}t?\" Look \
                         for undocumented patterns.\n",
                    );
                }
                _ => {
                    out.push_str(
                        "After walking all decisions, ask: \"What\u{2019}s the one \
                         decision in this component you\u{2019}d change if you were \
                         starting over today?\" This surfaces latent design debt.\n\n\
                         Then check for undocumented decisions in the code. \
                         For each, discuss and record.\n",
                    );
                }
            }
        }
        Mode::Agent => {
            out.push_str(
                "STEP: Walk Decisions\n\n\
                 Verify each recorded decision against the current source code:\n\n",
            );

            for (name, d) in &decisions {
                out.push_str(&format!(
                    "DECISION: {name} \u{2014} {}\n\
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
                    "\u{2192} Locate in source code and verify accuracy\n\
                     \u{2192} If drifted, call update_decision(mode=\"revise\")\n\n",
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

fn step_verify_constraints(
    graph: &InMemoryGraph,
    component: &str,
    task_type: Option<&str>,
    mode: Mode,
) -> String {
    let decisions = graph.decisions_for(component);

    if decisions.is_empty() {
        return "STEP: Verify Constraints\n\n\
                No constraints recorded. Component is ready.\n"
            .into();
    }

    let mut out = String::with_capacity(512);

    match mode {
        Mode::Interactive => {
            match task_type {
                Some("fix") => {
                    out.push_str(
                        "STEP: Verify Constraints\n\n\
                         Start by understanding the fix:\n\
                         \"Tell me about this bug \u{2014} what's happening, what should \
                         happen, and what's your plan to fix it?\"\n\
                         STOP. Wait.\n\n\
                         Then check each constraint the fix might affect:\n\n",
                    );
                }
                _ => {
                    out.push_str(
                        "STEP: Verify Constraints\n\n\
                         Start by understanding the feature:\n\
                         \"Walk me through this feature \u{2014} what are you adding \
                         and which parts of the code will it touch?\"\n\
                         STOP. Wait.\n\n\
                         Based on their answer, check relevant constraints:\n\n",
                    );
                }
            }

            for (name, d) in &decisions {
                let code_line = format_code_refs_line(&d.decision);
                out.push_str(&format!(
                    "CONSTRAINT: {name} \u{2014} {choice}\n\
                     Reason: {reason}\n\
                     {code_line}\
                     \u{2192} Read the constraint's code location\n\
                     \u{2192} Ask: \"Does your change respect this, or does it need \
                     to change?\"\n\
                     \u{2192} If it needs to change, discuss why. Call \
                     update_decision(mode=\"revise\") if agreed.\n\
                     \u{2192} STOP. Wait.\n\n",
                    choice = sanitize(&d.decision.choice),
                    reason = sanitize(&d.decision.reason),
                ));
            }

            match task_type {
                Some("fix") => {
                    out.push_str(
                        "After checking constraints, ask: \"If this fix ships \
                         and causes a regression, what's the most likely thing \
                         to break and why?\"\n",
                    );
                }
                _ => {
                    out.push_str(
                        "After checking constraints, ask: \"Is there anything \
                         this feature needs that the current architecture doesn't \
                         support?\"\n\
                         If yes, discuss whether to adapt the architecture or \
                         the feature.\n",
                    );
                }
            }
        }
        Mode::Agent => {
            out.push_str(
                "STEP: Verify Constraints\n\n\
                 Present each existing constraint that the task may affect:\n\n",
            );

            for (name, d) in &decisions {
                out.push_str(&format!(
                    "CONSTRAINT: {name} \u{2014} {} ({})\n\
                     \u{2192} Locate in source code and verify it is still enforced\n\
                     \u{2192} Check if the current task conflicts with this constraint\n\n",
                    sanitize(&d.decision.choice),
                    sanitize(&d.decision.reason),
                ));
            }

            out.push_str(
                "If any constraint has drifted \u{2192} call update_decision(mode=\"revise\").\n\
                 If any constraint conflicts with the task \u{2192} note the conflict \
                 and call update_decision.\n\
                 If you cannot locate a constraint in the source code, flag it as \
                 potentially drifted.\n\
                 Report all findings and call advance again.\n",
            );
        }
    }
    out
}

fn step_impact_check(
    graph: &InMemoryGraph,
    component: &str,
    task_type: Option<&str>,
    mode: Mode,
) -> String {
    let connects_to = graph.connects_to(component);
    let connects_from = graph.connects_from(component);

    let mut out = String::with_capacity(256);
    out.push_str("STEP: Impact Check\n\n");

    if connects_to.is_empty() && connects_from.is_empty() {
        out.push_str("No connected components. Impact check complete.\n");
        return out;
    }

    out.push_str("Connected components:\n");
    for c in &connects_to {
        out.push_str(&format!("  \u{2192} {c} (this component sends to it)\n"));
    }
    for c in &connects_from {
        out.push_str(&format!("  \u{2190} {c} (sends to this component)\n"));
    }

    match mode {
        Mode::Interactive => match task_type {
            Some("fix") => {
                out.push_str(
                    "\nAsk: \"Could this fix change the behavior that \
                     connected components depend on? Walk me through the \
                     interface.\"\n\
                     STOP. Wait.\n",
                );
            }
            _ => {
                out.push_str(
                    "\nAsk: \"Which of these connections does your feature \
                     affect? Does the interface between them need to change?\"\n\
                     STOP. Wait.\n",
                );
            }
        },
        Mode::Agent => {
            out.push_str(
                "\nFor each connected component, read the interface code and \
                 determine whether the current task affects it. Report \
                 findings and call advance again.\n",
            );
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
                 For each candidate, present it as a discussion:\n\
                 \"These decisions seem to work together \u{2014} [describe the pattern]. \
                 Does that match how you think about it?\"\n\
                 STOP. Wait.\n\
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

fn step_design_check(task_type: Option<&str>) -> String {
    let question = match task_type {
        Some("learn") => {
            "\"A new team member asks you to explain this component \
             over coffee. What do you tell them \u{2014} the essential things \
             they need to understand?\""
        }
        Some("review") => {
            "\"Based on this review, what\u{2019}s changed, what\u{2019}s still solid, \
             and what needs attention next?\""
        }
        Some("feature") => {
            "\"You\u{2019}re opening the PR. What do you write in the \
             description about architectural impact?\""
        }
        _ => {
            "\"A new team member is about to make their first change \
             to this component. What do they need to know before they \
             touch anything?\""
        }
    };

    format!(
        "STEP: Design Check\n\n\
         Ask: {question}\n\n\
         This is a practical check, not a quiz. The user should be able \
         to answer from understanding built during this session.\n\n\
         If they miss something important, don\u{2019}t say \"wrong.\" Say: \
         \"What about [topic]? We talked about that earlier \u{2014} how does \
         it fit in?\" Let them connect the dots.\n\n\
         If they cover the key points, the session is complete.\n"
    )
}

fn step_drift_check(graph: &InMemoryGraph, component: &str, mode: Mode) -> String {
    let mut decisions: Vec<_> = graph.decisions_for(component);
    decisions.sort_by_key(|(_, d)| d.decision.created);

    if decisions.is_empty() {
        return "STEP: Drift Check\n\nNo decisions to check.\n".into();
    }

    let mut out = String::with_capacity(512);

    match mode {
        Mode::Interactive => {
            out.push_str(
                "STEP: Drift Check\n\n\
                 Check each decision against current source code. Oldest first \
                 \u{2014} older decisions are more likely to have drifted.\n\n",
            );

            let now = Utc::now();
            for (name, d) in &decisions {
                let code_line = format_code_refs_line(&d.decision);
                let age_note = format!("Created: {}", d.decision.created.format("%Y-%m-%d"));
                out.push_str(&format!(
                    "DECISION: {name}\n\
                     Choice: {choice}\n\
                     {code_line}\
                     {age_note}\n\
                     \u{2192} Read the code at these locations\n\
                     \u{2192} Ask: \"This is {age} old. Does the code still do this?\"\n\
                     \u{2192} If drifted: discuss what changed and why, then call \
                     update_decision(mode=\"revise\")\n\
                     \u{2192} STOP. Wait.\n\n",
                    choice = sanitize(&d.decision.choice),
                    age = format_age(d.decision.created, now),
                ));
            }
        }
        Mode::Agent => {
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
                        store::format_code_refs(&d.decision.code_refs),
                    ));
                }
                out.push_str(
                    "\u{2192} Verify this matches the current implementation\n\
                     \u{2192} If drifted, call update_decision(mode=\"revise\")\n\n",
                );
            }

            out.push_str(
                "Verify each decision automatically against source code. \
                 Revise any that have drifted. Call advance again when done.\n",
            );
        }
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
                    "\nSome gaps are real and some are intentional — not every \
                     component needs a decision in every area.\n\n\
                     Walk through each uncovered area with the user:\n\
                     - Ask: \"Is [area] something this component needs to \
                     address, or is it intentionally out of scope?\"\n\
                     - STOP. Wait.\n\
                     - If it needs coverage, use cover_concerns to work \
                     through it together.\n\
                     - If it's intentional, note why and move on.\n",
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

fn step_warm_up() -> String {
    "STEP: Warm-Up\n\n\
     Start with a practical question:\n\
     \"If something broke in this component right now, what's the \
     first thing you'd check and why?\"\n\n\
     STOP. Wait.\n\n\
     Their answer reveals their mental model without making them \
     perform. Note:\n\
     - What they mention → they understand this\n\
     - What they omit → explore in later steps\n\
     - Don't correct yet — save discrepancies for analyze_code\n\n\
     Follow up with ONE boundary question:\n\
     \"What's the one thing this component should never do, even \
     if someone asks for it?\"\n\n\
     STOP. Wait.\n"
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

/// Maximum character count for a single decision value inlined into a prompt.
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

/// Sanitize and truncate text for inline use in questions.
///
/// Applies `sanitize` (strips control chars, caps at 512 chars), then
/// truncates further to `max_chars` characters with a trailing ellipsis.
fn sanitize_short(s: &str, max_chars: usize) -> String {
    let cleaned = sanitize(s);
    if cleaned.chars().count() <= max_chars {
        cleaned
    } else {
        let truncated: String = cleaned.chars().take(max_chars).collect();
        format!("{truncated}\u{2026}")
    }
}

/// Format a decision's code references as a single prompt line.
///
/// Returns `"Code: file::symbol, file2\n"` when refs exist, or an
/// empty string when there are none — callers can unconditionally
/// include the result without conditional formatting.
fn format_code_refs_line(decision: &Decision) -> String {
    if decision.code_refs.is_empty() {
        return String::new();
    }
    format!("Code: {}\n", store::format_code_refs(&decision.code_refs))
}

/// Human-readable age between `created` and `now`.
///
/// Takes `now` as a parameter so callers in prompts pass `Utc::now()`
/// while tests pass a fixed timestamp.
fn format_age(created: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let days = (now - created).num_days().max(0);
    if days < 30 {
        if days == 1 {
            "1 day".into()
        } else {
            format!("{days} days")
        }
    } else if days < 365 {
        let months = days / 30;
        if months == 1 {
            "1 month".into()
        } else {
            format!("{months} months")
        }
    } else {
        let years = days / 365;
        if years == 1 {
            "1 year".into()
        } else {
            format!("{years} years")
        }
    }
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
                    history: vec![],
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
                    history: vec![],
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
                    history: vec![],
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
            "design_check",
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
            "design_check",
        ] {
            let result =
                build_step_prompt(&state, "auth", step, None, None, Mode::Interactive).unwrap();
            assert!(
                result.instructions.contains("ONE topic per exchange"),
                "step `{step}` missing interaction protocol"
            );
        }
    }

    #[test]
    fn register_and_ready_skip_protocol() {
        let state = test_state();
        let reg =
            build_step_prompt(&state, "auth", "register", None, None, Mode::Interactive).unwrap();
        assert!(!reg.instructions.contains("ONE topic per exchange"));

        let ready =
            build_step_prompt(&state, "auth", "ready", None, None, Mode::Interactive).unwrap();
        assert!(!ready.instructions.contains("ONE topic per exchange"));
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
        assert!(result.instructions.contains("security measures"));
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
        assert!(result.instructions.contains("STOP. Wait"));
    }

    #[test]
    fn walk_decisions_review_variant() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "auth",
            "walk_decisions",
            None,
            Some("review"),
            Mode::Interactive,
        )
        .unwrap();
        assert!(
            result.instructions.contains("freshness"),
            "review variant should focus on freshness"
        );
        assert!(
            result.instructions.contains("drifted"),
            "review variant should ask about drift"
        );
        assert!(
            result.instructions.contains("2025-01-15"),
            "review variant should include decision date"
        );
        assert!(
            result.instructions.contains("recorded but aren"),
            "review closing should ask about unrecorded decisions"
        );
    }

    #[test]
    fn walk_decisions_learn_variant() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "auth",
            "walk_decisions",
            None,
            Some("learn"),
            Mode::Interactive,
        )
        .unwrap();
        assert!(
            result.instructions.contains("understanding why"),
            "learn variant should focus on understanding"
        );
        assert!(
            result.instructions.contains("trade-off"),
            "learn variant should probe trade-offs"
        );
        assert!(
            result.instructions.contains("Why was this approach chosen"),
            "learn variant should ask about alternatives"
        );
    }

    #[test]
    fn walk_decisions_default_discusses() {
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
        assert!(
            result.instructions.contains("Discuss each decision"),
            "default should frame as discussion"
        );
        assert!(
            !result.instructions.contains("confirm or correct"),
            "default should not use confirm-or-correct language"
        );
        assert!(
            result.instructions.contains("starting over today"),
            "default closing should surface design debt"
        );
    }

    #[test]
    fn walk_decisions_history_display() {
        let ts = Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap();

        let mut state = test_state();
        let revised = Arc::new(DecisionFile {
            decision: Decision {
                component: "auth".into(),
                choice: "JWT with rotating keys".into(),
                reason: "Key rotation improves security posture".into(),
                alternatives: vec![],
                tags: vec!["security".into()],
                attribution: Attribution::User,
                created: ts,
                code_refs: vec![],
                history: vec![HistoryEntry {
                    choice: "JWT with static keys".into(),
                    reason: "Simple key management".into(),
                    changed_at: ts,
                }],
            },
        });
        state.decisions.insert("auth-jwt".into(), revised);
        state.rebuild_graph();

        let result = build_step_prompt(
            &state,
            "auth",
            "walk_decisions",
            None,
            None,
            Mode::Interactive,
        )
        .unwrap();
        assert!(
            result.instructions.contains("Revised 1 time(s)"),
            "should show revision count"
        );
        assert!(
            result.instructions.contains("JWT with static keys"),
            "should show original choice"
        );
    }

    #[test]
    fn walk_decisions_agent_unchanged_by_task_type() {
        let state = test_state();
        let without =
            build_step_prompt(&state, "auth", "walk_decisions", None, None, Mode::Agent).unwrap();
        let with_review = build_step_prompt(
            &state,
            "auth",
            "walk_decisions",
            None,
            Some("review"),
            Mode::Agent,
        )
        .unwrap();
        assert_eq!(
            without.instructions, with_review.instructions,
            "agent mode should not vary by task_type"
        );
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
                .contains("Does your change respect this, or does it need")
        );
    }

    #[test]
    fn verify_constraints_anchors_on_code_location() {
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
            result.instructions.contains("code location"),
            "verify_constraints must direct agent to constraint code"
        );
    }

    #[test]
    fn verify_constraints_fix_variant() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "auth",
            "verify_constraints",
            None,
            Some("fix"),
            Mode::Interactive,
        )
        .unwrap();
        assert!(
            result.instructions.contains("Tell me about this bug"),
            "fix variant should open by understanding the bug"
        );
        assert!(
            result.instructions.contains("regression"),
            "fix variant closing should ask about regression risk"
        );
    }

    #[test]
    fn verify_constraints_feature_variant() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "auth",
            "verify_constraints",
            None,
            Some("feature"),
            Mode::Interactive,
        )
        .unwrap();
        assert!(
            result.instructions.contains("Walk me through this feature"),
            "feature variant should open by understanding the feature"
        );
        assert!(
            result.instructions.contains("architecture doesn't"),
            "feature variant closing should ask about architecture gaps"
        );
    }

    #[test]
    fn verify_constraints_default_uses_feature_variant() {
        let state = test_state();
        let no_type = build_step_prompt(
            &state,
            "auth",
            "verify_constraints",
            None,
            None,
            Mode::Interactive,
        )
        .unwrap();
        let feature = build_step_prompt(
            &state,
            "auth",
            "verify_constraints",
            None,
            Some("feature"),
            Mode::Interactive,
        )
        .unwrap();
        assert_eq!(
            no_type.instructions, feature.instructions,
            "default (no task_type) should match feature variant"
        );
    }

    #[test]
    fn verify_constraints_understands_change_before_checking() {
        let state = test_state();
        for task_type in [None, Some("fix"), Some("feature")] {
            let result = build_step_prompt(
                &state,
                "auth",
                "verify_constraints",
                None,
                task_type,
                Mode::Interactive,
            )
            .unwrap();
            let understanding_pos = result
                .instructions
                .find("Start by understanding")
                .expect("should start by understanding the change");
            let constraint_pos = result
                .instructions
                .find("CONSTRAINT:")
                .expect("should list constraints");
            assert!(
                understanding_pos < constraint_pos,
                "task_type={task_type:?}: must understand the change before listing constraints"
            );
        }
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
    fn impact_check_fix_variant() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "auth",
            "impact_check",
            None,
            Some("fix"),
            Mode::Interactive,
        )
        .unwrap();
        assert!(
            result
                .instructions
                .contains("Could this fix change the behavior"),
            "fix variant should ask about behavioral change from the fix"
        );
    }

    #[test]
    fn impact_check_feature_variant() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "auth",
            "impact_check",
            None,
            Some("feature"),
            Mode::Interactive,
        )
        .unwrap();
        assert!(
            result
                .instructions
                .contains("Which of these connections does your feature"),
            "feature variant should ask which connections the feature affects"
        );
    }

    #[test]
    fn impact_check_default_uses_feature_variant() {
        let state = test_state();
        let no_type = build_step_prompt(
            &state,
            "auth",
            "impact_check",
            None,
            None,
            Mode::Interactive,
        )
        .unwrap();
        let feature = build_step_prompt(
            &state,
            "auth",
            "impact_check",
            None,
            Some("feature"),
            Mode::Interactive,
        )
        .unwrap();
        assert_eq!(
            no_type.instructions, feature.instructions,
            "default (no task_type) should match feature variant"
        );
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
    fn pattern_detection_presents_as_discussion() {
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
        assert!(
            result
                .instructions
                .contains("Does that match how you think about it?"),
            "pattern_detection should frame as discussion"
        );
        assert!(
            !result.instructions.contains("Should I record it?"),
            "pattern_detection should not ask for confirmation"
        );
    }

    #[test]
    fn design_check_practical_not_quiz() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "auth",
            "design_check",
            None,
            None,
            Mode::Interactive,
        )
        .unwrap();
        assert!(
            result.instructions.contains("practical check"),
            "design_check should frame as practical check"
        );
        assert!(
            !result.instructions.contains("Do NOT help"),
            "design_check should not contain adversarial gating"
        );
        assert!(
            !result.instructions.contains("Do NOT give hints"),
            "design_check should not contain adversarial gating"
        );
        assert!(
            !result.instructions.contains("Without looking at the list"),
            "design_check should not demand unprompted recall"
        );
        assert!(
            !result.instructions.contains("demonstrates ownership"),
            "design_check should not use demonstrate-ownership language"
        );
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

    // ── INTERACTION_PROTOCOL tone ────────────────────────────────────

    #[test]
    fn interaction_protocol_peer_tone() {
        assert!(
            INTERACTION_PROTOCOL.contains("senior engineer"),
            "protocol should establish peer framing"
        );
        assert!(
            INTERACTION_PROTOCOL.contains("peer"),
            "protocol should reference peer relationship"
        );
        assert!(
            INTERACTION_PROTOCOL.contains("trade-off"),
            "protocol should emphasize trade-off discussion"
        );
        assert!(
            INTERACTION_PROTOCOL.contains("pushback"),
            "protocol should handle user pushback"
        );
    }

    #[test]
    fn interaction_protocol_no_old_patterns() {
        assert!(
            !INTERACTION_PROTOCOL.contains("restate in your own words"),
            "protocol must not use classroom restate language"
        );
        assert!(
            !INTERACTION_PROTOCOL.contains("restate that"),
            "protocol must not use classroom restate language"
        );
        assert!(
            !INTERACTION_PROTOCOL.contains("Does that deepen your understanding"),
            "protocol must not use patronizing deepening language"
        );
        assert!(
            !INTERACTION_PROTOCOL.contains("Do NOT help"),
            "protocol must not contain adversarial gating"
        );
        assert!(
            !INTERACTION_PROTOCOL.contains("Do NOT give hints"),
            "protocol must not contain adversarial gating"
        );
    }

    // ── Cross-prompt tone ────────────────────────────────────────────

    #[test]
    fn no_interactive_prompt_uses_adversarial_language() {
        let state = test_state();
        let interactive_steps = [
            "define_scope",
            "analyze_code",
            "cover_concerns",
            "walk_decisions",
            "verify_constraints",
            "impact_check",
            "pattern_detection",
            "design_check",
            "drift_check",
            "coverage_audit",
            "warm_up",
        ];

        for step in &interactive_steps {
            let result =
                build_step_prompt(&state, "auth", step, None, None, Mode::Interactive).unwrap();

            assert!(
                !result.instructions.contains("confirm or correct"),
                "step `{step}` must not use confirm-or-correct language"
            );
            assert!(
                !result.instructions.contains("Without looking at the list"),
                "step `{step}` must not demand unprompted recall"
            );
            assert!(
                !result.instructions.contains("demonstrate understanding"),
                "step `{step}` must not use demonstrate-understanding language"
            );
            assert!(
                !result.instructions.contains("demonstrate ownership"),
                "step `{step}` must not use demonstrate-ownership language"
            );
        }
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
            !result.instructions.contains("ONE topic per exchange"),
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
            !result.instructions.contains("ONE topic per exchange"),
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
            !result.instructions.contains("ONE topic per exchange"),
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

    // ── warm_up step (and user_explains alias) ─────────────────────

    #[test]
    fn build_step_prompt_accepts_warm_up() {
        let state = test_state();
        let result =
            build_step_prompt(&state, "auth", "warm_up", None, None, Mode::Interactive).unwrap();
        assert!(result.instructions.contains("first thing you'd check"));
    }

    #[test]
    fn build_step_prompt_accepts_user_explains_alias() {
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
        assert!(result.instructions.contains("first thing you'd check"));
    }

    #[test]
    fn warm_up_includes_interaction_protocol() {
        let state = test_state();
        let result =
            build_step_prompt(&state, "auth", "warm_up", None, None, Mode::Interactive).unwrap();
        assert!(
            result.instructions.contains("ONE topic per exchange"),
            "warm_up must include interaction protocol"
        );
    }

    #[test]
    fn warm_up_asks_boundary_question() {
        let state = test_state();
        let result =
            build_step_prompt(&state, "auth", "warm_up", None, None, Mode::Interactive).unwrap();
        assert!(
            result.instructions.contains("should never do"),
            "warm_up must probe component boundaries"
        );
    }

    #[test]
    fn warm_up_does_not_require_recall() {
        let state = test_state();
        let result =
            build_step_prompt(&state, "auth", "warm_up", None, None, Mode::Interactive).unwrap();
        assert!(
            !result.instructions.contains("from memory"),
            "warm_up should not ask for recall from memory"
        );
    }

    // ── design_check task_type variants (and summary_gate alias) ────

    #[test]
    fn build_step_prompt_accepts_design_check() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "auth",
            "design_check",
            None,
            None,
            Mode::Interactive,
        )
        .unwrap();
        assert!(result.instructions.contains("Design Check"));
    }

    #[test]
    fn build_step_prompt_accepts_summary_gate_alias() {
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
        assert!(result.instructions.contains("Design Check"));
    }

    #[test]
    fn design_check_learn_variant() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "auth",
            "design_check",
            None,
            Some("learn"),
            Mode::Interactive,
        )
        .unwrap();
        assert!(
            result.instructions.contains("explain this component"),
            "learn variant should use explain-over-coffee framing"
        );
        assert!(
            result.instructions.contains("over coffee"),
            "learn variant should use casual peer framing"
        );
    }

    #[test]
    fn design_check_feature_variant() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "auth",
            "design_check",
            None,
            Some("feature"),
            Mode::Interactive,
        )
        .unwrap();
        assert!(
            result.instructions.contains("opening the PR"),
            "feature variant should use PR framing"
        );
        assert!(
            result.instructions.contains("architectural impact"),
            "feature variant should ask about architectural impact"
        );
    }

    #[test]
    fn design_check_review_variant() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "auth",
            "design_check",
            None,
            Some("review"),
            Mode::Interactive,
        )
        .unwrap();
        assert!(
            result.instructions.contains("what\u{2019}s changed"),
            "review variant should ask what changed"
        );
        assert!(
            result.instructions.contains("needs attention"),
            "review variant should surface what needs attention"
        );
    }

    #[test]
    fn design_check_default_variant() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "auth",
            "design_check",
            None,
            None,
            Mode::Interactive,
        )
        .unwrap();
        assert!(
            result.instructions.contains("new team member"),
            "default variant should use new-team-member framing"
        );
        assert!(
            result.instructions.contains("need to know before they"),
            "default variant should ask what to know before touching code"
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
            result.instructions.contains("warm-up") || result.instructions.contains("mental model"),
            "learn variant should reference the warm-up step"
        );
        assert!(
            result.instructions.contains("discussion points"),
            "learn variant should frame discrepancies as discussion"
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
            !result.instructions.contains("warm-up"),
            "non-learn variant should not include learn preamble"
        );
    }

    #[test]
    fn analyze_code_interactive_one_at_a_time() {
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
        assert!(
            result.instructions.contains("One decision at a time"),
            "interactive variant should walk decisions one at a time"
        );
        assert!(
            !result.instructions.contains("Build a\n"),
            "interactive variant should not instruct building a list"
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
                !result.instructions.contains("ONE topic per exchange"),
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
                result.instructions.contains("ONE topic per exchange"),
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
            result.instructions.contains("component's job"),
            "interactive define_scope should ask about component's job"
        );
    }

    #[test]
    fn interactive_define_scope_probes_boundaries() {
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
            result.instructions.contains("probe the boundary"),
            "interactive define_scope should probe boundaries"
        );
        assert!(
            result.instructions.contains("scope creep"),
            "interactive define_scope should emphasize boundary importance"
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
    fn cover_concerns_feature_asks_about_feature_impact() {
        let state = test_state();
        let result = build_step_prompt(
            &state,
            "store",
            "cover_concerns",
            None,
            Some("feature"),
            Mode::Interactive,
        )
        .unwrap();
        assert!(
            result.instructions.contains("affect your feature"),
            "feature variant should ask how concern affects the feature"
        );
        assert!(
            result
                .instructions
                .contains("Focus only on concerns the feature actually impacts"),
            "feature variant should scope to relevant concerns"
        );
        assert!(
            !result.instructions.contains("How are you thinking about"),
            "feature variant should not use the default question"
        );
    }

    #[test]
    fn cover_concerns_default_starts_with_user_thinking() {
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
        assert!(
            result.instructions.contains("How are you thinking about"),
            "default variant should start with the user's opinion"
        );
        assert!(
            result
                .instructions
                .contains("Start with their thinking, not a menu of options"),
            "default variant should emphasize user-first discussion"
        );
        assert!(
            !result.instructions.contains("affect your feature"),
            "default variant should not use feature-specific question"
        );
    }

    #[test]
    fn cover_concerns_agent_unchanged_by_task_type() {
        let state = test_state();
        let without =
            build_step_prompt(&state, "store", "cover_concerns", None, None, Mode::Agent).unwrap();
        let with_feature = build_step_prompt(
            &state,
            "store",
            "cover_concerns",
            None,
            Some("feature"),
            Mode::Agent,
        )
        .unwrap();
        assert_eq!(
            without.instructions, with_feature.instructions,
            "agent mode should not vary by task_type"
        );
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
        assert!(result.instructions.contains("revise"));
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
    fn drift_check_interactive_includes_age() {
        let state = test_state();
        let result =
            build_step_prompt(&state, "auth", "drift_check", None, None, Mode::Interactive)
                .unwrap();
        assert!(
            result
                .instructions
                .contains("old. Does the code still do this?"),
            "interactive drift_check should include age in question"
        );
        assert!(
            result.instructions.contains("Created: 2025-01-15"),
            "interactive drift_check should show creation date"
        );
    }

    #[test]
    fn drift_check_interactive_stops_per_decision() {
        let state = test_state();
        let result =
            build_step_prompt(&state, "auth", "drift_check", None, None, Mode::Interactive)
                .unwrap();
        assert!(
            result.instructions.contains("STOP. Wait"),
            "interactive drift_check should stop after each decision"
        );
    }

    #[test]
    fn drift_check_interactive_discusses_before_revising() {
        let state = test_state();
        let result =
            build_step_prompt(&state, "auth", "drift_check", None, None, Mode::Interactive)
                .unwrap();
        assert!(
            result.instructions.contains("discuss what changed"),
            "interactive drift_check should discuss changes before revising"
        );
        assert!(
            result
                .instructions
                .contains("update_decision(mode=\"revise\")"),
            "interactive drift_check should use revise mode"
        );
    }

    #[test]
    fn drift_check_interactive_shows_code_refs() {
        let ts = Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap();
        let mut state = test_state();
        let with_refs = Arc::new(DecisionFile {
            decision: Decision {
                component: "auth".into(),
                choice: "JWT with DPoP binding".into(),
                reason: "Proof-of-possession prevents token theft".into(),
                alternatives: vec![],
                tags: vec!["security".into()],
                attribution: Attribution::User,
                created: ts,
                code_refs: vec![CodeRef {
                    file: "src/auth/jwt.rs".into(),
                    symbol: Some("verify_dpop".into()),
                }],
                history: vec![],
            },
        });
        state.decisions.insert("auth-jwt".into(), with_refs);
        state.rebuild_graph();

        let result =
            build_step_prompt(&state, "auth", "drift_check", None, None, Mode::Interactive)
                .unwrap();
        assert!(
            result.instructions.contains("src/auth/jwt.rs::verify_dpop"),
            "interactive drift_check should show code references via format_code_refs_line"
        );
    }

    #[test]
    fn drift_check_interactive_explains_oldest_first() {
        let state = test_state();
        let result =
            build_step_prompt(&state, "auth", "drift_check", None, None, Mode::Interactive)
                .unwrap();
        assert!(
            result.instructions.contains("Oldest first"),
            "interactive drift_check should explain oldest-first ordering"
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
                        && !result.instructions.contains("ONE topic per exchange"),
                    "step `{step}` should skip both protocols in {:?}",
                    mode
                );
            }
        }
    }

    // ── sanitize_short ──────────────────────────────────────────────

    #[test]
    fn sanitize_short_under_limit_unchanged() {
        assert_eq!(sanitize_short("hello world", 20), "hello world");
    }

    #[test]
    fn sanitize_short_truncates_with_ellipsis() {
        let result = sanitize_short("a long reason that exceeds the limit", 10);
        assert_eq!(result, "a long rea\u{2026}");
        assert_eq!(result.chars().count(), 11); // 10 chars + ellipsis
    }

    #[test]
    fn sanitize_short_exact_limit() {
        assert_eq!(sanitize_short("12345", 5), "12345");
    }

    #[test]
    fn sanitize_short_empty_input() {
        assert_eq!(sanitize_short("", 10), "");
    }

    #[test]
    fn sanitize_short_strips_control_chars_before_counting() {
        let input = "he\x01llo"; // control char removed → "hello" (5 chars)
        assert_eq!(sanitize_short(input, 5), "hello");
        assert_eq!(sanitize_short(input, 3), "hel\u{2026}");
    }

    // ── format_code_refs_line ───────────────────────────────────────

    fn minimal_decision(code_refs: Vec<CodeRef>) -> Decision {
        Decision {
            component: "test".into(),
            choice: "test choice".into(),
            reason: "test reason".into(),
            alternatives: vec![],
            tags: vec![],
            attribution: Attribution::User,
            created: Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap(),
            code_refs,
            history: vec![],
        }
    }

    #[test]
    fn format_code_refs_line_empty() {
        let d = minimal_decision(vec![]);
        assert_eq!(format_code_refs_line(&d), "");
    }

    #[test]
    fn format_code_refs_line_file_only() {
        let d = minimal_decision(vec![CodeRef {
            file: "src/main.rs".into(),
            symbol: None,
        }]);
        assert_eq!(format_code_refs_line(&d), "Code: src/main.rs\n");
    }

    #[test]
    fn format_code_refs_line_file_and_symbol() {
        let d = minimal_decision(vec![CodeRef {
            file: "src/store.rs".into(),
            symbol: Some("Store::load".into()),
        }]);
        assert_eq!(
            format_code_refs_line(&d),
            "Code: src/store.rs::Store::load\n"
        );
    }

    #[test]
    fn format_code_refs_line_multiple() {
        let d = minimal_decision(vec![
            CodeRef {
                file: "src/a.rs".into(),
                symbol: Some("foo".into()),
            },
            CodeRef {
                file: "src/b.rs".into(),
                symbol: None,
            },
        ]);
        assert_eq!(format_code_refs_line(&d), "Code: src/a.rs::foo, src/b.rs\n");
    }

    // ── format_age ──────────────────────────────────────────────────

    #[test]
    fn format_age_zero_days() {
        let now = Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap();
        assert_eq!(format_age(now, now), "0 days");
    }

    #[test]
    fn format_age_one_day() {
        let created = Utc.with_ymd_and_hms(2025, 6, 14, 12, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap();
        assert_eq!(format_age(created, now), "1 day");
    }

    #[test]
    fn format_age_several_days() {
        let created = Utc.with_ymd_and_hms(2025, 6, 1, 0, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2025, 6, 15, 0, 0, 0).unwrap();
        assert_eq!(format_age(created, now), "14 days");
    }

    #[test]
    fn format_age_one_month() {
        let created = Utc.with_ymd_and_hms(2025, 5, 15, 0, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2025, 6, 15, 0, 0, 0).unwrap();
        assert_eq!(format_age(created, now), "1 month");
    }

    #[test]
    fn format_age_several_months() {
        let created = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2025, 6, 15, 0, 0, 0).unwrap();
        assert_eq!(format_age(created, now), "5 months");
    }

    #[test]
    fn format_age_one_year() {
        let created = Utc.with_ymd_and_hms(2024, 6, 15, 0, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2025, 6, 15, 0, 0, 0).unwrap();
        assert_eq!(format_age(created, now), "1 year");
    }

    #[test]
    fn format_age_multiple_years() {
        let created = Utc.with_ymd_and_hms(2022, 1, 1, 0, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2025, 6, 15, 0, 0, 0).unwrap();
        assert_eq!(format_age(created, now), "3 years");
    }

    #[test]
    fn format_age_future_date_clamps_to_zero() {
        let created = Utc.with_ymd_and_hms(2025, 7, 1, 0, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2025, 6, 15, 0, 0, 0).unwrap();
        assert_eq!(format_age(created, now), "0 days");
    }
}
