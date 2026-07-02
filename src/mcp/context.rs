use std::collections::HashSet;
use std::sync::Arc;

use serde_json::Value;

use crate::store::graph::InMemoryGraph;
use crate::store::schema::{Attribution, EdgeKind};
use crate::store::{self, DecisionFile, PatternFile, ProjectState};
use crate::workflow::concerns;

// ── Context depth ──────────────────────────────────────────────────────────

/// Controls how much detail `get_context` returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContextDepth {
    /// Full context: decisions with reasoning, related decisions,
    /// transitive dependencies, patterns, and verbose brief.
    Full,
    /// Constraints only: choice text (no reasoning/alternatives),
    /// no related decisions, compact brief. ~60-70% fewer tokens.
    Constraints,
}

// ── get_context ──────────────────────────────────────────────────────────

/// Assemble a tailored spec for a component: its decisions, project-wide
/// rules, related decisions from connected components, applicable patterns,
/// and a pre-assembled authoritative brief.
///
/// `depth` controls verbosity: `Full` returns everything; `Constraints`
/// strips reasoning, alternatives, and related decisions for ~60-70%
/// fewer tokens on mid-implementation constraint checks.
pub(crate) fn get_context(
    state: &ProjectState,
    component: &str,
    task_description: Option<&str>,
    depth: ContextDepth,
) -> Result<Value, String> {
    let graph = state.graph();

    if component == "project" {
        return Ok(project_context(state, graph, task_description));
    }

    let comp = state
        .components
        .get(component)
        .ok_or_else(|| format!("component `{component}` does not exist"))?;

    let connects_to = graph.connects_to(component);
    let connects_from = graph.connects_from(component);

    let component_decisions = graph.decisions_for(component);
    let project_decisions = graph.project_decisions();
    let patterns = graph.patterns_for(component);

    // Concern coverage: project rules + component decisions.
    let coverage_decisions: Vec<&crate::store::DecisionFile> = project_decisions
        .iter()
        .chain(component_decisions.iter())
        .map(|(_, d)| *d)
        .collect();
    let (covered_concerns, uncovered_concerns) =
        concerns::compute_concern_coverage(&coverage_decisions);

    let status = if !component_decisions.is_empty() {
        "covered"
    } else if !project_decisions.is_empty() {
        "partially_covered"
    } else {
        "not_covered"
    };

    match depth {
        ContextDepth::Full => {
            let related_decisions = graph.related_decisions(component);
            let seeds: Vec<&str> = component_decisions
                .iter()
                .map(|(name, _)| name.as_ref())
                .collect();
            let transitive_deps = graph.transitive_depends_on(&seeds, 3);

            let brief = build_brief(&BriefParams {
                component,
                task: task_description,
                component_decisions: &component_decisions,
                project_decisions: &project_decisions,
                related_decisions: &related_decisions,
                transitive_deps: &transitive_deps,
                patterns: &patterns,
                uncovered_concerns: &uncovered_concerns,
            });

            let mut seen: HashSet<&str> =
                HashSet::with_capacity(component_decisions.len() + related_decisions.len());
            for (name, _) in &component_decisions {
                seen.insert(name);
            }
            let combined_related: Vec<String> = related_decisions
                .iter()
                .chain(transitive_deps.iter())
                .filter(|(name, _)| seen.insert(name))
                .map(|(_, d)| {
                    format!(
                        "{}: {} ({})",
                        d.decision.component, d.decision.choice, d.decision.reason
                    )
                })
                .collect();

            Ok(serde_json::json!({
                "component": {
                    "name": comp.component.name,
                    "description": comp.component.description,
                    "connects_to": connects_to,
                    "connects_from": connects_from,
                },
                "decisions": format_decisions(&component_decisions),
                "project_rules": project_decisions.iter()
                    .map(|(_, d)| &d.decision.choice)
                    .collect::<Vec<_>>(),
                "patterns": format_patterns(&patterns),
                "related_decisions": combined_related,
                "concern_coverage": {
                    "covered": covered_concerns,
                    "uncovered": uncovered_concerns,
                },
                "brief": brief,
                "status": status,
            }))
        }
        ContextDepth::Constraints => {
            let brief = build_brief_compact(
                component,
                task_description,
                &component_decisions,
                &project_decisions,
                &patterns,
                &uncovered_concerns,
            );

            Ok(serde_json::json!({
                "component": { "name": comp.component.name },
                "decisions": format_decisions_compact(&component_decisions),
                "project_rules": project_decisions.iter()
                    .map(|(_, d)| &d.decision.choice)
                    .collect::<Vec<_>>(),
                "patterns": patterns.iter()
                    .map(|(name, _)| name.as_ref())
                    .collect::<Vec<_>>(),
                "brief": brief,
                "status": status,
            }))
        }
    }
}

fn project_context(
    state: &ProjectState,
    graph: &InMemoryGraph,
    task_description: Option<&str>,
) -> Value {
    let project_decisions = graph.project_decisions();

    let mut brief = String::with_capacity(256);
    if let Some(task) = task_description {
        brief.push_str(&format!("TASK: {task}\n\n"));
    }
    if project_decisions.is_empty() {
        brief.push_str("No project-wide decisions recorded yet.\n");
    } else {
        brief.push_str("PROJECT-WIDE RULES:\n");
        for (_, d) in &project_decisions {
            brief.push_str(&format!("- {}\n", d.decision.choice));
        }
    }
    brief.push_str("\nWHEN UNCERTAIN:\n");
    brief.push_str("STOP. Ask the user to design project-wide rules first.\n");

    let status = if project_decisions.is_empty() {
        "not_covered"
    } else {
        "covered"
    };

    serde_json::json!({
        "component": {
            "name": "project",
            "description": state.project.project.description,
        },
        "decisions": format_decisions(&project_decisions),
        "project_rules": project_decisions.iter()
            .map(|(_, d)| &d.decision.choice).collect::<Vec<_>>(),
        "patterns": [],
        "related_decisions": [],
        "brief": brief,
        "status": status,
    })
}

// ── build_brief ──────────────────────────────────────────────────────────

struct BriefParams<'a> {
    component: &'a str,
    task: Option<&'a str>,
    component_decisions: &'a [(&'a Arc<str>, &'a DecisionFile)],
    project_decisions: &'a [(&'a Arc<str>, &'a DecisionFile)],
    related_decisions: &'a [(&'a Arc<str>, &'a DecisionFile)],
    transitive_deps: &'a [(&'a Arc<str>, &'a DecisionFile)],
    patterns: &'a [(&'a Arc<str>, &'a PatternFile)],
    uncovered_concerns: &'a [&'a str],
}

