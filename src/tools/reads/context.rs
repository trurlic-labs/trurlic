//! What the reads written by hand assemble: a component's context
//! and the decisions anchored in a file.

use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;

use serde_json::{Map, Value, json};

use crate::store::{self, Anchor, Component, Named, PROJECT_COMPONENT, ProjectState, Status};

/// Controls how much detail `get_context` returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ContextDepth {
    /// Decisions with reasoning, related decisions, transitive
    /// dependencies, and the full brief.
    Full,
    /// Choice text only, no related decisions, compact brief.
    Constraints,
}

// -- get_context ----------------------------------------------------------------

/// A component's decisions, the project-wide rules, the decisions of
/// connected components and of what its decisions depend on, and a brief
/// assembled from them. `depth` is how much of that is returned.
pub(super) fn get_context(
    state: &ProjectState,
    component: &str,
    task: Option<&str>,
    depth: ContextDepth,
) -> Result<Value, String> {
    if component == PROJECT_COMPONENT {
        return Ok(project_context(state, task));
    }
    let comp = state
        .components
        .get(component)
        .ok_or_else(|| format!("component `{component}` does not exist"))?;
    let own = state.decisions_of(component);
    let rules = state.project_decisions();
    let view = View {
        component,
        task,
        own: &own,
        rules: &rules,
    };
    Ok(match depth {
        ContextDepth::Full => full_context(state, comp, &view),
        ContextDepth::Constraints => json!({
            "component": { "name": component },
            "decisions": own
                .iter()
                .map(|(name, d)| json!({ "name": name, "choice": d.choice }))
                .collect::<Vec<_>>(),
            "project_rules": view.project_rules(),
            "brief": build_brief_compact(&view),
            "status": view.coverage(),
        }),
    })
}

/// What both depths of a component's context are built from.
struct View<'a> {
    component: &'a str,
    task: Option<&'a str>,
    own: &'a [Named<'a>],
    rules: &'a [Named<'a>],
}

impl View<'_> {
    fn coverage(&self) -> &'static str {
        if !self.own.is_empty() {
            "covered"
        } else if !self.rules.is_empty() {
            "partially_covered"
        } else {
            "not_covered"
        }
    }

    fn project_rules(&self) -> Vec<&str> {
        self.rules.iter().map(|(_, d)| d.choice.as_str()).collect()
    }
}

fn full_context(state: &ProjectState, comp: &Component, view: &View<'_>) -> Value {
    let orphaned = orphaned(&state.code_root, view.own);
    let related = state.related_decisions(view.component);
    let seeds: Vec<&str> = view.own.iter().map(|&(name, _)| name).collect();
    let dependencies = state.dependencies(&seeds, 3);
    let brief = build_brief(&Brief {
        view,
        related: &related,
        dependencies: &dependencies,
        orphaned: &orphaned,
    });
    let mut seen: BTreeSet<&str> = seeds.iter().copied().collect();
    let combined_related: Vec<String> = related
        .iter()
        .chain(&dependencies)
        .filter(|(name, _)| seen.insert(name))
        .map(|(_, d)| format!("{}: {} ({})", d.component, d.choice, d.reason))
        .collect();
    json!({
        "component": {
            "name": view.component,
            "description": comp.description,
            "connects_to": comp.connects_to,
            "connects_from": state.connects_from(view.component),
        },
        "decisions": format_decisions(view.own),
        "project_rules": view.project_rules(),
        "related_decisions": combined_related,
        "health": health(view.own, orphaned.len()),
        "brief": brief,
        "status": view.coverage(),
    })
}

fn project_context(state: &ProjectState, task: Option<&str>) -> Value {
    let rules = state.project_decisions();

    let mut brief = String::with_capacity(256);
    push_task(&mut brief, task);
    if rules.is_empty() {
        brief.push_str("No project-wide decisions recorded yet.\n\n");
    } else {
        let listed = rules
            .iter()
            .map(|(_, d)| format!("{}{}", d.choice, status_suffix(&d.status)));
        push_section(&mut brief, "PROJECT-WIDE RULES", listed);
    }
    brief.push_str("WHEN UNCERTAIN:\n");
    brief.push_str("STOP. Ask the user to design project-wide rules first.\n");

    json!({
        "component": {
            "name": PROJECT_COMPONENT,
            "description": state.project.description,
        },
        "decisions": format_decisions(&rules),
        "project_rules": rules.iter().map(|(_, d)| &d.choice).collect::<Vec<_>>(),
        "related_decisions": [],
        "brief": brief,
        "status": if rules.is_empty() { "not_covered" } else { "covered" },
    })
}

