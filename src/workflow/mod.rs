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
//! 3. **Graph is the only state.** No session tracking, no step counters.
//!    The state machine inspects the graph and deduces which step comes next.
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
    /// DefineScope → CoverConcerns → PatternDetection → SummaryGate.
    NewComponent,

    /// Add a feature to an existing component.
    /// VerifyConstraints → CoverConcerns(focused) → Ready.
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
            _ => Err(format!(
                "invalid task_type `{s}` — expected: \
                 new_component, feature, fix, learn, review, harden"
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
            Self::Ready => "ready",
        }
    }

    /// Whether this step represents a terminal state (no more workflow
    /// actions required).
    #[allow(dead_code)]
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Ready | Self::SummaryGate)
    }
}