/// Format the authoritative brief that coding agents consume directly.
///
/// When `uncovered_concerns` outnumber the covered ones, the brief opens
/// with a degradation warning — belt-and-suspenders for agents that
/// bypass `advance`.
fn build_brief(p: &BriefParams<'_>) -> String {
    let mut brief = String::with_capacity(512);

    // Degradation warning: when more concerns are uncovered than covered,
    // the design is incomplete and the agent should proceed with caution.
    let covered_count = concerns::CONCERNS.len() - p.uncovered_concerns.len();
    if p.uncovered_concerns.len() > covered_count {
        brief.push_str(&format!(
            "\u{26a0} DESIGN INCOMPLETE \u{2014} {} concern areas have no decisions:\n  {}\n\n\
             Proceed with caution. Existing decisions below are constraints, \
             but expect gaps.\n\n",
            p.uncovered_concerns.len(),
            p.uncovered_concerns.join(", "),
        ));
    }

    if let Some(task) = p.task {
        brief.push_str(&format!("TASK: {task}\n\n"));
    }

    if !p.project_decisions.is_empty() {
        brief.push_str("RULES (inviolable — every generated line must respect these):\n");
        for (_, d) in p.project_decisions {
            brief.push_str(&format!("- {}\n", d.decision.choice));
        }
        brief.push('\n');
    }

    if !p.patterns.is_empty() {
        brief.push_str("PATTERNS:\n");
        for (name, pat) in p.patterns {
            brief.push_str(&format!(
                "- {}: {}\n",
                name.as_ref(),
                pat.pattern.description
            ));
        }
        brief.push('\n');
    }

    brief.push_str(&format!("COMPONENT: {}\n", p.component));
    if p.component_decisions.is_empty() {
        brief.push_str("- No decisions recorded yet.\n");
    } else {
        for (_, d) in p.component_decisions {
            let suffix = attribution_suffix(d.decision.attribution);
            brief.push_str(&format!(
                "- {} ({}){}\n",
                d.decision.choice, d.decision.reason, suffix
            ));
            if !d.decision.code_refs.is_empty() {
                brief.push_str(&format!(
                    "  Code: {}\n",
                    store::format_code_refs(&d.decision.code_refs)
                ));
            }
        }
    }
    brief.push('\n');

    if !p.transitive_deps.is_empty() {
        brief.push_str("DEPENDENCIES:\n");
        for (_, d) in p.transitive_deps {
            brief.push_str(&format!(
                "- {}: {} ({})\n",
                d.decision.component, d.decision.choice, d.decision.reason
            ));
        }
        brief.push('\n');
    }

    if !p.related_decisions.is_empty() {
        brief.push_str("RELATED:\n");
        for (_, d) in p.related_decisions {
            brief.push_str(&format!(
                "- {}: {}\n",
                d.decision.component, d.decision.choice
            ));
        }
        brief.push('\n');
    }

    brief.push_str(
        "OVERRIDE POLICY:\n\
         RULES are inviolable. Component decisions are strong defaults —\n\
         follow them unless the user explicitly revises them in a design session.\n\
         Never silently deviate from either.\n\n",
    );

    brief.push_str("WHEN UNCERTAIN:\n");
    brief.push_str(
        "STOP. Call check_pattern to verify coverage. If not covered,\n\
         ask the user to design it first. Never silently deviate.\n",
    );

    brief
}

// ── check_pattern ────────────────────────────────────────────────────────

/// Check whether a pattern or approach is covered by existing decisions.
///
/// Enhanced matching: keywords against decision content + node tags.
/// Pattern membership (via MemberOf edges) boosts ranking.
pub(crate) fn check_pattern(state: &ProjectState, description: &str) -> Value {
    let query_words = extract_words(description);
    if query_words.is_empty() {
        return serde_json::json!({
            "status": "not_covered",
            "message": "Description too short or vague. Provide more detail \
                        about the pattern to check.",
            "decisions": [],
            "patterns": [],
        });
    }

    let graph = state.graph();

    struct Match<'a> {
        score: usize,
        in_pattern: bool,
        name: &'a str,
        dec: &'a DecisionFile,
    }

    let mut matches: Vec<Match<'_>> = Vec::with_capacity(state.decisions.len());

    for (name, dec) in &state.decisions {
        let decision_words = extract_words_from(&[
            &dec.decision.choice,
            &dec.decision.reason,
            &dec.decision.component,
        ]);

        let keyword_hits = query_words
            .iter()
            .filter(|qw| decision_words.iter().any(|dw| dw == *qw))
            .count();

        // Tag hits (weighted 2×) from graph node metadata.
        let tag_hits = graph
            .node_meta(name)
            .map(|m| {
                query_words
                    .iter()
                    .filter(|qw| m.tags.iter().any(|t| t.as_ref() == qw.as_str()))
                    .count()
            })
            .unwrap_or(0);

        let score = keyword_hits + tag_hits * 2;
        if score == 0 {
            continue;
        }

        let in_pattern = graph.is_pattern_member(name);

        matches.push(Match {
            score,
            in_pattern,
            name,
            dec,
        });
    }

    // Pattern members first, then by score descending.
    matches.sort_by(|a, b| b.in_pattern.cmp(&a.in_pattern).then(b.score.cmp(&a.score)));

    // Collect patterns from matched decisions via targeted reverse-MemberOf lookup.
    let mut matched_patterns: Vec<Value> = Vec::new();
    let mut seen_patterns: HashSet<&str> = HashSet::new();
    for m in &matches {
        for (pat_name, pat) in graph.patterns_containing(m.name) {
            if seen_patterns.insert(pat_name) {
                matched_patterns.push(serde_json::json!({
                    "name": pat_name.as_ref(),
                    "description": pat.pattern.description,
                }));
            }
        }
    }

    if matches.is_empty() {
        // Suggest the most relevant component for a design session by
        // matching query words against component names and descriptions.
        let suggested_component = suggest_component_for(state, &query_words);

        serde_json::json!({
            "status": "not_covered",
            "message": "No existing decisions cover this pattern.",
            "decisions": [],
            "patterns": [],
            "suggested_component": suggested_component,
        })
    } else {
        serde_json::json!({
            "status": "covered",
            "message": "This pattern is addressed by existing decisions.",
            "decisions": matches.iter().map(|m| {
                serde_json::json!({
                    "name": m.name,
                    "component": m.dec.decision.component,
                    "choice": m.dec.decision.choice,
                    "reason": m.dec.decision.reason,
                })
            }).collect::<Vec<_>>(),
            "patterns": matched_patterns,
        })
    }
}