// -- The brief --------------------------------------------------------------------

struct Brief<'a> {
    view: &'a View<'a>,
    related: &'a [Named<'a>],
    dependencies: &'a [Named<'a>],
    /// The decisions of `own` whose every anchor names a file that is gone.
    orphaned: &'a BTreeSet<&'a str>,
}

const OVERRIDE_POLICY: &str = "OVERRIDE POLICY:\n\
    RULES are inviolable. Component decisions are strong defaults:\n\
    follow them unless the user explicitly revises them.\n\
    Never silently deviate from either.\n\n";

const WHEN_UNCERTAIN: &str = "WHEN UNCERTAIN:\n\
    STOP. Search .trurlic/decisions/ for a decision that covers it. If none\n\
    does, ask the user to design it first. Never silently deviate.\n";

/// The brief coding agents read in place of the structured fields.
fn build_brief(p: &Brief<'_>) -> String {
    let view = p.view;
    let mut brief = String::with_capacity(512);
    push_task(&mut brief, view.task);
    let rules = view.rules.iter().map(|(_, d)| &d.choice);
    push_section(
        &mut brief,
        "RULES (inviolable: every generated line must respect these)",
        rules,
    );

    brief.push_str(&format!("COMPONENT: {}\n", view.component));
    if view.own.is_empty() {
        brief.push_str("- No decisions recorded yet.\n");
    }
    for &(name, d) in view.own {
        let suffix = status_suffix(&d.status);
        brief.push_str(&format!("- {} ({}){suffix}\n", d.choice, d.reason));
        if p.orphaned.contains(name) {
            brief.push_str("  ORPHANED ANCHORS: every anchored file is gone\n");
        }
        if !d.anchors.is_empty() {
            brief.push_str(&format!("  Code: {}\n", list_anchors(&d.anchors)));
        }
    }
    brief.push('\n');

    let dependencies = p
        .dependencies
        .iter()
        .map(|(_, d)| format!("{}: {} ({})", d.component, d.choice, d.reason));
    push_section(&mut brief, "DEPENDENCIES", dependencies);
    let related = p
        .related
        .iter()
        .map(|(_, d)| format!("{}: {}", d.component, d.choice));
    push_section(&mut brief, "RELATED", related);
    brief.push_str(OVERRIDE_POLICY);
    brief.push_str(WHEN_UNCERTAIN);
    brief
}

/// Choices without reasoning, dependencies or related decisions, for a
/// check in the middle of implementing.
fn build_brief_compact(view: &View<'_>) -> String {
    let mut brief = String::with_capacity(256);
    push_task(&mut brief, view.task);
    push_section(
        &mut brief,
        "RULES",
        view.rules.iter().map(|(_, d)| &d.choice),
    );
    brief.push_str(&format!("CONSTRAINTS ({}):\n", view.component));
    if view.own.is_empty() {
        brief.push_str("- (none)\n");
    }
    for (_, d) in view.own {
        brief.push_str(&format!("- {}{}\n", d.choice, status_suffix(&d.status)));
        if !d.anchors.is_empty() {
            brief.push_str(&format!("  Code: {}\n", list_anchors(&d.anchors)));
        }
    }
    brief
}

fn push_task(brief: &mut String, task: Option<&str>) {
    if let Some(task) = task {
        brief.push_str(&format!("TASK: {task}\n\n"));
    }
}

/// `title`, then each line as a list item and a blank line; nothing when
/// there is no line.
fn push_section(brief: &mut String, title: &str, lines: impl Iterator<Item = impl fmt::Display>) {
    let mut lines = lines.peekable();
    if lines.peek().is_none() {
        return;
    }
    brief.push_str(&format!("{title}:\n"));
    for line in lines {
        brief.push_str(&format!("- {line}\n"));
    }
    brief.push('\n');
}

// -- get_decisions_for_file -------------------------------------------------------

