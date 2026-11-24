use std::path::Path;

use serde_json::{Map, Value};

use crate::Result;
use crate::console::out;
use crate::discovery::discover;
use crate::mcp::twin::{self, Part, Report, ToolError};

use super::Format;

/// `trurlic status`: the `status` tool, its section and origin given as
/// flags, printed for a terminal or as the tool's JSON. It writes nothing,
/// to the graph's repository neither.
pub(crate) fn status(
    cwd: &Path,
    section: Option<&str>,
    origin: Option<&str>,
    format: Format,
) -> std::result::Result<(), ToolError> {
    let mut arguments = Map::new();
    if let Some(section) = section {
        arguments.insert("section".into(), Value::from(section));
    }
    if let Some(origin) = origin {
        arguments.insert("origin".into(), Value::from(origin));
    }
    let store = discover(cwd)?;
    let report = twin::status(&store, &arguments);
    match (report, format) {
        (Ok(report), Format::Json) => Ok(out!("{}", twin::result_json(&report))?),
        (Ok(report), Format::Text) => Ok(print(&report, origin)?),
        (Err(refusal), Format::Json) => {
            out!("{}", twin::refusal_json(&refusal))?;
            Err(refusal)
        }
        (Err(refusal), Format::Text) => Err(refusal),
    }
}

fn print(report: &Report, origin: Option<&str>) -> Result<()> {
    let counts = &report.counts;
    let placement = report.placement.as_str();
    out!("project: {} ({placement})", report.project)?;
    if let (Some(graph), Some(code_root)) = (&report.graph, &report.code_root) {
        out!("graph: {graph}")?;
        out!("code root: {code_root}")?;
    }
    out!("components: {}", counts.components)?;
    let under = origin.map_or_else(String::new, |origin| format!(" under `{origin}`"));
    let (all, wide, proposed) = (counts.decisions, counts.project_wide, counts.proposed);
    out!("decisions{under}: {all} ({wide} project-wide, {proposed} proposed)")?;
    out!("edges: {}", counts.edges)?;
    if counts.non_canonical > 0 {
        out!("not in canonical form: {}", counts.non_canonical)?;
    }
    if let Some(components) = &report.components {
        out!("by component:")?;
        for row in &components.items {
            let (name, decisions, proposed) = (&row.name, row.decisions, row.proposed);
            out!("  {name}: {decisions} ({proposed} proposed)")?;
        }
        omitted(components)?;
    }
    if let Some(issues) = report.issues.as_ref().filter(|issues| issues.total > 0) {
        out!("issues: {}", issues.total)?;
        for issue in &issues.items {
            out!("  {}", issue.message)?;
        }
        omitted(issues)?;
    }
    for finding in report.history.iter().flat_map(|history| &history.items) {
        out!("history: {}", finding.message)?;
    }
    Ok(())
}

fn omitted<T>(part: &Part<T>) -> Result<()> {
    match &part.more {
        Some(more) => out!("  and {} more: {more}", part.omitted),
        None => Ok(()),
    }
}
