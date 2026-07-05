//! `trurlic gc` — reclaim decisions that have lost their anchor.
//!
//! Over a project's life, decisions drift out of relevance: a component is
//! deleted and its decisions are orphaned, the code a decision points at is
//! removed, or an agent-recorded decision sits unreviewed for months. `gc`
//! surfaces these and, when asked, removes the ones that are safe to drop.
//!
//! CLI-only — not exposed over MCP. Coding agents record and revise decisions;
//! pruning the graph is a human-supervised maintenance action.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{Duration, Utc};

use crate::Result;
use crate::store::schema::{Attribution, DecisionFile};
use crate::store::{ProjectState, Store, StoreLock};
use crate::workflow::concerns;

use super::open_store_mut;

/// How much `gc` is allowed to reclaim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcScope {
    /// Remove only structurally-orphaned decisions (their component is gone).
    /// Stale and long-unreviewed agent decisions are reported, not removed.
    Safe,
    /// Additionally remove stale and long-unreviewed agent decisions.
    Aggressive,
}

/// Whether `gc` writes its removals or only reports them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcExecution {
    /// Perform the removals.
    Apply,
    /// Report what would happen; change nothing on disk.
    DryRun,
}

/// What the caller should do when `--aggressive --apply` is requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AggressiveConfirm {
    /// User already confirmed via `--yes`; proceed without prompting.
    Confirmed,
    /// Running on a TTY without `--yes`; the caller must prompt.
    PromptUser,
}

/// Decide how to handle an `--aggressive --apply` invocation.
///
/// Returns `Ok(Confirmed)` if `--yes` was passed, `Ok(PromptUser)` if running
/// on a TTY, or `Err` if not on a TTY and `--yes` was omitted (non-interactive
/// environments must not hang waiting for input).
pub(crate) fn resolve_aggressive_confirm(
    yes: bool,
    is_tty: bool,
) -> crate::Result<AggressiveConfirm> {
    if yes {
        return Ok(AggressiveConfirm::Confirmed);
    }
    if is_tty {
        return Ok(AggressiveConfirm::PromptUser);
    }
    Err(crate::Error::Validation(
        "--aggressive --apply requires --yes in non-interactive environments".into(),
    ))
}

/// Agent decisions older than this without promotion are treated as review
/// debt worth surfacing.
const AGENT_REVIEW_STALE_DAYS: i64 = 90;

/// A decision `gc` has flagged, carrying the reason and its owning component.
struct Candidate {
    name: String,
    component: String,
    /// Human-readable explanation for the report line.
    detail: String,
}

/// Reclaim orphaned decisions, and — under `Aggressive` — stale and
/// long-unreviewed agent decisions too. Every candidate passes a cascade
/// pre-flight; those that would break a dependent or shrink a pattern below
/// its minimum are reported as blocked and left in place.
pub fn gc(cwd: &Path, scope: GcScope, execution: GcExecution) -> Result<()> {
    let (store, lock, mut state) = open_store_mut(cwd)?;

    let (orphaned, stale, old_agent) = classify(&state);
    let surfaced = orphaned.len() + stale.len() + old_agent.len();
    if surfaced == 0 {
        println!("gc: nothing to collect.");
        return Ok(());
    }

    let apply = matches!(execution, GcExecution::Apply);
    let reclaim_extra = matches!(scope, GcScope::Aggressive);
    if !apply {
        println!("Dry run — no changes written.");
    }

    let (removable, blocked) = plan_removals(
        &state,
        &orphaned,
        &stale,
        &old_agent,
        reclaim_extra,
        execution,
    );

    // `removable` is what leaves under Apply and what *would* leave under a dry
    // run; both report the same figure so the summary agrees with the sections.
    let acted = if apply {
        apply_removals(&store, &lock, &mut state, &removable)?
    } else {
        removable.len()
    };
    drop(lock);

    let flagged = surfaced - acted - blocked;
    let verb = if apply { "removed" } else { "would remove" };
    println!("\nSummary: {verb} {acted}, flagged {flagged}, blocked {blocked}");
    if !apply {
        println!("Run again with --apply to write.");
    }
    Ok(())
}