/// Every decision anchored on `file`, on a file under it as a directory, or
/// on a directory that holds it, by component then name, each with the
/// anchors that matched. Project-wide rules are left out unless anchored
/// there: they apply everywhere, and `get_context("project")` lists them.
pub(super) fn get_decisions_for_file(state: &ProjectState, file: &str) -> Result<Value, String> {
    let path =
        store::normalize_path(file).map_err(|fault| format!("invalid file path: {fault}"))?;
    let decisions: Vec<Value> = state
        .decisions_in(&path)
        .into_iter()
        .map(|(name, d)| {
            let mut entry = Map::new();
            entry.insert("name".into(), json!(name));
            entry.insert("component".into(), json!(d.component));
            entry.insert("choice".into(), json!(d.choice));
            entry.insert("reason".into(), json!(d.reason));
            entry.insert("status".into(), json!(d.status.name()));
            if !d.tags.is_empty() {
                entry.insert("tags".into(), json!(d.tags));
            }
            let matching = anchors_json(store::anchors_in(d, &path));
            entry.insert("matching_anchors".into(), matching);
            Value::Object(entry)
        })
        .collect();
    Ok(json!({
        "file": path,
        "count": decisions.len(),
        "decisions": decisions,
    }))
}

// -- Health -------------------------------------------------------------------------

/// The names of the decisions of `own` that have anchors and whose every
/// anchored file is confirmed gone. A probe that fails counts as present,
/// so a filesystem error never reports a live file as deleted.
fn orphaned<'a>(code_root: &Path, own: &[Named<'a>]) -> BTreeSet<&'a str> {
    own.iter()
        .filter(|(_, d)| {
            !d.anchors.is_empty()
                && d.anchors
                    .iter()
                    .all(|anchor| matches!(code_root.join(anchor.file()).try_exists(), Ok(false)))
        })
        .map(|&(name, _)| name)
        .collect()
}

/// How many decisions a component carries, how many are proposed, how many
/// have lost every anchored file, and the most pressing warning of these.
fn health(own: &[Named<'_>], orphaned: usize) -> Value {
    const MAX_HEALTHY_DECISIONS: usize = 20;
    const MAX_HEALTHY_PROPOSED: usize = 5;

    let total = own.len();
    let proposed = own
        .iter()
        .filter(|(_, d)| matches!(d.status, Status::Proposed { .. }))
        .count();
    let warning = if total > MAX_HEALTHY_DECISIONS {
        json!("Too many decisions for this component: consolidate or remove outdated ones")
    } else if proposed > MAX_HEALTHY_PROPOSED {
        json!("Several proposed decisions wait for the owner's review")
    } else if orphaned > 0 {
        json!(format!(
            "{orphaned} decisions anchor only files that are gone: revise or remove them"
        ))
    } else {
        Value::Null
    };
    json!({
        "total": total,
        "proposed": proposed,
        "orphaned_anchors": orphaned,
        "warning": warning,
    })
}

// -- Formatting ---------------------------------------------------------------------

fn format_decisions(decisions: &[Named<'_>]) -> Vec<Value> {
    decisions
        .iter()
        .map(|(name, d)| {
            let mut entry = Map::new();
            entry.insert("name".into(), json!(name));
            entry.insert("choice".into(), json!(d.choice));
            entry.insert("reason".into(), json!(d.reason));
            entry.insert("status".into(), json!(d.status.name()));
            if !d.anchors.is_empty() {
                entry.insert("anchors".into(), anchors_json(&d.anchors));
            }
            Value::Object(entry)
        })
        .collect()
}

fn anchors_json<'a>(anchors: impl IntoIterator<Item = &'a Anchor>) -> Value {
    anchors
        .into_iter()
        .map(|anchor| match anchor.symbol() {
            Some(symbol) => json!({ "file": anchor.file(), "symbol": symbol }),
            None => json!({ "file": anchor.file() }),
        })
        .collect()
}

/// `file` or `file::symbol` of each anchor, comma-separated.
fn list_anchors<'a>(anchors: impl IntoIterator<Item = &'a Anchor>) -> String {
    anchors
        .into_iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// What a brief appends to a decision the owner has not accepted.
fn status_suffix(status: &Status) -> &'static str {
    match status {
        Status::Accepted { .. } => "",
        Status::Proposed {
            accepted_text: None,
        } => " (proposed)",
        Status::Proposed {
            accepted_text: Some(_),
        } => " (proposed revision of an accepted decision)",
    }
}