// ── get_architecture ─────────────────────────────────────────────────────

pub(crate) fn get_architecture(state: &ProjectState) -> Value {
    let graph = state.graph();

    // Pre-compute project rules (shared across all components for coverage).
    let project_rules = graph.project_decisions();
    let project_rule_refs: Vec<&DecisionFile> = project_rules.iter().map(|(_, d)| *d).collect();

    let components: Vec<Value> = state
        .components
        .iter()
        .map(|(name, comp)| {
            let comp_decisions = graph.decisions_for(name);
            let decision_count = comp_decisions.len();
            let connects_to = graph.connects_to(name);

            // Concern coverage: project rules + this component's decisions.
            let all_decs: Vec<&DecisionFile> = project_rule_refs
                .iter()
                .copied()
                .chain(comp_decisions.iter().map(|(_, d)| *d))
                .collect();
            let (covered, uncovered) = concerns::compute_concern_coverage(&all_decs);

            serde_json::json!({
                "name": name,
                "description": comp.component.description,
                "connects_to": connects_to,
                "decision_count": decision_count,
                "concern_coverage": {
                    "covered": covered.len(),
                    "uncovered": uncovered.len(),
                    "uncovered_concerns": uncovered,
                },
            })
        })
        .collect();

    let patterns: Vec<Value> = state
        .patterns
        .iter()
        .map(|(name, pat)| {
            let decision_count = graph.forward_edge_count(name, EdgeKind::MemberOf);
            let component_count = graph.forward_edge_count(name, EdgeKind::AppliesTo);

            serde_json::json!({
                "name": name,
                "description": pat.pattern.description,
                "decision_count": decision_count,
                "component_count": component_count,
            })
        })
        .collect();

    let project_decisions: Vec<Value> = project_rules
        .iter()
        .map(|(name, d)| {
            serde_json::json!({
                "name": name.as_ref(),
                "choice": d.decision.choice,
                "reason": d.decision.reason,
            })
        })
        .collect();

    let undesigned: Vec<&str> = state
        .components
        .iter()
        .filter(|(name, _)| graph.decisions_for(name).is_empty())
        .map(|(name, _)| name.as_str())
        .collect();

    serde_json::json!({
        "project": {
            "name": state.project.project.name,
            "description": state.project.project.description,
        },
        "components": components,
        "patterns": patterns,
        "project_decisions": project_decisions,
        "undesigned_components": undesigned,
        "total_components": state.components.len(),
        "total_decisions": state.decisions.len(),
        "total_patterns": state.patterns.len(),
    })
}

// ── get_decision_history ───────────────────────────────────────────────────

/// Return a decision's current values alongside its revision history.
///
/// History is chronological (oldest first): each entry captures the
/// choice and reason as they stood before a revision replaced them.
/// `revision_count` equals the number of history entries — the number of
/// times choice or reason has changed since the decision was recorded.
pub(crate) fn get_decision_history(state: &ProjectState, name: &str) -> Result<Value, String> {
    let dec = state
        .decisions
        .get(name)
        .ok_or_else(|| format!("decision `{name}` does not exist"))?;
    let d = &dec.decision;

    Ok(serde_json::json!({
        "name": name,
        "current": {
            "choice": d.choice,
            "reason": d.reason,
            "attribution": attribution_str(d.attribution),
            "created": d.created,
        },
        "history": d.history,
        "revision_count": d.history.len(),
    }))
}

// ── Helpers ──────────────────────────────────────────────────────────────

fn format_decisions(decisions: &[(&Arc<str>, &DecisionFile)]) -> Vec<Value> {
    decisions
        .iter()
        .map(|(name, d)| {
            let mut obj = serde_json::json!({
                "name": name.as_ref(),
                "choice": d.decision.choice,
                "reason": d.decision.reason,
                "attribution": attribution_str(d.decision.attribution),
            });
            if !d.decision.code_refs.is_empty() {
                obj["code_refs"] =
                    serde_json::json!(store::code_refs_to_json(&d.decision.code_refs));
            }
            obj
        })
        .collect()
}

fn format_patterns(patterns: &[(&Arc<str>, &PatternFile)]) -> Vec<Value> {
    patterns
        .iter()
        .map(|(name, p)| {
            serde_json::json!({
                "name": name.as_ref(),
                "description": p.pattern.description,
            })
        })
        .collect()
}

/// Compact decision format: name + choice only, no reasoning.
fn format_decisions_compact(decisions: &[(&Arc<str>, &DecisionFile)]) -> Vec<Value> {
    decisions
        .iter()
        .map(|(name, d)| {
            serde_json::json!({
                "name": name.as_ref(),
                "choice": d.decision.choice,
            })
        })
        .collect()
}

/// Token-efficient brief: constraint list without reasoning, dependencies,
/// or related decisions. For mid-implementation constraint checks where the
/// agent already knows the rationale.
fn build_brief_compact(
    component: &str,
    task_description: Option<&str>,
    component_decisions: &[(&Arc<str>, &DecisionFile)],
    project_decisions: &[(&Arc<str>, &DecisionFile)],
    patterns: &[(&Arc<str>, &PatternFile)],
    uncovered_concerns: &[&str],
) -> String {
    let mut brief = String::with_capacity(256);

    let covered_count = concerns::CONCERNS.len() - uncovered_concerns.len();
    if uncovered_concerns.len() > covered_count {
        brief.push_str(&format!(
            "\u{26a0} DESIGN INCOMPLETE \u{2014} {} concern areas have no decisions:\n  {}\n\n",
            uncovered_concerns.len(),
            uncovered_concerns.join(", "),
        ));
    }

    if let Some(task) = task_description {
        brief.push_str(&format!("TASK: {task}\n\n"));
    }

    if !project_decisions.is_empty() {
        brief.push_str("RULES:\n");
        for (_, d) in project_decisions {
            brief.push_str(&format!("- {}\n", d.decision.choice));
        }
        brief.push('\n');
    }

    if !patterns.is_empty() {
        brief.push_str("PATTERNS:\n");
        for (name, _) in patterns {
            brief.push_str(&format!("- {}\n", name.as_ref()));
        }
        brief.push('\n');
    }

    brief.push_str(&format!("CONSTRAINTS ({component}):\n"));
    if component_decisions.is_empty() {
        brief.push_str("- (none)\n");
    } else {
        for (_, d) in component_decisions {
            let suffix = attribution_suffix(d.decision.attribution);
            brief.push_str(&format!("- {}{}\n", d.decision.choice, suffix));
            if !d.decision.code_refs.is_empty() {
                brief.push_str(&format!(
                    "  Code: {}\n",
                    store::format_code_refs(&d.decision.code_refs)
                ));
            }
        }
    }

    brief
}