/// Run the batch cascade pre-flight over every candidate that would be removed,
/// print each category's removable/blocked breakdown, and return the flat set
/// cleared for removal plus the count that the pre-flight blocked.
///
/// Orphaned decisions are always removal targets; stale and agent-review debt
/// join them only under `--aggressive`. The pre-flight judges the whole
/// attempted set in one batch-aware pass, so co-removed dependents and pattern
/// members are scored against what actually leaves — removing two members of a
/// shared pattern can never silently drop it below its minimum.
fn plan_removals<'a>(
    state: &ProjectState,
    orphaned: &'a [Candidate],
    stale: &'a [Candidate],
    old_agent: &'a [Candidate],
    reclaim_extra: bool,
    execution: GcExecution,
) -> (Vec<&'a Candidate>, usize) {
    let mut attempted: Vec<&Candidate> = orphaned.iter().collect();
    if reclaim_extra {
        attempted.extend(stale.iter());
        attempted.extend(old_agent.iter());
    }
    let names: Vec<&str> = attempted.iter().map(|c| c.name.as_str()).collect();
    let blocked_reasons: BTreeMap<&str, String> = state
        .graph()
        .partition_removable_decisions(&names)
        .blocked
        .into_iter()
        .collect();

    let (orphan_removable, orphan_blocked) = split_blocked(orphaned, &blocked_reasons);
    let (stale_removable, stale_blocked) = if reclaim_extra {
        split_blocked(stale, &blocked_reasons)
    } else {
        (Vec::new(), Vec::new())
    };
    let (agent_removable, agent_blocked) = if reclaim_extra {
        split_blocked(old_agent, &blocked_reasons)
    } else {
        (Vec::new(), Vec::new())
    };

    print_removal_section("Orphaned", &orphan_removable, &orphan_blocked, execution);
    if reclaim_extra {
        print_removal_section(
            "Stale (all code refs dead)",
            &stale_removable,
            &stale_blocked,
            execution,
        );
        print_removal_section(
            "Agent unreviewed > 90 days",
            &agent_removable,
            &agent_blocked,
            execution,
        );
    } else {
        print_report_section("Stale (all code refs dead)", stale);
        print_report_section("Agent unreviewed > 90 days", old_agent);
    }

    let removable: Vec<&Candidate> = orphan_removable
        .into_iter()
        .chain(stale_removable)
        .chain(agent_removable)
        .collect();
    let blocked = orphan_blocked.len() + stale_blocked.len() + agent_blocked.len();
    (removable, blocked)
}

/// Snapshot the decisions about to leave, remove them in one atomic batch, and
/// report the concern coverage the removal erased. Returns the number removed.
fn apply_removals(
    store: &Store,
    lock: &StoreLock,
    state: &mut ProjectState,
    removable: &[&Candidate],
) -> Result<usize> {
    if removable.is_empty() {
        return Ok(0);
    }

    // Snapshot before removal so the coverage each decision carried can be
    // reported once it is gone from the graph.
    let snapshots: Vec<(String, std::sync::Arc<DecisionFile>)> = removable
        .iter()
        .filter_map(|c| {
            state
                .decisions
                .get(&c.name)
                .map(|d| (c.component.clone(), std::sync::Arc::clone(d)))
        })
        .collect();

    let names: Vec<&str> = removable.iter().map(|c| c.name.as_str()).collect();
    store.remove_decisions(lock, state, &names)?;

    report_lost_coverage(state, &snapshots);
    Ok(names.len())
}

/// Sort every decision into at most one reclaim category. Precedence is
/// structural first: an orphaned decision is reported as orphaned even if its
/// code refs are also dead, and a stale decision is not double-counted as
/// review debt.
fn classify(state: &ProjectState) -> (Vec<Candidate>, Vec<Candidate>, Vec<Candidate>) {
    let cutoff = Utc::now() - Duration::days(AGENT_REVIEW_STALE_DAYS);
    let mut orphaned = Vec::new();
    let mut stale = Vec::new();
    let mut old_agent = Vec::new();

    for (name, dec) in &state.decisions {
        let d = &dec.decision;
        if d.component != "project" && !state.components.contains_key(&d.component) {
            orphaned.push(Candidate {
                name: name.clone(),
                component: d.component.clone(),
                detail: format!("component [{}] no longer exists", d.component),
            });
        } else if crate::store::decision_refs_all_missing(&state.project_root, dec) {
            let files: Vec<&str> = d.code_refs.iter().map(|r| r.file.as_str()).collect();
            stale.push(Candidate {
                name: name.clone(),
                component: d.component.clone(),
                detail: format!("{} deleted", files.join(", ")),
            });
        } else if d.attribution == Attribution::Agent && d.created < cutoff {
            old_agent.push(Candidate {
                name: name.clone(),
                component: d.component.clone(),
                detail: format!("agent, created {}", d.created.format("%Y-%m-%d")),
            });
        }
    }

    (orphaned, stale, old_agent)
}