// -- Tests ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::store::testing::{anchored, empty_state, sample_component, sample_decision};
    use crate::store::{AcceptedText, Decision};

    fn decision(component: &str, choice: &str) -> Decision {
        let mut decision = sample_decision(component);
        decision.choice = choice.into();
        decision
    }

    /// `auth` connects to `database`; `rate-limiter` connects to `auth`.
    fn test_state() -> ProjectState {
        let mut state = empty_state();
        let component = |connects_to: &[&str]| Component {
            connects_to: connects_to.iter().map(|&to| to.to_owned()).collect(),
            ..sample_component()
        };
        state
            .components
            .insert("auth".into(), component(&["database"]));
        state.components.insert("database".into(), component(&[]));
        state
            .components
            .insert("rate-limiter".into(), component(&["auth"]));
        let mut jwt = decision("auth", "JWT with DPoP binding");
        jwt.depends_on.insert("db-pool".into());
        state.decisions.insert("use-jwt".into(), jwt);
        state.decisions.insert(
            "db-pool".into(),
            decision("database", "Shared connection pool"),
        );
        state.decisions.insert(
            "error-strategy".into(),
            decision(PROJECT_COMPONENT, "Result<T, AppError>"),
        );
        state
    }

    fn full(state: &ProjectState, component: &str) -> Value {
        get_context(state, component, None, ContextDepth::Full).unwrap()
    }

    #[test]
    fn a_component_context_holds_its_neighbours_rules_and_decisions() {
        let context = full(&test_state(), "auth");
        assert_eq!(context["component"]["connects_to"], json!(["database"]));
        assert_eq!(
            context["component"]["connects_from"],
            json!(["rate-limiter"])
        );
        assert_eq!(context["project_rules"], json!(["Result<T, AppError>"]));
        assert_eq!(context["decisions"][0]["name"], "use-jwt");
        assert_eq!(context["decisions"][0]["status"], "accepted");
        assert_eq!(context["status"], "covered");
        assert!(context.get("patterns").is_none());
    }

    #[test]
    fn related_decisions_and_dependencies_are_listed_once() {
        let context = full(&test_state(), "auth");
        let related = context["related_decisions"].as_array().unwrap();
        assert_eq!(related.len(), 1, "{related:?}");
        assert!(
            related[0]
                .as_str()
                .unwrap()
                .starts_with("database: Shared connection pool")
        );
        let brief = context["brief"].as_str().unwrap();
        assert!(
            brief.contains("DEPENDENCIES:\n- database: Shared connection pool"),
            "{brief}"
        );
        assert!(
            brief.contains("RELATED:\n- database: Shared connection pool"),
            "{brief}"
        );
    }

    #[test]
    fn coverage_follows_the_decisions_there_are() {
        let mut state = test_state();
        state.components.insert("empty".into(), sample_component());
        assert_eq!(full(&state, "empty")["status"], "partially_covered");
        state.decisions.remove("error-strategy");
        assert_eq!(full(&state, "empty")["status"], "not_covered");
    }

    #[test]
    fn a_component_that_does_not_exist_is_refused() {
        let err = get_context(&test_state(), "nonexistent", None, ContextDepth::Full).unwrap_err();
        assert!(err.contains("does not exist"), "{err}");
    }

    #[test]
    fn the_project_context_lists_the_rules() {
        let context = full(&test_state(), PROJECT_COMPONENT);
        assert_eq!(context["component"]["name"], PROJECT_COMPONENT);
        assert_eq!(context["decisions"][0]["name"], "error-strategy");
        assert!(
            context["brief"]
                .as_str()
                .unwrap()
                .contains("PROJECT-WIDE RULES:")
        );
    }

    #[test]
    fn the_brief_names_the_task_and_the_policy() {
        let context = get_context(
            &test_state(),
            "auth",
            Some("Add token refresh"),
            ContextDepth::Full,
        )
        .unwrap();
        let brief = context["brief"].as_str().unwrap();
        assert!(brief.starts_with("TASK: Add token refresh\n\n"));
        assert!(brief.contains("RULES (inviolable"));
        assert!(brief.contains("OVERRIDE POLICY:"));
        assert!(brief.contains("Search .trurlic/decisions/"), "{brief}");
        assert!(brief.is_ascii(), "{brief}");
    }

    #[test]
    fn constraints_depth_keeps_choices_only() {
        let context = get_context(&test_state(), "auth", None, ContextDepth::Constraints).unwrap();
        assert_eq!(
            context["decisions"],
            json!([{ "name": "use-jwt", "choice": "JWT with DPoP binding" }])
        );
        assert!(context.get("related_decisions").is_none());
        let brief = context["brief"].as_str().unwrap();
        assert!(
            brief.contains("CONSTRAINTS (auth):\n- JWT with DPoP binding\n"),
            "{brief}"
        );
    }

    #[test]
    fn a_proposed_decision_is_marked_in_both_briefs_and_counted() {
        let mut state = test_state();
        state.decisions.get_mut("use-jwt").unwrap().status = Status::Proposed {
            accepted_text: None,
        };
        let mut revision = decision("auth", "Rotate keys daily");
        revision.status = Status::Proposed {
            accepted_text: Some(AcceptedText {
                choice: "Rotate keys weekly".into(),
                reason: "The text the owner accepted".into(),
                alternatives: vec![],
                via: None,
            }),
        };
        state.decisions.insert("rotate".into(), revision);

        let context = full(&state, "auth");
        let brief = context["brief"].as_str().unwrap();
        assert!(
            brief.contains("Rotate keys daily (Reason of the sample decision) (proposed revision"),
            "{brief}"
        );
        assert!(
            brief.contains("JWT with DPoP binding (Reason of the sample decision) (proposed)"),
            "{brief}"
        );
        assert_eq!(context["health"]["proposed"], 2);
        let compact = get_context(&state, "auth", None, ContextDepth::Constraints).unwrap();
        assert!(
            compact["brief"]
                .as_str()
                .unwrap()
                .contains("- JWT with DPoP binding (proposed)\n")
        );
    }

    #[test]
    fn a_decision_whose_anchored_files_are_all_gone_is_flagged() {
        let tmp = tempfile::TempDir::new().unwrap();
        fs::create_dir(tmp.path().join("src")).unwrap();
        fs::write(tmp.path().join("src/live.rs"), "").unwrap();
        let mut state = test_state();
        state.code_root = tmp.path().to_path_buf();
        state
            .decisions
            .insert("gone".into(), anchored("auth", &["src/gone.rs"]));
        state.decisions.insert(
            "mixed".into(),
            anchored("auth", &["src/gone.rs", "src/live.rs"]),
        );

        let context = full(&state, "auth");

        assert_eq!(context["health"]["orphaned_anchors"], 1);
        assert!(
            context["health"]["warning"]
                .as_str()
                .unwrap()
                .contains("1 decisions")
        );
        let brief = context["brief"].as_str().unwrap();
        assert_eq!(brief.matches("ORPHANED ANCHORS").count(), 1, "{brief}");
        assert!(
            brief.contains("  Code: src/gone.rs, src/live.rs\n"),
            "{brief}"
        );
        let anchors = &context["decisions"][1]["anchors"];
        assert_eq!(
            anchors,
            &json!([{ "file": "src/gone.rs" }, { "file": "src/live.rs" }])
        );
    }

    #[test]
    fn health_warns_on_the_most_pressing_count_first() {
        let accepted: Vec<Decision> = (0..21).map(|_| sample_decision("auth")).collect();
        let own: Vec<Named<'_>> = accepted.iter().map(|d| ("d", d)).collect();
        assert!(
            health(&own, 3)["warning"]
                .as_str()
                .unwrap()
                .starts_with("Too many")
        );
        assert!(health(&own[..2], 0)["warning"].is_null());
        assert!(
            health(&own[..2], 1)["warning"]
                .as_str()
                .unwrap()
                .contains("gone")
        );
    }

    #[test]
    fn decisions_for_a_file_carry_their_matching_anchors() {
        let mut state = test_state();
        let mut tagged = anchored("database", &["src/store/write.rs", "src/mcp/mod.rs"]);
        tagged.tags.insert("reliability".into());
        state.decisions.insert("atomic".into(), tagged);
        state
            .decisions
            .insert("dir".into(), anchored("auth", &["src/store"]));

        let found = get_decisions_for_file(&state, "./src/store/write.rs").unwrap();

        assert_eq!(found["file"], "src/store/write.rs");
        assert_eq!(found["count"], 2);
        assert_eq!(found["decisions"][0]["name"], "dir");
        assert_eq!(found["decisions"][1]["name"], "atomic");
        assert_eq!(found["decisions"][1]["tags"], json!(["reliability"]));
        assert_eq!(
            found["decisions"][1]["matching_anchors"],
            json!([{ "file": "src/store/write.rs" }])
        );
        assert_eq!(found["decisions"][1]["status"], "accepted");
    }

    #[test]
    fn decisions_for_a_path_outside_the_root_are_refused() {
        let err = get_decisions_for_file(&test_state(), "/etc/passwd").unwrap_err();
        assert!(err.starts_with("invalid file path:"), "{err}");
    }
}
