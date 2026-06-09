//! Workflow engine — single source of truth for task-driven orchestration.
//!
//! The workflow module owns TaskType classification, Step deduction, concern
//! tracking, and (future) per-step prompt generation. Transport layers (MCP,
//! session) delegate to this module for all workflow logic.
//!
//! # Design Principles
//!
//! 1. **Task-driven, not mode-driven.** The workflow adapts to what the
//!    developer wants to accomplish, not to which transport tool was called.
//!
//! 2. **One step at a time.** Each `advance` call returns instructions for
//!    exactly one step.
//!
//! 3. **Graph is the primary state.** The state machine inspects the graph
//!    and deduces which step comes next. For steps whose postconditions
//!    are not verifiable from the graph alone, callers may provide a
//!    `completed_steps` hint — a progression signal compatible with
//!    crash recovery (session files persist completed steps across restarts).
//!
//! 4. **Transport-agnostic.** Prompt generation lives here, not in `mcp/`
//!    or `session/`.

pub mod advance;
pub mod concerns;
pub mod steps;

// ── Constants ─────────────────────────────────────────────────────────────

/// Decisions older than this trigger staleness detection (days).
pub const STALENESS_THRESHOLD_DAYS: i64 = 90;

/// Maximum concerns in `CoverConcerns` focus list.
pub const CONCERN_FOCUS_LIMIT: usize = 3;

// ── TaskType ──────────────────────────────────────────────────────────────

/// What the developer wants to accomplish. Determines which workflow
/// steps apply and in what order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskType {
    /// Build a new component from scratch.
    /// DefineScope → CoverConcerns → PatternDetection → SummaryGate → Ready.
    NewComponent,

    /// Add a feature to an existing component.
    /// VerifyConstraints → CoverConcerns(focused) → PatternDetection → Ready.
    Feature,

    /// Fix a bug or apply a hotfix.
    /// VerifyConstraints → ImpactCheck → Ready.
    Fix,

    /// Study existing architecture.
    /// AnalyzeCode → WalkDecisions → PatternDetection → Ready.
    Learn,

    /// Challenge existing decisions for drift.
    /// WalkDecisions → DriftCheck → CoverageAudit → PatternDetection → Ready.
    Review,

    /// Strengthen coverage of under-designed areas.
    /// CoverageAudit → CoverConcerns(gaps) → PatternDetection → Ready.
    Harden,

    /// Autonomous codebase scan for existing projects.
    /// Agent reads source code and records components, decisions, and
    /// patterns without interactive dialogue.
    /// ScanProject → ExtractDecisions(per-component) → ProjectRules → PatternDetection → Ready.
    Bootstrap,
}

impl TaskType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NewComponent => "new_component",
            Self::Feature => "feature",
            Self::Fix => "fix",
            Self::Learn => "learn",
            Self::Review => "review",
            Self::Harden => "harden",
            Self::Bootstrap => "bootstrap",
        }
    }

    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "new_component" => Ok(Self::NewComponent),
            "feature" => Ok(Self::Feature),
            "fix" => Ok(Self::Fix),
            "learn" => Ok(Self::Learn),
            "review" => Ok(Self::Review),
            "harden" => Ok(Self::Harden),
            "bootstrap" => Ok(Self::Bootstrap),
            _ => Err(format!(
                "invalid task_type `{s}` — expected: \
                 new_component, feature, fix, learn, review, harden, bootstrap"
            )),
        }
    }
}

// ── Step ──────────────────────────────────────────────────────────────────

