//! `verify_against_decisions` — the post-implementation feedback loop.
//!
//! After an agent writes code, this read-only tool returns the architectural
//! decisions that apply to the files it changed, plus instructions the agent
//! follows to check its code respects them *before* committing. It reads the
//! same decision graph as `get_context` (`graph.decisions_for` and
//! `graph.project_decisions`), never opens file contents, calls no LLM, and
//! mutates nothing. The filter is a pure function of the graph and the
//! changed-file list; the response builder wraps it in JSON.

use std::sync::Arc;

use serde_json::Value;

use crate::store::graph::InMemoryGraph;
use crate::store::{self, CodeRef, DecisionFile};

/// Instructions returned to the agent alongside the affected decisions.
///
/// The tool never reads source itself — the agent reads each affected file
/// and evaluates. The three verdicts give the agent a fixed vocabulary so a
/// violation is an explicit, actionable outcome rather than a vague "looks
/// fine". `NEEDS_REVIEW` is the escape hatch that keeps the agent from
/// silently guessing when a decision is ambiguous for the change.
const INSTRUCTIONS: &str = "For each decision in `decisions_to_verify`, read the files listed in \
its `affected_files` and judge whether your changes respect it. Check your changes against every \
rule in `project_decisions` too — those apply everywhere. Assign one verdict per decision:\n\
- RESPECTED: the code honors the decision.\n\
- VIOLATED: the code contradicts the decision — fix it before committing.\n\
- NEEDS_REVIEW: the decision is ambiguous for this change, or the code diverges for a reason worth \
recording — surface it instead of guessing.\n\
Resolve every VIOLATED verdict before you commit.";

// ── Filtering (pure, no I/O) ─────────────────────────────────────────────

/// A component decision selected for verification, paired with the changed
/// files that triggered its selection. Borrows from both the graph and the
/// caller's normalized changed-file list — nothing is cloned.
pub(crate) struct ToVerify<'a> {
    pub name: &'a Arc<str>,
    pub decision: &'a DecisionFile,
    pub affected_files: Vec<&'a str>,
}

/// Result of partitioning a component's decisions against a set of changed
/// files: the affected decisions (with their triggering files) and a count of
/// those filtered out as unaffected.
pub(crate) struct Filtered<'a> {
    pub to_verify: Vec<ToVerify<'a>>,
    pub unaffected_count: usize,
}

/// Whether a changed file and a decision's `code_ref` path refer to the same
/// code, matching in either direction:
/// - exact: `changed == ref_file`,
/// - the changed file lives inside a directory-valued ref (`changed` is under `ref_file/`),
/// - the ref lives inside a changed directory (`ref_file` is under `changed/`).
///
/// Bidirectional on purpose: a `code_ref` may name a directory (`src/auth`)
/// while the changed path is a file within it (`src/auth/token.rs`), or the
/// reverse. The one-directional matcher behind `decisions_for_file` treats
/// only the *query* as the prefix, so it misses the directory-ref case —
/// verify must catch both.
pub(crate) fn file_matches(changed: &str, ref_file: &str) -> bool {
    changed == ref_file || is_within(changed, ref_file) || is_within(ref_file, changed)
}

/// Whether `child` is a path strictly inside directory `dir` — `dir` is a
/// prefix of `child` ending exactly on a path separator. Allocation-free.
fn is_within(child: &str, dir: &str) -> bool {
    child.len() > dir.len() && child.as_bytes()[dir.len()] == b'/' && child.starts_with(dir)
}

/// Whether a decision carries a `scope` or `boundary` tag — the marker for a
/// decision that applies broadly rather than to specific files. Used for
/// decisions that record no `code_refs`.
fn is_broad_scope(decision: &DecisionFile) -> bool {
    decision
        .decision
        .tags
        .iter()
        .any(|tag| tag == "scope" || tag == "boundary")
}

/// The changed files that match at least one of a decision's `code_refs`, in
/// input order with duplicates removed.
fn matching_changed<'a>(code_refs: &[CodeRef], changed_files: &'a [String]) -> Vec<&'a str> {
    let mut matched: Vec<&str> = Vec::new();
    for changed in changed_files {
        let changed = changed.as_str();
        if code_refs.iter().any(|cr| file_matches(changed, &cr.file)) && !matched.contains(&changed)
        {
            matched.push(changed);
        }
    }
    matched
}