fn attribution_str(attr: Attribution) -> &'static str {
    match attr {
        Attribution::User => "user",
        Attribution::Agent => "agent",
    }
}

fn attribution_suffix(attr: Attribution) -> &'static str {
    match attr {
        Attribution::User => "",
        Attribution::Agent => " (agent \u{2014} unconfirmed)",
    }
}

fn extract_words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 3)
        .map(|w| w.to_lowercase())
        .filter(|w| !is_stop_word(w))
        .collect()
}

fn extract_words_from(fields: &[&str]) -> Vec<String> {
    fields
        .iter()
        .flat_map(|f| f.split(|c: char| !c.is_alphanumeric()))
        .filter(|w| w.len() >= 3)
        .map(|w| w.to_lowercase())
        .filter(|w| !is_stop_word(w))
        .collect()
}

fn is_stop_word(word: &str) -> bool {
    matches!(
        word,
        "the"
            | "and"
            | "for"
            | "are"
            | "but"
            | "not"
            | "you"
            | "all"
            | "can"
            | "had"
            | "her"
            | "was"
            | "one"
            | "our"
            | "out"
            | "has"
            | "have"
            | "been"
            | "were"
            | "being"
            | "will"
            | "would"
            | "could"
            | "should"
            | "must"
            | "shall"
            | "may"
            | "might"
            | "does"
            | "did"
            | "this"
            | "that"
            | "these"
            | "those"
            | "with"
            | "from"
            | "into"
            | "through"
            | "during"
            | "before"
            | "after"
            | "above"
            | "below"
            | "between"
            | "under"
            | "over"
            | "each"
            | "every"
            | "any"
            | "some"
            | "use"
            | "using"
            | "used"
            | "new"
            | "add"
            | "adding"
            | "added"
            | "about"
            | "which"
            | "when"
            | "where"
            | "what"
            | "how"
            | "why"
            | "also"
            | "just"
            | "than"
            | "then"
            | "them"
            | "they"
            | "their"
    )
}