/// A single workflow step. Each `advance()` call returns at most one.
///
/// Steps have preconditions (what the graph must look like for this step to
/// apply) and postconditions (what changes after the step succeeds). The
/// advance state machine checks preconditions to determine the next step.
///
/// Some postconditions are heuristic — `PatternDetection` might not produce
/// graph artifacts if no patterns exist. The state machine errs on the side
/// of returning `Ready` rather than looping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Agent must register the component before anything else.
    Register,

    /// Define what the component is and isn't responsible for.
    /// Postcondition: ≥1 decision with tag "scope" exists.
    DefineScope,

    /// Analyze source code, identify all implicit decisions.
    /// Postcondition: decisions recorded by the agent.
    AnalyzeCode,

    /// Cover specific uncovered concern areas.
    /// Carries the focus list (top N uncovered concerns by priority).
    CoverConcerns { focus: Vec<String> },

    /// Walk through existing decisions one by one.
    /// For learn: present, explain, capture understanding.
    /// For review: challenge against current source code.
    WalkDecisions,

    /// Verify existing constraints still hold for this task.
    /// For fix/feature: quick check, flag any that conflict.
    VerifyConstraints,

    /// Check whether a fix or feature impacts other components.
    ImpactCheck,

    /// Identify patterns across recorded decisions.
    PatternDetection,

    /// Final comprehension gate — user summarizes without help.
    SummaryGate,

    /// Verify all decisions still match the source code.
    DriftCheck,

    /// Audit concern coverage, identify gaps.
    CoverageAudit,

    /// Scan the full project structure, identify components and connections.
    /// Bootstrap-only. Non-interactive.
    /// Postcondition: ≥1 component registered.
    ScanProject,

    /// Extract decisions from a specific component's source code.
    /// Bootstrap-only. Non-interactive.
    /// Carries the target component name for the action payload.
    /// Postcondition: target component has ≥1 decision.
    ExtractDecisions { component: String },

    /// Record project-level cross-cutting decisions.
    /// Bootstrap-only. Non-interactive.
    /// Postcondition: project has ≥1 decision.
    ProjectRules,

    /// All steps complete. Ready for implementation.
    Ready,
}

impl Step {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Register => "register",
            Self::DefineScope => "define_scope",
            Self::AnalyzeCode => "analyze_code",
            Self::CoverConcerns { .. } => "cover_concerns",
            Self::WalkDecisions => "walk_decisions",
            Self::VerifyConstraints => "verify_constraints",
            Self::ImpactCheck => "impact_check",
            Self::PatternDetection => "pattern_detection",
            Self::SummaryGate => "summary_gate",
            Self::DriftCheck => "drift_check",
            Self::CoverageAudit => "coverage_audit",
            Self::ScanProject => "scan_project",
            Self::ExtractDecisions { .. } => "extract_decisions",
            Self::ProjectRules => "project_rules",
            Self::Ready => "ready",
        }
    }

    /// Whether this step represents a terminal state (no more workflow
    /// actions required).
    #[allow(dead_code)]
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Ready)
    }
}

// ── Integration tests ─────────────────────────────────────────────────────
//
// These verify the advance→steps pipeline: every step name returned by
// advance() must be accepted by build_step_prompt(), and the responses
// must be structurally consistent.