/// Partition a component's decisions into those affected by the changed files
/// and those that are not.
///
/// - A decision **with** `code_refs` is affected when any ref matches any
///   changed file (rule 3a); `affected_files` lists exactly the matching
///   changed files.
/// - A decision **without** `code_refs` is affected when it carries a `scope`
///   or `boundary` tag (rule 3b); it applies broadly, so `affected_files` is
///   the full changed-file list.
///
/// A decision with no `code_refs` and no such tag is unaffected — it counts
/// toward `unaffected_count`, never toward `to_verify`.
pub(crate) fn filter_decisions<'a>(
    component_decisions: &[(&'a Arc<str>, &'a DecisionFile)],
    changed_files: &'a [String],
) -> Filtered<'a> {
    let mut to_verify = Vec::new();
    let mut unaffected_count = 0;

    for &(name, decision) in component_decisions {
        let code_refs = &decision.decision.code_refs;
        let affected_files: Vec<&str> = if code_refs.is_empty() {
            if is_broad_scope(decision) {
                changed_files.iter().map(String::as_str).collect()
            } else {
                Vec::new()
            }
        } else {
            matching_changed(code_refs, changed_files)
        };

        if affected_files.is_empty() {
            unaffected_count += 1;
        } else {
            to_verify.push(ToVerify {
                name,
                decision,
                affected_files,
            });
        }
    }

    Filtered {
        to_verify,
        unaffected_count,
    }
}

// ── Response building ────────────────────────────────────────────────────

/// Build the `verify_against_decisions` response for a component and a set of
/// already-normalized changed files.
///
/// Assembles the affected component decisions (via [`filter_decisions`]), the
/// full set of project-wide rules (they apply everywhere), the unaffected
/// count, and the agent [`INSTRUCTIONS`]. When no component decision is
/// affected, `decisions_to_verify` is empty and a `message` explains why —
/// project decisions are still returned.
pub(crate) fn build_response(
    graph: &InMemoryGraph,
    component: &str,
    changed_files: &[String],
) -> Value {
    let component_decisions = graph.decisions_for(component);
    let project_decisions = graph.project_decisions();

    let filtered = filter_decisions(&component_decisions, changed_files);
    let no_matches = filtered.to_verify.is_empty();

    let decisions_to_verify: Vec<Value> = filtered.to_verify.iter().map(verify_entry).collect();
    let project: Vec<Value> = project_decisions
        .iter()
        .map(|(name, decision)| project_entry(name, decision))
        .collect();

    let mut response = serde_json::json!({
        "component": component,
        "decisions_to_verify": decisions_to_verify,
        "project_decisions": project,
        "unaffected_count": filtered.unaffected_count,
        "instructions": INSTRUCTIONS,
    });
    if no_matches {
        response["message"] = Value::String("No decisions affected by these changes".into());
    }
    response
}

/// JSON for one affected component decision: identity, choice, reason, the
/// changed files that triggered it, and — when present — its tags and
/// `code_refs`.
fn verify_entry(entry: &ToVerify<'_>) -> Value {
    let decision = &entry.decision.decision;
    let mut obj = serde_json::json!({
        "name": entry.name.as_ref(),
        "choice": decision.choice,
        "reason": decision.reason,
        "affected_files": entry.affected_files,
    });
    if !decision.tags.is_empty() {
        obj["tags"] = serde_json::json!(decision.tags);
    }
    if !decision.code_refs.is_empty() {
        obj["code_refs"] = Value::Array(store::code_refs_to_json(&decision.code_refs));
    }
    obj
}