/// Bucket a category's candidates into those the batch pre-flight cleared and
/// those it blocked (paired with the blocker explanation), by looking each up
/// in the shared `blocked_reasons` map computed once over the full batch.
fn split_blocked<'a>(
    candidates: &'a [Candidate],
    blocked_reasons: &BTreeMap<&str, String>,
) -> (Vec<&'a Candidate>, Vec<(&'a Candidate, String)>) {
    let mut removable = Vec::new();
    let mut blocked = Vec::new();
    for candidate in candidates {
        match blocked_reasons.get(candidate.name.as_str()) {
            Some(reason) => blocked.push((candidate, reason.clone())),
            None => removable.push(candidate),
        }
    }
    (removable, blocked)
}

/// Print the concern coverage the removals erased, grouped by component.
/// Components that no longer exist (the orphan case) are skipped — reporting
/// lost coverage for a deleted component is noise.
fn report_lost_coverage(state: &ProjectState, removed: &[(String, std::sync::Arc<DecisionFile>)]) {
    let mut by_component: BTreeMap<&str, Vec<&DecisionFile>> = BTreeMap::new();
    for (component, dec) in removed {
        by_component.entry(component).or_default().push(dec);
    }

    for (component, removed_decisions) in by_component {
        if !state.components.contains_key(component) {
            continue;
        }
        let remaining = state.graph().coverage_baseline(component);

        let mut lost: Vec<&'static str> = Vec::new();
        for dec in removed_decisions {
            for area in concerns::coverage_lost(dec, &remaining) {
                if !lost.contains(&area) {
                    lost.push(area);
                }
            }
        }
        if !lost.is_empty() {
            println!("\u{26a0} [{component}] lost coverage: {}", lost.join(", "));
        }
    }
}

fn print_removal_section(
    title: &str,
    removable: &[&Candidate],
    blocked: &[(&Candidate, String)],
    execution: GcExecution,
) {
    if removable.is_empty() && blocked.is_empty() {
        return;
    }
    let (verb, mark) = match execution {
        GcExecution::Apply => ("removed", '\u{2713}'),
        GcExecution::DryRun => ("would remove", '\u{26a0}'),
    };
    println!("\n{title} ({verb}):");
    for candidate in removable {
        println!("  {mark} {} ({})", candidate.name, candidate.detail);
    }
    for (candidate, why) in blocked {
        println!(
            "  \u{26a0} {} ({}) \u{2014} blocked: {why}",
            candidate.name, candidate.detail
        );
    }
}