#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::store::schema::*;
    use chrono::{TimeZone, Utc};
    use std::collections::BTreeMap;

    fn build_state(
        components: &[(&str, &str)],
        decisions: &[(&str, DecisionFile)],
    ) -> crate::store::ProjectState {
        let mut comp_map = BTreeMap::new();
        for &(name, desc) in components {
            comp_map.insert(
                name.into(),
                ComponentFile {
                    component: Component {
                        name: name.into(),
                        description: desc.into(),
                    },
                },
            );
        }

        let mut dec_map = BTreeMap::new();
        let mut nodes: Vec<NodeEntry> = components
            .iter()
            .map(|&(name, _)| NodeEntry {
                name: name.into(),
                kind: NodeKind::Component,
                tags: vec![],
                hash: String::new(),
            })
            .collect();
        let mut edges = Vec::new();

        for &(name, ref dec) in decisions {
            dec_map.insert(name.into(), dec.clone());
            nodes.push(NodeEntry {
                name: name.into(),
                kind: NodeKind::Decision,
                tags: dec.decision.tags.clone(),
                hash: String::new(),
            });
            edges.push(EdgeEntry {
                from: name.into(),
                to: dec.decision.component.clone(),
                kind: EdgeKind::BelongsTo,
            });
        }

        crate::store::ProjectState::new(
            ProjectFile {
                trurlic_version: FORMAT_VERSION.into(),
                project: Project {
                    name: "test".into(),
                    description: String::new(),
                },
            },
            comp_map,
            dec_map,
            BTreeMap::new(),
            GraphIndex {
                version: 1,
                rebuilt: Utc::now(),
                nodes,
                edges,
            },
        )
    }

    fn fresh_decision(component: &str, choice: &str, reason: &str, tags: &[&str]) -> DecisionFile {
        DecisionFile {
            decision: Decision {
                component: component.into(),
                choice: choice.into(),
                reason: reason.into(),
                alternatives: vec![],
                tags: tags.iter().map(|t| (*t).into()).collect(),
                created: Utc::now(),
            },
        }
    }

    fn stale_decision(component: &str, choice: &str, reason: &str, tags: &[&str]) -> DecisionFile {
        DecisionFile {
            decision: Decision {
                component: component.into(),
                choice: choice.into(),
                reason: reason.into(),
                alternatives: vec![],
                tags: tags.iter().map(|t| (*t).into()).collect(),
                created: Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
            },
        }
    }

    /// Advance returns a step → build_step_prompt accepts that step.
    /// This is the core pipeline contract.
    fn assert_pipeline(
        state: &crate::store::ProjectState,
        component: &str,
        task_type: Option<TaskType>,
    ) {
        let result = advance::advance(state, component, task_type, None, &[])
            .expect("advance should succeed");

        let step_name = result["step"]
            .as_str()
            .expect("step field must be a string");
        let ready = result["ready"].as_bool().unwrap_or(false);

        // Ready steps don't use get_step_prompt — they use get_context.
        if ready && step_name == "ready" {
            return;
        }

        // Register steps use add_component, not get_step_prompt.
        if step_name == "register" {
            return;
        }

        // Bootstrap's extract_decisions targets a specific component
        // (returned in action.args.component), not the advance caller's
        // component. Use it when present so the pipeline contract holds.
        let prompt_component = result["action"]["args"]["component"]
            .as_str()
            .unwrap_or(component);

        let prompt = steps::build_step_prompt(state, prompt_component, step_name, None)
            .unwrap_or_else(|e| panic!("build_step_prompt({step_name}) failed: {e}"));

        // Every prompt must include the source code preamble.
        assert!(
            prompt.instructions.contains("source code"),
            "step `{step_name}` prompt missing source code preamble"
        );

        // If advance returned a focus list, it must match.
        if let Some(advance_focus) = result["action"]["focus"].as_array() {
            assert_eq!(
                advance_focus.len(),
                prompt.focus.len(),
                "focus length mismatch for step `{step_name}`"
            );
        }
    }

    // ── Pipeline tests per task type ──────────────────────────────────

    #[test]
    fn pipeline_new_component_empty() {
        let state = build_state(&[("store", "Data store")], &[]);
        assert_pipeline(&state, "store", Some(TaskType::NewComponent));
    }

    #[test]
    fn pipeline_new_component_with_scope() {
        let state = build_state(
            &[("store", "Data store")],
            &[(
                "d-scope",
                fresh_decision("store", "Data layer", "Scope", &["scope"]),
            )],
        );
        assert_pipeline(&state, "store", Some(TaskType::NewComponent));
    }

    #[test]
    fn pipeline_feature() {
        let state = build_state(
            &[("store", "Data store")],
            &[(
                "d1",
                fresh_decision("store", "TOML format", "Readable", &["format"]),
            )],
        );
        assert_pipeline(&state, "store", Some(TaskType::Feature));
    }

    #[test]
    fn pipeline_fix() {
        let state = build_state(
            &[("store", "Data store")],
            &[(
                "d1",
                fresh_decision("store", "TOML format", "Readable", &["format"]),
            )],
        );
        assert_pipeline(&state, "store", Some(TaskType::Fix));
    }

    #[test]
    fn pipeline_learn_empty() {
        let state = build_state(&[("store", "Data store")], &[]);
        assert_pipeline(&state, "store", Some(TaskType::Learn));
    }

    #[test]
    fn pipeline_learn_with_decisions() {
        let state = build_state(
            &[("store", "Data store")],
            &[(
                "d1",
                fresh_decision("store", "TOML format", "Readable", &[]),
            )],
        );
        assert_pipeline(&state, "store", Some(TaskType::Learn));
    }

    #[test]
    fn pipeline_review_stale() {
        let state = build_state(
            &[("store", "Data store")],
            &[(
                "d1",
                stale_decision("store", "TOML format", "Readable", &[]),
            )],
        );
        assert_pipeline(&state, "store", Some(TaskType::Review));
    }

    #[test]
    fn pipeline_review_fresh() {
        let state = build_state(
            &[("store", "Data store")],
            &[(
                "d1",
                fresh_decision("store", "TOML format", "Readable", &[]),
            )],
        );
        assert_pipeline(&state, "store", Some(TaskType::Review));
    }

    #[test]
    fn pipeline_harden() {
        let state = build_state(
            &[("store", "Data store")],
            &[(
                "d1",
                fresh_decision("store", "TOML format", "Readable", &["format"]),
            )],
        );
        assert_pipeline(&state, "store", Some(TaskType::Harden));
    }

    #[test]
    fn pipeline_inferred_task_type() {
        let state = build_state(&[("store", "Data store")], &[]);
        // No task_type → inferred from graph state (Learn for empty).
        assert_pipeline(&state, "store", None);
    }

    #[test]
    fn pipeline_unregistered() {
        let state = build_state(&[], &[]);
        assert_pipeline(&state, "unknown-component", None);
    }

    #[test]
    fn pipeline_project_scope() {
        let state = build_state(&[], &[]);
        assert_pipeline(&state, "project", None);
    }

    #[test]
    fn pipeline_project_with_rules() {
        let state = build_state(
            &[],
            &[(
                "rule-1",
                fresh_decision("project", "Fail-closed", "Safety", &[]),
            )],
        );
        assert_pipeline(&state, "project", Some(TaskType::Learn));
    }

    // ── Bootstrap pipeline ───────────────────────────────────────────

    #[test]
    fn pipeline_bootstrap_empty() {
        let state = build_state(&[], &[]);
        assert_pipeline(&state, "project", Some(TaskType::Bootstrap));
    }

    #[test]
    fn pipeline_bootstrap_with_components() {
        let state = build_state(&[("auth", "Auth"), ("store", "Data store")], &[]);
        assert_pipeline(&state, "project", Some(TaskType::Bootstrap));
    }

    #[test]
    fn pipeline_bootstrap_with_decisions() {
        let state = build_state(
            &[("auth", "Auth")],
            &[("d1", fresh_decision("auth", "JWT tokens", "Stateless", &[]))],
        );
        assert_pipeline(&state, "project", Some(TaskType::Bootstrap));
    }

    // ── Exhaustive step name coverage ─────────────────────────────────

    #[test]
    fn every_step_as_str_accepted_by_build_step_prompt() {
        let state = build_state(
            &[("store", "Data store")],
            &[(
                "d1",
                fresh_decision("store", "TOML format", "Readable", &[]),
            )],
        );

        let step_names = [
            "register",
            "define_scope",
            "analyze_code",
            "cover_concerns",
            "walk_decisions",
            "verify_constraints",
            "impact_check",
            "pattern_detection",
            "summary_gate",
            "drift_check",
            "coverage_audit",
            "scan_project",
            "extract_decisions",
            "project_rules",
            "ready",
        ];

        for name in &step_names {
            let result = steps::build_step_prompt(&state, "store", name, None);
            assert!(
                result.is_ok(),
                "build_step_prompt must accept step `{name}`: {:?}",
                result.err()
            );
        }
    }

    #[test]
    fn step_as_str_round_trips_through_pipeline() {
        // Verify Step::as_str() values match what build_step_prompt accepts.
        let variants: Vec<Step> = vec![
            Step::Register,
            Step::DefineScope,
            Step::AnalyzeCode,
            Step::CoverConcerns {
                focus: vec!["Security".into()],
            },
            Step::WalkDecisions,
            Step::VerifyConstraints,
            Step::ImpactCheck,
            Step::PatternDetection,
            Step::SummaryGate,
            Step::DriftCheck,
            Step::CoverageAudit,
            Step::ScanProject,
            Step::ExtractDecisions {
                component: "store".into(),
            },
            Step::ProjectRules,
            Step::Ready,
        ];

        let state = build_state(
            &[("store", "Data store")],
            &[("d1", fresh_decision("store", "TOML", "Test", &[]))],
        );

        for variant in &variants {
            let name = variant.as_str();
            let result = steps::build_step_prompt(&state, "store", name, None);
            assert!(
                result.is_ok(),
                "Step::{:?} as_str `{name}` rejected by build_step_prompt: {:?}",
                variant,
                result.err()
            );
        }
    }
}
