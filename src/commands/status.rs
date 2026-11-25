use std::path::Path;

use serde_json::{Map, Value};

use crate::Result;
use crate::console::out;
use crate::mcp::twin::{
    self, AnchorCounts, Cut, LintItem, Part, PatternCounts, QualityCounts, Report, ToolError,
};

use super::{Format, answer};

/// The arguments of `status` that `trurlic status` takes as flags.
#[derive(Debug, Clone, Copy)]
pub(crate) struct StatusFlags<'a> {
    pub(crate) section: Option<&'a str>,
    pub(crate) origin: Option<&'a str>,
    pub(crate) from: Option<&'a str>,
}

/// `trurlic status`: the `status` tool, its arguments given as flags,
/// printed for a terminal or as the tool's JSON. It writes nothing, to
/// the graph's repository neither.
pub(crate) fn status(
    cwd: &Path,
    flags: StatusFlags<'_>,
    format: Format,
) -> std::result::Result<(), ToolError> {
    let mut arguments = Map::new();
    for (key, flag) in [
        ("section", flags.section),
        ("origin", flags.origin),
        ("from", flags.from),
    ] {
        if let Some(flag) = flag {
            arguments.insert(key.into(), Value::from(flag));
        }
    }
    answer(twin::status(cwd, &arguments), format, |report| {
        print(report, flags.origin)
    })
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
    quality(&report.quality)?;
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
    lint(report)?;
    anchors(report)
}

/// The anchor findings, counted by kind, then a line each, when the
/// report has the section: the count line even with none, so a clean tree
/// is told from one not asked.
fn anchors(report: &Report) -> Result<()> {
    let (Some(section), Some(counts)) = (&report.anchors, &report.counts.anchors) else {
        return Ok(());
    };
    let AnchorCounts {
        unresolved,
        malformed,
        ambiguous,
        unverified,
        weak,
    } = counts;
    out!(
        "anchors: {unresolved} unresolved, {malformed} malformed, {ambiguous} ambiguous, \
         {unverified} unverified, {weak} weak"
    )?;
    for item in &section.items {
        let (kind, anchor, decision) = (item.kind.as_str(), &item.anchor, &item.decision);
        out!("  {kind}: `{anchor}` of `{decision}`: {}", item.cause)?;
    }
    omitted(section)
}

/// The lint section's counts and findings, a line each, when it has any.
fn lint(report: &Report) -> Result<()> {
    let lint_counts = report.counts.lint.as_ref();
    if let Some(patterns) = lint_counts.and_then(|counts| counts.patterns.as_ref()) {
        unjudged(patterns, &report.quality)?;
    }
    let (Some(lint), Some(counts)) = (report.lint.as_ref(), report.counts.lint.as_ref()) else {
        return Ok(());
    };
    if lint.total == 0 {
        return Ok(());
    }
    out!(
        "lint: {} ({} likely duplicate pair(s), {} quality finding(s), {} decision(s) with \
         only file anchors)",
        lint.total,
        counts.duplicates,
        counts.quality,
        counts.file_anchors_only
    )?;
    for item in &lint.items {
        out!("  {}", finding(item))?;
    }
    omitted(lint)
}

fn finding(item: &LintItem) -> String {
    match item {
        LintItem::Duplicate {
            decision,
            other,
            score,
        } => format!("likely duplicates ({score}): `{decision}` and `{other}`"),
        LintItem::Quality(found) => {
            let (decision, field, level) = (&found.decision, &found.field, found.level.as_str());
            let what = match (&found.limit, found.pattern) {
                (Some(limit), _) => format!(
                    "{}, {} {} against {}",
                    found.rule, limit.measured, limit.unit, limit.bound
                ),
                (None, Some(matched)) => {
                    let (index, at) = (matched.index, matched.at);
                    format!("matches `[quality] patterns[{index}]` at byte {at}")
                }
                (None, None) => found.rule.to_owned(),
            };
            format!("`{decision}` `{field}`: {what} ({level})")
        }
        LintItem::FileAnchorsOnly { decision } => {
            format!(
                "`{decision}`: only file anchors, none naming the part of its file it constrains"
            )
        }
    }
}

/// Why the `[quality]` patterns left decisions unjudged, if they did.
fn unjudged(patterns: &PatternCounts, quality: &QualityCounts) -> Result<()> {
    match &patterns.cut {
        None => Ok(()),
        Some(Cut::NotAsked) => out!(
            "lint: the limits alone; `trurlic status --section lint` judges the {} [quality] \
             pattern(s) too",
            quality.refusing + quality.warning
        ),
        Some(Cut::WorkLimit(work)) => {
            out!(
                "lint: the [quality] patterns read {} of {} decision(s), by name, within a work \
                 of {} state-bytes of the {} they would do",
                patterns.judged,
                patterns.judged + patterns.unjudged,
                work.bound,
                work.measured
            )?;
            if let Some(next) = &work.next {
                out!("  `trurlic status --section lint --from {next}` reads on from `{next}`")?;
            }
            match work.too_costly {
                0 => Ok(()),
                costly => {
                    out!("  {costly} decision(s) pass the bound alone, so no call reads them")
                }
            }
        }
        Some(Cut::Uncompiled(uncompiled)) => out!(
            "lint: `[quality] patterns[{}]` does not compile, so no pattern was judged: {}",
            uncompiled.index,
            uncompiled.message
        ),
    }
}

/// The rules `[quality]` adds, counted, or that it adds none.
fn quality(counts: &QualityCounts) -> Result<()> {
    let (limits, refusing, warning) = (counts.limits, counts.refusing, counts.warning);
    if limits + refusing + warning == 0 {
        return out!("quality: the built-in rules");
    }
    out!(
        "quality: {limits} limit(s), {refusing} refusing and {warning} warning pattern(s) in \
         [quality]"
    )
}

fn omitted<T>(part: &Part<T>) -> Result<()> {
    match &part.more {
        Some(more) => out!("  and {} more: {more}", part.omitted),
        None => Ok(()),
    }
}