/// Find the component whose name or description best matches the query words.
/// Returns `None` if no component has any keyword overlap (suggesting the
/// agent should `add_component` first).
///
/// Matching: component name is split on hyphens, description is word-extracted.
/// Score = count of matching query words. Highest score wins; ties broken by
/// fewest existing decisions (prefer under-designed components).
fn suggest_component_for<'a>(state: &'a ProjectState, query_words: &[String]) -> Option<&'a str> {
    let mut best: Option<(&str, usize, usize)> = None; // (name, score, decision_count)

    for (name, comp) in &state.components {
        let name_words: Vec<String> = name
            .split('-')
            .filter(|w| w.len() >= 3)
            .map(|w| w.to_lowercase())
            .collect();
        let desc_words = extract_words(&comp.component.description);

        let score = query_words
            .iter()
            .filter(|qw| name_words.iter().any(|nw| nw == *qw) || desc_words.contains(qw))
            .count();

        if score == 0 {
            continue;
        }

        let dec_count = state.graph().decisions_for(name).len();
        let dominated = best
            .as_ref()
            .is_some_and(|(_, bs, bd)| *bs > score || (*bs == score && *bd <= dec_count));
        if !dominated {
            best = Some((name, score, dec_count));
        }
    }

    best.map(|(name, _, _)| name)
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::schema::*;

    fn test_state() -> ProjectState {
        crate::store::testing::rich_test_state()
    }

    // ── get_context ─────────────────────────────────────────────────────

    #[test]
    fn get_context_returns_component_info() {
        let state = test_state();
        let result = get_context(&state, "auth", None, ContextDepth::Full).unwrap();

        assert_eq!(result["component"]["name"], "auth");
        assert_eq!(result["component"]["connects_to"][0], "database");
        assert_eq!(result["status"], "covered");
    }

    #[test]
    fn get_context_includes_reverse_connections() {
        let state = test_state();
        let result = get_context(&state, "database", None, ContextDepth::Full).unwrap();

        let connects_from = result["component"]["connects_from"].as_array().unwrap();
        let names: Vec<&str> = connects_from.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(names.contains(&"auth"));
        assert!(names.contains(&"rate-limiter"));
    }

    #[test]
    fn get_context_includes_project_rules() {
        let state = test_state();
        let result = get_context(&state, "auth", None, ContextDepth::Full).unwrap();

        let rules = result["project_rules"].as_array().unwrap();
        assert_eq!(rules.len(), 1);
        assert!(rules[0].as_str().unwrap().contains("Result<T, AppError>"));
    }

    #[test]
    fn get_context_includes_related_decisions() {
        let state = test_state();
        let result = get_context(&state, "auth", None, ContextDepth::Full).unwrap();

        let related = result["related_decisions"].as_array().unwrap();
        assert!(!related.is_empty());
        let text = related[0].as_str().unwrap();
        assert!(text.contains("database"));
        assert!(text.contains("connection pool"));
    }

    #[test]
    fn get_context_includes_patterns_field() {
        let state = test_state();
        let result = get_context(&state, "auth", None, ContextDepth::Full).unwrap();
        // Empty patterns for this fixture, but field must exist.
        let patterns = result["patterns"].as_array().unwrap();
        assert!(patterns.is_empty());
    }

    #[test]
    fn get_context_not_covered_when_no_decisions() {
        let mut state = test_state();
        state.decisions.clear();
        // Clear edges that reference decisions
        state
            .graph_index
            .edges
            .retain(|e| e.kind == EdgeKind::ConnectsTo);
        state
            .graph_index
            .nodes
            .retain(|n| n.kind == NodeKind::Component);
        state.rebuild_graph();
        let result = get_context(&state, "auth", None, ContextDepth::Full).unwrap();
        assert_eq!(result["status"], "not_covered");
    }

    #[test]
    fn get_context_partially_covered_with_only_project_rules() {
        let state = test_state();
        // rate-limiter has no component-specific decisions but project has rules.
        let result = get_context(&state, "rate-limiter", None, ContextDepth::Full).unwrap();
        assert_eq!(result["status"], "partially_covered");
    }

    #[test]
    fn get_context_rejects_nonexistent_component() {
        let state = test_state();
        let err = get_context(&state, "nonexistent", None, ContextDepth::Full).unwrap_err();
        assert!(err.contains("does not exist"));
    }

    #[test]
    fn get_context_for_project() {
        let state = test_state();
        let result = get_context(&state, "project", None, ContextDepth::Full).unwrap();

        assert_eq!(result["component"]["name"], "project");
        assert_eq!(result["status"], "covered");
        let brief = result["brief"].as_str().unwrap();
        assert!(brief.contains("PROJECT-WIDE RULES"));
    }

    #[test]
    fn get_context_includes_task_in_brief() {
        let state = test_state();
        let result = get_context(
            &state,
            "auth",
            Some("implement token refresh"),
            ContextDepth::Full,
        )
        .unwrap();

        let brief = result["brief"].as_str().unwrap();
        assert!(
            brief.contains("TASK: implement token refresh"),
            "brief must contain the task line"
        );
    }

    // ── build_brief ─────────────────────────────────────────────────────

    #[test]
    fn brief_has_when_uncertain() {
        let state = test_state();
        let result = get_context(&state, "auth", None, ContextDepth::Full).unwrap();
        let brief = result["brief"].as_str().unwrap();

        assert!(brief.contains("WHEN UNCERTAIN:"));
        assert!(brief.contains("STOP"));
        assert!(brief.contains("check_pattern"));
        assert!(brief.contains("Never silently deviate"));
    }

    #[test]
    fn brief_has_override_policy() {
        let state = test_state();
        let result = get_context(&state, "auth", None, ContextDepth::Full).unwrap();
        let brief = result["brief"].as_str().unwrap();

        assert!(brief.contains("OVERRIDE POLICY:"));
        assert!(brief.contains("inviolable"));
        assert!(brief.contains("strong defaults"));
    }

    #[test]
    fn brief_has_rules_section() {
        let state = test_state();
        let result = get_context(&state, "auth", None, ContextDepth::Full).unwrap();
        let brief = result["brief"].as_str().unwrap();

        assert!(brief.contains("RULES (inviolable"));
        assert!(brief.contains("Result<T, AppError>"));
    }

    #[test]
    fn brief_includes_transitive_dependencies() {
        use crate::store::schema::*;
        use chrono::{TimeZone, Utc};

        let mut state = test_state();

        // Add a decision that use-jwt depends on (in a non-connected component).
        state.decisions.insert(
            "tls-required".into(),
            Arc::new(DecisionFile {
                decision: Decision {
                    component: "rate-limiter".into(),
                    choice: "TLS everywhere".into(),
                    reason: "Zero trust".into(),
                    alternatives: vec![],
                    tags: vec![],
                    attribution: Attribution::User,
                    created: Utc.with_ymd_and_hms(2025, 6, 1, 12, 0, 0).unwrap(),
                    code_refs: vec![],
                    history: vec![],
                },
            }),
        );
        state.graph_index.nodes.push(NodeEntry {
            name: "tls-required".into(),
            kind: NodeKind::Decision,
            tags: vec![],
            hash: String::new(),
        });
        state.graph_index.edges.push(EdgeEntry {
            from: "tls-required".into(),
            to: "rate-limiter".into(),
            kind: EdgeKind::BelongsTo,
        });
        // use-jwt depends on tls-required (cross-component dependency).
        state.graph_index.edges.push(EdgeEntry {
            from: "use-jwt".into(),
            to: "tls-required".into(),
            kind: EdgeKind::DependsOn,
        });
        state.rebuild_graph();

        let result = get_context(&state, "auth", None, ContextDepth::Full).unwrap();
        let brief = result["brief"].as_str().unwrap();

        assert!(
            brief.contains("DEPENDENCIES:"),
            "brief should have a DEPENDENCIES section when transitive deps exist: {brief}"
        );
        assert!(
            brief.contains("TLS everywhere"),
            "transitive dependency should appear in brief: {brief}"
        );

        // Also in the related_decisions field.
        let related = result["related_decisions"].as_array().unwrap();
        assert!(
            related
                .iter()
                .any(|r| r.as_str().is_some_and(|s| s.contains("TLS everywhere"))),
            "transitive dependency should appear in related_decisions: {related:?}"
        );
    }

    // ── check_pattern ───────────────────────────────────────────────────

    #[test]
    fn check_pattern_finds_matching_decisions() {
        let state = test_state();
        let result = check_pattern(&state, "JWT token format for authentication");

        assert_eq!(result["status"], "covered");
        let decisions = result["decisions"].as_array().unwrap();
        assert!(!decisions.is_empty());
        assert!(decisions.iter().any(|d| d["name"] == "use-jwt"));
    }

    #[test]
    fn check_pattern_returns_not_covered() {
        let state = test_state();
        let result = check_pattern(&state, "WebSocket real-time notifications");

        assert_eq!(result["status"], "not_covered");
        let decisions = result["decisions"].as_array().unwrap();
        assert!(decisions.is_empty());
        let patterns = result["patterns"].as_array().unwrap();
        assert!(patterns.is_empty());
    }

    #[test]
    fn check_pattern_handles_empty_description() {
        let state = test_state();
        let result = check_pattern(&state, "");
        assert_eq!(result["status"], "not_covered");
        assert!(result["decisions"].as_array().unwrap().is_empty());
        assert!(result["patterns"].as_array().unwrap().is_empty());

        let result = check_pattern(&state, "a b");
        assert_eq!(result["status"], "not_covered");
        assert!(result["patterns"].as_array().unwrap().is_empty());
    }

    #[test]
    fn check_pattern_case_insensitive() {
        let state = test_state();
        let result = check_pattern(&state, "REDIS CONNECTION POOL");
        assert_eq!(result["status"], "covered");
    }

    #[test]
    fn check_pattern_boosts_tag_matches() {
        let state = test_state();
        // "security" is a tag on use-jwt. Should boost its ranking.
        let result = check_pattern(&state, "security authentication tokens");
        assert_eq!(result["status"], "covered");
        let decisions = result["decisions"].as_array().unwrap();
        assert_eq!(decisions[0]["name"], "use-jwt");
    }

    #[test]
    fn check_pattern_returns_matched_patterns() {
        let mut state = test_state();

        // Add a pattern with MemberOf edges.
        state.patterns.insert(
            "auth-pattern".into(),
            Arc::new(PatternFile {
                pattern: Pattern {
                    name: "auth-pattern".into(),
                    description: "Stateless auth via JWT".into(),
                },
            }),
        );
        state.graph_index.nodes.push(NodeEntry {
            name: "auth-pattern".into(),
            kind: NodeKind::Pattern,
            tags: vec![],
            hash: String::new(),
        });
        state.graph_index.edges.push(EdgeEntry {
            from: "auth-pattern".into(),
            to: "use-jwt".into(),
            kind: EdgeKind::MemberOf,
        });
        state.graph_index.edges.push(EdgeEntry {
            from: "auth-pattern".into(),
            to: "error-strategy".into(),
            kind: EdgeKind::MemberOf,
        });

        state.rebuild_graph();

        let result = check_pattern(&state, "JWT authentication tokens");
        assert_eq!(result["status"], "covered");
        let patterns = result["patterns"].as_array().unwrap();
        assert_eq!(patterns.len(), 1);
        assert_eq!(patterns[0]["name"], "auth-pattern");
        assert_eq!(patterns[0]["description"], "Stateless auth via JWT");
    }

    // ── get_architecture ────────────────────────────────────────────────

    #[test]
    fn get_architecture_returns_full_overview() {
        let state = test_state();
        let result = get_architecture(&state);

        assert_eq!(result["project"]["name"], "test-project");
        assert_eq!(result["total_components"], 3);
        assert_eq!(result["total_decisions"], 3);
        assert_eq!(result["total_patterns"], 0);

        let components = result["components"].as_array().unwrap();
        assert_eq!(components.len(), 3);

        let auth = components.iter().find(|c| c["name"] == "auth").unwrap();
        assert_eq!(auth["decision_count"], 1);
        let auth_connects = auth["connects_to"].as_array().unwrap();
        assert!(auth_connects.iter().any(|v| v == "database"));
    }

    #[test]
    fn get_architecture_includes_pattern_member_counts() {
        let mut state = test_state();

        state.patterns.insert(
            "state-in-redis".into(),
            Arc::new(PatternFile {
                pattern: Pattern {
                    name: "state-in-redis".into(),
                    description: "All state uses Redis".into(),
                },
            }),
        );
        state.graph_index.nodes.push(NodeEntry {
            name: "state-in-redis".into(),
            kind: NodeKind::Pattern,
            tags: vec![],
            hash: String::new(),
        });
        state.graph_index.edges.push(EdgeEntry {
            from: "state-in-redis".into(),
            to: "use-jwt".into(),
            kind: EdgeKind::MemberOf,
        });
        state.graph_index.edges.push(EdgeEntry {
            from: "state-in-redis".into(),
            to: "db-pool".into(),
            kind: EdgeKind::MemberOf,
        });
        state.graph_index.edges.push(EdgeEntry {
            from: "state-in-redis".into(),
            to: "auth".into(),
            kind: EdgeKind::AppliesTo,
        });

        state.rebuild_graph();

        let result = get_architecture(&state);
        assert_eq!(result["total_patterns"], 1);

        let patterns = result["patterns"].as_array().unwrap();
        assert_eq!(patterns.len(), 1);
        assert_eq!(patterns[0]["name"], "state-in-redis");
        assert_eq!(patterns[0]["decision_count"], 2);
        assert_eq!(patterns[0]["component_count"], 1);
    }

    // ── extract_words ───────────────────────────────────────────────────

    #[test]
    fn extract_words_filters_short_and_stop_words() {
        let words = extract_words("use the Redis for session storage");
        assert!(words.contains(&"redis".to_string()));
        assert!(words.contains(&"session".to_string()));
        assert!(words.contains(&"storage".to_string()));
        assert!(!words.contains(&"use".to_string()));
        assert!(!words.contains(&"the".to_string()));
        assert!(!words.contains(&"for".to_string()));
    }

    #[test]
    fn extract_words_lowercases() {
        let words = extract_words("JWT DPoP BINDING");
        assert!(words.contains(&"jwt".to_string()));
        assert!(words.contains(&"dpop".to_string()));
        assert!(words.contains(&"binding".to_string()));
    }

    // ── no workflow hints ─────────────────────────────────────────────

    #[test]
    fn get_context_no_workflow() {
        let state = test_state();
        let result = get_context(&state, "auth", None, ContextDepth::Full).unwrap();
        assert!(result.get("workflow").is_none());
        assert_eq!(result["status"], "covered");
    }

    #[test]
    fn get_context_constraints_no_workflow() {
        let state = test_state();
        let result = get_context(&state, "auth", None, ContextDepth::Constraints).unwrap();
        assert!(result.get("workflow").is_none());
    }

    #[test]
    fn check_pattern_no_workflow() {
        let state = test_state();
        let covered = check_pattern(&state, "JWT token authentication");
        assert_eq!(covered["status"], "covered");
        assert!(covered.get("workflow").is_none());

        let uncovered = check_pattern(&state, "WebSocket notifications");
        assert_eq!(uncovered["status"], "not_covered");
        assert!(uncovered.get("workflow").is_none());
    }

    #[test]
    fn get_architecture_no_workflow() {
        let state = test_state();
        let result = get_architecture(&state);
        assert!(result.get("workflow").is_none());
        // undesigned_components is now plain data, not nested in workflow.
        let undesigned = result["undesigned_components"].as_array().unwrap();
        assert_eq!(undesigned.len(), 1);
        assert_eq!(undesigned[0], "rate-limiter");
    }

    #[test]
    fn project_context_no_workflow() {
        use crate::store::schema::*;
        let state = ProjectState::new(
            ProjectFile {
                trurlic_version: FORMAT_VERSION.into(),
                project: Project {
                    name: "empty".into(),
                    description: String::new(),
                },
            },
            std::collections::BTreeMap::new(),
            std::collections::BTreeMap::new(),
            std::collections::BTreeMap::new(),
            GraphIndex {
                version: 1,
                rebuilt: chrono::Utc::now(),
                nodes: vec![],
                edges: vec![],
            },
        );
        let result = get_context(&state, "project", None, ContextDepth::Full).unwrap();
        assert!(result.get("workflow").is_none());
    }

    // ── degradation warning ───────────────────────────────────────────

    #[test]
    fn brief_has_degradation_warning_when_incomplete() {
        let state = test_state();
        // rate-limiter has no component decisions → only project rules cover
        // Error handling. 9 of 10 concerns uncovered → degradation warning.
        let result = get_context(&state, "rate-limiter", None, ContextDepth::Full).unwrap();
        let brief = result["brief"].as_str().unwrap();
        assert!(
            brief.contains("DESIGN INCOMPLETE"),
            "brief should warn about incomplete design: {brief}"
        );
    }

    #[test]
    fn brief_no_degradation_warning_when_covered() {
        let state = test_state();
        // auth has 1 decision + project rule. Whether that's enough depends
        // on keyword coverage, but with just 2 decisions most concerns are
        // uncovered. We need a well-covered component to test no-warning.
        // Use the project context instead — no concern coverage applies.
        let result = get_context(&state, "project", None, ContextDepth::Full).unwrap();
        let brief = result["brief"].as_str().unwrap();
        assert!(
            !brief.contains("DESIGN INCOMPLETE"),
            "project brief should not have degradation warning"
        );
    }

    // ── concern coverage ──────────────────────────────────────────────

    #[test]
    fn get_context_includes_concern_coverage() {
        let state = test_state();
        let result = get_context(&state, "auth", None, ContextDepth::Full).unwrap();
        let coverage = &result["concern_coverage"];
        assert!(coverage["covered"].is_array());
        assert!(coverage["uncovered"].is_array());
        // Project-wide "Result<T, AppError>" covers Error handling.
        let covered = coverage["covered"].as_array().unwrap();
        assert!(
            covered
                .iter()
                .any(|c| c.as_str().unwrap().contains("Error handling")),
            "project rule should cover Error handling: {covered:?}"
        );
        // Most concerns remain uncovered.
        let uncovered = coverage["uncovered"].as_array().unwrap();
        assert!(uncovered.len() > covered.len());
    }

    #[test]
    fn get_architecture_includes_per_component_coverage() {
        let state = test_state();
        let result = get_architecture(&state);
        let components = result["components"].as_array().unwrap();
        let auth = components.iter().find(|c| c["name"] == "auth").unwrap();
        assert!(auth["concern_coverage"]["covered"].is_number());
        assert!(auth["concern_coverage"]["uncovered"].is_number());
        assert!(auth["concern_coverage"]["uncovered_concerns"].is_array());
    }

    // ── context depth ─────────────────────────────────────────────────

    #[test]
    fn constraints_depth_omits_reasoning() {
        let state = test_state();
        let result = get_context(&state, "auth", None, ContextDepth::Constraints).unwrap();
        // Decisions should have name + choice but no reason.
        let decisions = result["decisions"].as_array().unwrap();
        assert!(!decisions.is_empty());
        assert!(decisions[0].get("name").is_some());
        assert!(decisions[0].get("choice").is_some());
        assert!(
            decisions[0].get("reason").is_none(),
            "constraints depth must omit reasoning"
        );
    }

    #[test]
    fn constraints_depth_omits_related_decisions() {
        let state = test_state();
        let result = get_context(&state, "auth", None, ContextDepth::Constraints).unwrap();
        assert!(
            result.get("related_decisions").is_none(),
            "constraints depth must omit related_decisions"
        );
    }

    #[test]
    fn constraints_depth_has_compact_brief() {
        let state = test_state();
        let full = get_context(&state, "auth", None, ContextDepth::Full).unwrap();
        let compact = get_context(&state, "auth", None, ContextDepth::Constraints).unwrap();
        let full_len = full["brief"].as_str().unwrap().len();
        let compact_len = compact["brief"].as_str().unwrap().len();
        assert!(
            compact_len < full_len,
            "compact brief ({compact_len}) must be shorter than full ({full_len})"
        );
    }

    // ── component suggestion ──────────────────────────────────────────

    #[test]
    fn check_pattern_suggests_matching_component() {
        let state = test_state();
        // "rate limiting" should match the "rate-limiter" component by name.
        let result = check_pattern(&state, "rate limiting for API requests");
        assert_eq!(result["status"], "not_covered");
        assert_eq!(result["suggested_component"], "rate-limiter");
    }

    #[test]
    fn check_pattern_suggests_null_when_no_match() {
        let state = test_state();
        // "WebSocket notifications" matches no existing component.
        let result = check_pattern(&state, "WebSocket notification streaming");
        assert_eq!(result["status"], "not_covered");
        assert!(result["suggested_component"].is_null());
    }

    // ── attribution in context ────────────────────────────────────────

    #[test]
    fn context_brief_flags_agent_attribution() {
        use chrono::{TimeZone, Utc};

        let ts = Utc.with_ymd_and_hms(2025, 6, 1, 12, 0, 0).unwrap();

        let mut state = test_state();
        state.decisions.insert(
            "agent-dec".into(),
            Arc::new(DecisionFile {
                decision: Decision {
                    component: "auth".into(),
                    choice: "Agent suggested approach".into(),
                    reason: "Automated".into(),
                    alternatives: vec![],
                    tags: vec![],
                    attribution: Attribution::Agent,
                    created: ts,
                    code_refs: vec![],
                    history: vec![],
                },
            }),
        );
        state.graph_index.nodes.push(NodeEntry {
            name: "agent-dec".into(),
            kind: NodeKind::Decision,
            tags: vec![],
            hash: String::new(),
        });
        state.graph_index.edges.push(EdgeEntry {
            from: "agent-dec".into(),
            to: "auth".into(),
            kind: EdgeKind::BelongsTo,
        });
        state.rebuild_graph();

        let result = get_context(&state, "auth", None, ContextDepth::Full).unwrap();
        let brief = result["brief"].as_str().unwrap();

        assert!(
            brief.contains("unconfirmed"),
            "agent decision must be flagged as unconfirmed in brief: {brief}"
        );
        // User decision must NOT have the unconfirmed marker.
        let user_line = brief.lines().find(|l| l.contains("JWT with DPoP")).unwrap();
        assert!(
            !user_line.contains("unconfirmed"),
            "user decision must NOT be flagged: {user_line}"
        );

        // JSON decisions include attribution field.
        let decisions = result["decisions"].as_array().unwrap();
        let agent_dec = decisions.iter().find(|d| d["name"] == "agent-dec").unwrap();
        assert_eq!(agent_dec["attribution"], "agent");

        let user_dec = decisions.iter().find(|d| d["name"] == "use-jwt").unwrap();
        assert_eq!(user_dec["attribution"], "user");
    }

    // ── code_refs in brief ────────────────────────────────────────────

    #[test]
    fn brief_includes_code_refs_line() {
        use chrono::Utc;
        let dec = DecisionFile {
            decision: Decision {
                component: "store".into(),
                choice: "BLAKE3 hashing".into(),
                reason: "Fast integrity checks".into(),
                alternatives: vec![],
                tags: vec![],
                attribution: Attribution::User,
                created: Utc::now(),
                code_refs: vec![
                    CodeRef {
                        file: "src/store/write.rs".into(),
                        symbol: Some("content_hash".into()),
                    },
                    CodeRef {
                        file: "src/store/validate.rs".into(),
                        symbol: None,
                    },
                ],
                history: vec![],
            },
        };
        let name: Arc<str> = Arc::from("blake3-hashing");
        let comp_decs = vec![(&name, &dec)];

        let brief = build_brief(&BriefParams {
            component: "store",
            task: None,
            project_decisions: &[],
            component_decisions: &comp_decs,
            transitive_deps: &[],
            related_decisions: &[],
            patterns: &[],
            uncovered_concerns: &[],
        });

        assert!(
            brief.contains("Code: src/store/write.rs::content_hash, src/store/validate.rs"),
            "brief should include code_refs line, got:\n{brief}"
        );
    }

    #[test]
    fn brief_omits_code_line_when_empty() {
        use chrono::Utc;
        let dec = DecisionFile {
            decision: Decision {
                component: "store".into(),
                choice: "BLAKE3 hashing".into(),
                reason: "Fast integrity checks".into(),
                alternatives: vec![],
                tags: vec![],
                attribution: Attribution::User,
                created: Utc::now(),
                code_refs: vec![],
                history: vec![],
            },
        };
        let name: Arc<str> = Arc::from("blake3-hashing");
        let comp_decs = vec![(&name, &dec)];

        let brief = build_brief(&BriefParams {
            component: "store",
            task: None,
            project_decisions: &[],
            component_decisions: &comp_decs,
            transitive_deps: &[],
            related_decisions: &[],
            patterns: &[],
            uncovered_concerns: &[],
        });

        assert!(
            !brief.contains("Code:"),
            "brief should not contain Code: when code_refs empty"
        );
    }

    #[test]
    fn format_decisions_includes_code_refs() {
        use chrono::Utc;
        let dec = DecisionFile {
            decision: Decision {
                component: "store".into(),
                choice: "Atomic writes".into(),
                reason: "Crash safety".into(),
                alternatives: vec![],
                tags: vec![],
                attribution: Attribution::User,
                created: Utc::now(),
                code_refs: vec![CodeRef {
                    file: "src/store/write.rs".into(),
                    symbol: Some("commit_with_graph".into()),
                }],
                history: vec![],
            },
        };
        let name: Arc<str> = Arc::from("atomic-writes");
        let decisions = vec![(&name, &dec)];

        let formatted = format_decisions(&decisions);
        assert_eq!(formatted.len(), 1);
        let refs = formatted[0]["code_refs"].as_array().unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0]["file"], "src/store/write.rs");
        assert_eq!(refs[0]["symbol"], "commit_with_graph");
    }

    // ── get_decision_history ──────────────────────────────────────────

    #[test]
    fn get_decision_history_no_history_is_empty() {
        let state = test_state();
        // use-jwt is a freshly recorded decision — never revised.
        let result = get_decision_history(&state, "use-jwt").unwrap();

        assert_eq!(result["name"], "use-jwt");
        assert_eq!(result["revision_count"], 0);
        assert!(result["history"].as_array().unwrap().is_empty());
        // Current values are always present.
        assert!(result["current"]["choice"].is_string());
        assert!(result["current"]["reason"].is_string());
        assert_eq!(result["current"]["attribution"], "user");
        assert!(result["current"]["created"].is_string());
    }

    #[test]
    fn get_decision_history_returns_chronological_entries() {
        use chrono::{TimeZone, Utc};

        let mut state = test_state();
        state.decisions.insert(
            "revised-dec".into(),
            Arc::new(DecisionFile {
                decision: Decision {
                    component: "auth".into(),
                    choice: "JWT with DPoP binding".into(),
                    reason: "Proof-of-possession prevents replay".into(),
                    alternatives: vec![],
                    tags: vec![],
                    attribution: Attribution::User,
                    created: Utc.with_ymd_and_hms(2025, 6, 1, 10, 30, 0).unwrap(),
                    code_refs: vec![],
                    history: vec![
                        HistoryEntry {
                            choice: "JWT tokens".into(),
                            reason: "Stateless authentication".into(),
                            changed_at: Utc.with_ymd_and_hms(2025, 7, 15, 14, 0, 0).unwrap(),
                        },
                        HistoryEntry {
                            choice: "JWT with refresh tokens".into(),
                            reason: "Stateless auth with rotation".into(),
                            changed_at: Utc.with_ymd_and_hms(2025, 8, 1, 9, 30, 0).unwrap(),
                        },
                    ],
                },
            }),
        );

        let result = get_decision_history(&state, "revised-dec").unwrap();

        assert_eq!(result["revision_count"], 2);
        assert_eq!(result["current"]["choice"], "JWT with DPoP binding");

        let history = result["history"].as_array().unwrap();
        assert_eq!(history.len(), 2);
        // Oldest first: entries trace the decision's evolution.
        assert_eq!(history[0]["choice"], "JWT tokens");
        assert_eq!(history[1]["choice"], "JWT with refresh tokens");
        assert!(history[0]["changed_at"].is_string());
    }

    #[test]
    fn get_decision_history_rejects_nonexistent() {
        let state = test_state();
        let err = get_decision_history(&state, "ghost").unwrap_err();
        assert!(err.contains("ghost"));
        assert!(err.contains("does not exist"));
    }

    #[test]
    fn format_decisions_omits_code_refs_when_empty() {
        use chrono::Utc;
        let dec = DecisionFile {
            decision: Decision {
                component: "store".into(),
                choice: "Atomic writes".into(),
                reason: "Crash safety".into(),
                alternatives: vec![],
                tags: vec![],
                attribution: Attribution::User,
                created: Utc::now(),
                code_refs: vec![],
                history: vec![],
            },
        };
        let name: Arc<str> = Arc::from("atomic-writes");
        let decisions = vec![(&name, &dec)];

        let formatted = format_decisions(&decisions);
        assert!(
            formatted[0].get("code_refs").is_none(),
            "should omit code_refs key when empty"
        );
    }
}