fn print_report_section(title: &str, candidates: &[Candidate]) {
    if candidates.is_empty() {
        return;
    }
    println!("\n{title} (remove with --aggressive):");
    for candidate in candidates {
        println!("  \u{26a0} {} ({})", candidate.name, candidate.detail);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{add_component, decide, init};
    use crate::store::RecordDecisionParams;
    use crate::store::Store;
    use crate::store::schema::{Attribution, CodeRef, Decision, DecisionFile};
    use chrono::Utc;
    use tempfile::TempDir;

    // ── resolve_aggressive_confirm ──────────────────────────────────────

    #[test]
    fn aggressive_confirm_yes_flag_proceeds() {
        let result = resolve_aggressive_confirm(true, false).unwrap();
        assert_eq!(result, AggressiveConfirm::Confirmed);
    }

    #[test]
    fn aggressive_confirm_yes_flag_on_tty_still_proceeds() {
        let result = resolve_aggressive_confirm(true, true).unwrap();
        assert_eq!(result, AggressiveConfirm::Confirmed);
    }

    #[test]
    fn aggressive_confirm_tty_without_yes_prompts() {
        let result = resolve_aggressive_confirm(false, true).unwrap();
        assert_eq!(result, AggressiveConfirm::PromptUser);
    }

    #[test]
    fn aggressive_confirm_no_tty_no_yes_errors() {
        let err = resolve_aggressive_confirm(false, false).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("--yes"), "error must mention --yes: {msg}");
        assert!(
            msg.contains("--aggressive"),
            "error must mention --aggressive: {msg}"
        );
    }

    // ── CLI parsing ─────────────────────────────────────────────────────

    #[test]
    fn cli_bare_gc_defaults_to_dry_run() {
        use crate::cli::{Cli, Command};
        use clap::Parser;
        let cli = Cli::parse_from(["trurlic", "gc"]);
        match cli.command {
            Command::Gc {
                apply,
                aggressive,
                yes,
                ..
            } => {
                assert!(!apply, "bare gc must not apply");
                assert!(!aggressive);
                assert!(!yes);
            }
            other => panic!("expected Gc, got {other:?}"),
        }
    }

    #[test]
    fn cli_gc_apply_flag() {
        use crate::cli::{Cli, Command};
        use clap::Parser;
        let cli = Cli::parse_from(["trurlic", "gc", "--apply"]);
        match cli.command {
            Command::Gc { apply, .. } => assert!(apply),
            other => panic!("expected Gc, got {other:?}"),
        }
    }

    #[test]
    fn cli_gc_aggressive_apply_yes() {
        use crate::cli::{Cli, Command};
        use clap::Parser;
        let cli = Cli::parse_from(["trurlic", "gc", "--aggressive", "--apply", "--yes"]);
        match cli.command {
            Command::Gc {
                apply,
                aggressive,
                yes,
                ..
            } => {
                assert!(apply);
                assert!(aggressive);
                assert!(yes);
            }
            other => panic!("expected Gc, got {other:?}"),
        }
    }

    #[test]
    fn cli_gc_deprecated_dry_run_still_parses() {
        use crate::cli::{Cli, Command};
        use clap::Parser;
        let cli = Cli::parse_from(["trurlic", "gc", "--dry-run"]);
        match cli.command {
            Command::Gc { apply, dry_run, .. } => {
                assert!(!apply, "deprecated --dry-run must not set apply");
                assert!(dry_run, "deprecated flag should still parse");
            }
            other => panic!("expected Gc, got {other:?}"),
        }
    }

    /// Write a decision file straight to disk so tests can set fields the CLI
    /// cannot (a stale code ref, an aged `created`, or a missing component).
    fn plant_decision(store: &Store, name: &str, decision: Decision) {
        let lock = store.lock().unwrap();
        store
            .write_atomic(
                &lock,
                &store.decision_path(name),
                &DecisionFile { decision },
            )
            .unwrap();
    }

    fn base_decision(component: &str, choice: &str) -> Decision {
        Decision {
            component: component.into(),
            choice: choice.into(),
            reason: "Recorded for a garbage-collection test".into(),
            alternatives: vec![],
            tags: vec![],
            attribution: Attribution::User,
            created: Utc::now(),
            code_refs: vec![],
            history: vec![],
        }
    }

    #[test]
    fn gc_removes_orphaned_decisions() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        let store = Store::discover(tmp.path()).unwrap();

        // Two decisions whose component was never added — orphaned on load.
        plant_decision(&store, "widget-a", base_decision("widget", "Grid layout"));
        plant_decision(&store, "widget-b", base_decision("widget", "Dark theme"));

        gc(tmp.path(), GcScope::Safe, GcExecution::Apply).unwrap();

        let state = Store::discover(tmp.path()).unwrap().load_state().unwrap();
        assert!(state.decisions.is_empty(), "orphans must be reclaimed");
    }

    #[test]
    fn gc_safe_flags_but_keeps_stale_and_old_agent() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();
        let store = Store::discover(tmp.path()).unwrap();

        let mut stale = base_decision("auth", "Custom XML parser");
        stale.code_refs = vec![CodeRef {
            file: "src/parsers/xml.rs".into(),
            symbol: None,
        }];
        plant_decision(&store, "xml-parser", stale);

        let mut old = base_decision("auth", "Auto-detected cache layer");
        old.attribution = Attribution::Agent;
        old.created = Utc::now() - Duration::days(120);
        plant_decision(&store, "auto-cache", old);

        gc(tmp.path(), GcScope::Safe, GcExecution::Apply).unwrap();

        // Safe mode only reports these — nothing removed.
        let state = Store::discover(tmp.path()).unwrap().load_state().unwrap();
        assert!(state.decisions.contains_key("xml-parser"));
        assert!(state.decisions.contains_key("auto-cache"));
    }

    #[test]
    fn gc_aggressive_removes_stale_and_old_agent() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();
        let store = Store::discover(tmp.path()).unwrap();

        let mut stale = base_decision("auth", "Custom XML parser");
        stale.code_refs = vec![CodeRef {
            file: "src/parsers/xml.rs".into(),
            symbol: None,
        }];
        plant_decision(&store, "xml-parser", stale);

        let mut old = base_decision("auth", "Auto-detected cache layer");
        old.attribution = Attribution::Agent;
        old.created = Utc::now() - Duration::days(120);
        plant_decision(&store, "auto-cache", old);

        gc(tmp.path(), GcScope::Aggressive, GcExecution::Apply).unwrap();

        let state = Store::discover(tmp.path()).unwrap().load_state().unwrap();
        assert!(!state.decisions.contains_key("xml-parser"));
        assert!(!state.decisions.contains_key("auto-cache"));
    }

    #[test]
    fn gc_keeps_live_agent_decisions() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();
        let store = Store::discover(tmp.path()).unwrap();

        // Recent agent decision — under the review-debt threshold.
        let mut recent = base_decision("auth", "Recent agent choice");
        recent.attribution = Attribution::Agent;
        recent.created = Utc::now() - Duration::days(3);
        plant_decision(&store, "recent-agent", recent);

        gc(tmp.path(), GcScope::Aggressive, GcExecution::Apply).unwrap();

        let state = Store::discover(tmp.path()).unwrap().load_state().unwrap();
        assert!(
            state.decisions.contains_key("recent-agent"),
            "a recently-created agent decision is not review debt"
        );
    }

    #[test]
    fn gc_dry_run_removes_nothing() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        let store = Store::discover(tmp.path()).unwrap();
        plant_decision(&store, "widget-a", base_decision("widget", "Grid layout"));

        gc(tmp.path(), GcScope::Aggressive, GcExecution::DryRun).unwrap();

        let state = Store::discover(tmp.path()).unwrap().load_state().unwrap();
        assert!(
            state.decisions.contains_key("widget-a"),
            "dry run must not delete"
        );
    }

    #[test]
    fn gc_skips_cascade_blocked_orphans() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();
        decide(
            tmp.path(),
            "auth",
            "Live decision",
            "Depends on the orphan",
            &[],
            &[],
        )
        .unwrap();

        let store = Store::discover(tmp.path()).unwrap();

        // Orphan referenced by a live decision through a DependsOn edge: its
        // component is gone, but removing it would break `live-decision`.
        plant_decision(
            &store,
            "orphan-dep",
            base_decision("widget", "Orphaned base"),
        );
        let lock = store.lock().unwrap();
        let mut state = store.load_state().unwrap();
        state
            .graph_index
            .edges
            .push(crate::store::schema::EdgeEntry {
                from: "live-decision".into(),
                to: "orphan-dep".into(),
                kind: crate::store::schema::EdgeKind::DependsOn,
            });
        store
            .commit_batch(&lock, vec![], vec![], Some(state.graph_index.clone()))
            .unwrap();
        drop(lock);

        gc(tmp.path(), GcScope::Safe, GcExecution::Apply).unwrap();

        // The dependent kept the orphan alive.
        let state = Store::discover(tmp.path()).unwrap().load_state().unwrap();
        assert!(
            state.decisions.contains_key("orphan-dep"),
            "a cascade-blocked orphan must be left in place"
        );
    }

    #[test]
    fn gc_reports_no_candidates_cleanly() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();
        let store = Store::discover(tmp.path()).unwrap();
        let lock = store.lock().unwrap();
        let mut state = store.load_state().unwrap();
        store
            .record_decision(
                &lock,
                &mut state,
                RecordDecisionParams {
                    component: "auth",
                    choice: "A healthy decision",
                    reason: "Nothing to collect here",
                    depends_on: &[],
                    alternatives: &[],
                    constrains: &[],
                    tags: &[],
                    attribution: Attribution::User,
                    code_refs: &[],
                },
            )
            .unwrap();
        drop(lock);

        // A clean store yields no removals and no error.
        gc(tmp.path(), GcScope::Aggressive, GcExecution::Apply).unwrap();

        let state = store.load_state().unwrap();
        assert_eq!(state.decisions.len(), 1);
    }
}