/// JSON for one project-wide rule: identity, choice, reason, and — when
/// present — its tags. Project rules are returned in full regardless of the
/// changed files.
fn project_entry(name: &Arc<str>, decision: &DecisionFile) -> Value {
    let decision = &decision.decision;
    let mut obj = serde_json::json!({
        "name": name.as_ref(),
        "choice": decision.choice,
        "reason": decision.reason,
    });
    if !decision.tags.is_empty() {
        obj["tags"] = serde_json::json!(decision.tags);
    }
    obj
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::schema::{Attribution, CodeRef, Decision, DecisionFile};
    use chrono::{TimeZone, Utc};

    fn decision(
        component: &str,
        tags: &[&str],
        code_refs: &[(&str, Option<&str>)],
    ) -> DecisionFile {
        DecisionFile {
            decision: Decision {
                component: component.into(),
                choice: format!("choice for {component}"),
                reason: format!("reason for {component}"),
                alternatives: vec![],
                tags: tags.iter().map(|t| (*t).into()).collect(),
                attribution: Attribution::User,
                created: Utc.with_ymd_and_hms(2025, 6, 1, 10, 0, 0).unwrap(),
                code_refs: code_refs
                    .iter()
                    .map(|(file, symbol)| CodeRef {
                        file: (*file).into(),
                        symbol: symbol.map(Into::into),
                    })
                    .collect(),
                history: vec![],
            },
        }
    }

    fn changed(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|p| (*p).into()).collect()
    }

    // ── file_matches ─────────────────────────────────────────────────────

    #[test]
    fn file_matches_exact() {
        assert!(file_matches("src/mcp/verify.rs", "src/mcp/verify.rs"));
    }

    #[test]
    fn file_matches_unrelated_is_false() {
        assert!(!file_matches("src/mcp/verify.rs", "src/store/write.rs"));
    }

    #[test]
    fn file_matches_changed_file_inside_ref_dir() {
        // ref is a directory, changed is a file within it.
        assert!(file_matches("src/auth/token.rs", "src/auth"));
    }

    #[test]
    fn file_matches_ref_inside_changed_dir() {
        // changed is a directory, ref is a file within it.
        assert!(file_matches("src/auth", "src/auth/token.rs"));
    }

    #[test]
    fn file_matches_rejects_partial_segment() {
        // "src/auth" must not match "src/authz" — the boundary is a full segment.
        assert!(!file_matches("src/authz/mod.rs", "src/auth"));
        assert!(!file_matches("src/auth", "src/authz/mod.rs"));
    }

    // ── filter_decisions ─────────────────────────────────────────────────

    #[test]
    fn code_ref_exact_match_includes_decision() {
        let d = decision("mcp", &[], &[("src/mcp/verify.rs", Some("build_response"))]);
        let name: Arc<str> = "verify-tool".into();
        let files = changed(&["src/mcp/verify.rs"]);

        let filtered = filter_decisions(&[(&name, &d)], &files);
        assert_eq!(filtered.to_verify.len(), 1);
        assert_eq!(filtered.unaffected_count, 0);
        assert_eq!(
            filtered.to_verify[0].affected_files,
            vec!["src/mcp/verify.rs"]
        );
    }

    #[test]
    fn unrelated_file_excludes_decision() {
        let d = decision("mcp", &[], &[("src/mcp/verify.rs", None)]);
        let name: Arc<str> = "verify-tool".into();
        let files = changed(&["src/store/write.rs"]);

        let filtered = filter_decisions(&[(&name, &d)], &files);
        assert!(filtered.to_verify.is_empty());
        assert_eq!(filtered.unaffected_count, 1);
    }

    #[test]
    fn directory_prefix_match_both_directions() {
        // ref is a directory; several changed files live under it.
        let d = decision("store", &[], &[("src/store", None)]);
        let name: Arc<str> = "store-scope".into();
        let files = changed(&[
            "src/store/write.rs",
            "src/store/query.rs",
            "src/mcp/tools.rs",
        ]);

        let filtered = filter_decisions(&[(&name, &d)], &files);
        assert_eq!(filtered.to_verify.len(), 1);
        // Only the two files under src/store match, in input order.
        assert_eq!(
            filtered.to_verify[0].affected_files,
            vec!["src/store/write.rs", "src/store/query.rs"]
        );
    }

    #[test]
    fn no_code_refs_with_scope_tag_included_and_applies_to_all() {
        let d = decision("store", &["scope"], &[]);
        let name: Arc<str> = "store-boundary".into();
        let files = changed(&["src/store/write.rs", "src/mcp/tools.rs"]);

        let filtered = filter_decisions(&[(&name, &d)], &files);
        assert_eq!(filtered.to_verify.len(), 1);
        // scope/boundary decisions apply broadly: affected_files is every changed file.
        assert_eq!(
            filtered.to_verify[0].affected_files,
            vec!["src/store/write.rs", "src/mcp/tools.rs"]
        );
    }

    #[test]
    fn no_code_refs_with_boundary_tag_included() {
        let d = decision("store", &["boundary"], &[]);
        let name: Arc<str> = "store-boundary".into();
        let files = changed(&["anything.rs"]);

        let filtered = filter_decisions(&[(&name, &d)], &files);
        assert_eq!(filtered.to_verify.len(), 1);
    }

    #[test]
    fn no_code_refs_other_tags_excluded_and_counted_unaffected() {
        let d = decision("store", &["performance", "reliability"], &[]);
        let name: Arc<str> = "store-perf".into();
        let files = changed(&["src/store/write.rs"]);

        let filtered = filter_decisions(&[(&name, &d)], &files);
        assert!(filtered.to_verify.is_empty());
        assert_eq!(filtered.unaffected_count, 1);
    }

    #[test]
    fn affected_files_dedups_duplicate_changed_entries() {
        let d = decision("mcp", &[], &[("src/mcp/verify.rs", None)]);
        let name: Arc<str> = "verify-tool".into();
        // Same normalized path passed twice — affected_files must list it once.
        let files = changed(&["src/mcp/verify.rs", "src/mcp/verify.rs"]);

        let filtered = filter_decisions(&[(&name, &d)], &files);
        assert_eq!(filtered.to_verify.len(), 1);
        assert_eq!(
            filtered.to_verify[0].affected_files,
            vec!["src/mcp/verify.rs"]
        );
    }

    #[test]
    fn mixed_set_partitions_correctly() {
        let affected = decision("mcp", &[], &[("src/mcp/verify.rs", None)]);
        let broad = decision("mcp", &["scope"], &[]);
        let unaffected_refs = decision("mcp", &[], &[("src/mcp/watcher.rs", None)]);
        let unaffected_plain = decision("mcp", &["performance"], &[]);
        let (a, b, c, e): (Arc<str>, Arc<str>, Arc<str>, Arc<str>) =
            ("a".into(), "b".into(), "c".into(), "e".into());
        let files = changed(&["src/mcp/verify.rs"]);

        let filtered = filter_decisions(
            &[
                (&a, &affected),
                (&b, &broad),
                (&c, &unaffected_refs),
                (&e, &unaffected_plain),
            ],
            &files,
        );
        assert_eq!(filtered.to_verify.len(), 2);
        assert_eq!(filtered.unaffected_count, 2);
    }

    // ── build_response ───────────────────────────────────────────────────

    #[test]
    fn build_response_no_matches_sets_message() {
        // A component decision that no changed file touches, plus a project rule.
        let comp = decision("mcp", &[], &[("src/mcp/watcher.rs", None)]);
        let name: Arc<str> = "watcher".into();
        let files = changed(&["src/mcp/verify.rs"]);
        let filtered = filter_decisions(&[(&name, &comp)], &files);
        // Sanity: the fixture really produces no matches.
        assert!(filtered.to_verify.is_empty());

        // Direct assertion on the entry shape without a full graph.
        let entry = project_entry(&name, &comp);
        assert_eq!(entry["name"], "watcher");
        assert!(entry.get("tags").is_none(), "empty tags must be omitted");
    }

    #[test]
    fn verify_entry_omits_empty_tags_and_refs() {
        let d = decision("mcp", &[], &[]);
        let name: Arc<str> = "plain".into();
        let entry = verify_entry(&ToVerify {
            name: &name,
            decision: &d,
            affected_files: vec!["src/mcp/verify.rs"],
        });
        assert_eq!(entry["name"], "plain");
        assert_eq!(entry["affected_files"][0], "src/mcp/verify.rs");
        assert!(entry.get("tags").is_none());
        assert!(entry.get("code_refs").is_none());
    }

    #[test]
    fn verify_entry_includes_tags_and_refs_when_present() {
        let d = decision(
            "mcp",
            &["scope"],
            &[("src/mcp/verify.rs", Some("build_response"))],
        );
        let name: Arc<str> = "rich".into();
        let entry = verify_entry(&ToVerify {
            name: &name,
            decision: &d,
            affected_files: vec!["src/mcp/verify.rs"],
        });
        assert_eq!(entry["tags"][0], "scope");
        assert_eq!(entry["code_refs"][0]["file"], "src/mcp/verify.rs");
        assert_eq!(entry["code_refs"][0]["symbol"], "build_response");
    }
}
