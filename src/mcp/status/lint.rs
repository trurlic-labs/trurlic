//! The `lint` section of `status`: what to read that refuses nothing, of
//! three kinds, each gathered to the most a result can show and counted
//! whole. Likely duplicate pairs come best first; quality findings, each
//! rule of the bar a stored decision breaks, and the decisions with only
//! file anchors come by decision name. The section lists them in that order
//! of kinds, each kind fitted to the budget on its own (`fill`). The
//! `[quality]` patterns are judged only for the section asked for alone
//! (`patterns`).

use crate::budget::MAX_TOOL_RESULT_BYTES;
use crate::lint::duplicates::{Score, Similarity};
use crate::lint::quality;
use crate::mcp::spec::output::{Count, Nested, Str, Tag};
use crate::mcp::spec::{Output, Shown, to_value};
use crate::mcp::truncate::json_len;
use crate::store::limits::MAX_NAME_BYTES;
use crate::store::{Decision, Level, ProjectState, QualityFault, Shortfall};

use super::{Listed, Part};

mod patterns;

pub(super) use patterns::Patterns;
pub(crate) use patterns::{Cut, PatternCounts};
use patterns::{PAGE_MORE, Reading};

/// What every surface says of the duplicate pairs a listing leaves out.
pub(crate) const PAIRS_MORE: &str = "the section lists the best pairs; the ones left out rank \
     at or below the last listed, and come into view as the listed ones are settled (merged, \
     or revised apart)";

/// What the `lint` section says of what it leaves out, true of each kind.
pub(super) const LINT_MORE: &str = "each kind lists what fits and `counts.lint` counts it \
     whole: the duplicate pairs left out rank at or below the last listed; the \
     quality findings and the \
     decisions with only file anchors left out follow the last listed by decision \
     name, and `section: \"lint\"` with `from` lists the quality findings from a \
     decision on; they come into view as the listed ones are settled (pairs merged \
     or revised apart, decisions brought to the bar or anchored to a symbol); \
     `counts.lint.patterns` says which decisions the `[quality]` patterns did not \
     read and how to read them";

/// A finding of `lint`: something to read, which refuses nothing.
pub(crate) enum LintItem {
    /// Two decisions that likely state one rule twice, `decision` first by
    /// name, and their cosine to six decimals.
    Duplicate {
        decision: String,
        other: String,
        score: String,
    },
    /// A rule of the bar the stored decision breaks.
    Quality(QualityItem),
    /// A decision whose anchors all name a file and no symbol: none says
    /// what part of its file the decision constrains.
    FileAnchorsOnly { decision: String },
}

/// A rule of the bar a stored decision breaks, as a refusal names it.
pub(crate) struct QualityItem {
    pub(crate) decision: String,
    /// The field, an element of a list by its index: `alternatives[1]`.
    pub(crate) field: String,
    pub(crate) rule: &'static str,
    pub(crate) level: Level,
    /// A limit's measure and bound, in the rule's unit.
    pub(crate) limit: Option<Measure>,
    /// A pattern's first match, or where its cache gave up.
    pub(crate) pattern: Option<Matched>,
}

/// The pattern of a finding: its index in `[quality] patterns` and the
/// byte offset of its first match in the field's text, `None` when its
/// cache gave up there.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Matched {
    pub(crate) index: usize,
    pub(crate) at: Option<usize>,
}

pub(crate) struct Measure {
    pub(crate) measured: usize,
    pub(crate) bound: usize,
    pub(crate) unit: &'static str,
}

/// The kind of a [`LintItem`], one per variant, in the section's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LintKind {
    Duplicate,
    Quality,
    FileAnchorsOnly,
}

impl LintKind {
    pub(super) const ALL: [Self; 3] = [Self::Duplicate, Self::Quality, Self::FileAnchorsOnly];

    const fn as_str(self) -> &'static str {
        match self {
            Self::Duplicate => "duplicate",
            Self::Quality => "quality",
            Self::FileAnchorsOnly => "file_anchors_only",
        }
    }
}

impl LintItem {
    const fn kind(&self) -> &'static LintKind {
        match self {
            Self::Duplicate { .. } => &LintKind::Duplicate,
            Self::Quality(_) => &LintKind::Quality,
            Self::FileAnchorsOnly { .. } => &LintKind::FileAnchorsOnly,
        }
    }
}

/// How many findings of each kind there are, and how far the `[quality]`
/// patterns were judged.
pub(crate) struct LintCounts {
    pub(crate) duplicates: usize,
    pub(crate) quality: usize,
    pub(crate) file_anchors_only: usize,
    /// When `[quality]` has patterns.
    pub(crate) patterns: Option<PatternCounts>,
}

/// The findings of each kind, in the section's order, and their counts.
pub(super) struct Gathered<'s> {
    pub(super) items: LintItems<'s>,
    pub(super) counts: LintCounts,
}

/// The findings of each kind before the budget cuts them, and the page of
/// quality findings the patterns read, when they read one.
pub(super) struct LintItems<'s> {
    pub(super) kinds: Vec<(LintKind, Listed<LintItem>)>,
    page: Option<Page<'s>>,
}

/// A page of quality findings: how far the patterns read, and the
/// decision of each finding listed, in order.
struct Page<'s> {
    reading: Reading<'s>,
    decisions: Vec<&'s str>,
}

impl LintItems<'_> {
    /// The bytes the counts of a page may grow by once the budget cuts its
    /// listing: a `next` and why, beside the counts the frame measured.
    pub(super) fn reserve(&self) -> usize {
        self.page.as_ref().map_or(0, |_| {
            MAX_NAME_BYTES + r#","next":"","why":"work_limit","more":"""#.len() + PAGE_MORE.len()
        })
    }
}

/// Every finding of `state`, of the decisions recorded under `origin`
/// when one is given (a pair when either decision is), each kind to the
/// most one result can show; the quality findings of the patterns only
/// when `patterns` is `Judged`, within a bound on the bytes they read, and
/// then only of the decisions it reads from.
pub(super) fn gather<'s>(
    state: &'s ProjectState,
    origin: Option<&str>,
    patterns: Patterns<'_>,
) -> Gathered<'s> {
    let under = |decision: &Decision| {
        origin.is_none_or(|origin| decision.origin.as_deref() == Some(origin))
    };
    let touching = |name: &str| state.decisions.get(name).is_some_and(under);
    let pairs = Similarity::of(state).pairs(lint_limit(LintKind::Duplicate), touching);
    let limit = lint_limit(LintKind::Quality);
    let wanted = |name: &str, decision: &Decision| patterns.reads(name) && under(decision);
    let (mut shortfalls, reading) = patterns::shortfalls(state, patterns, limit, wanted);
    let page = reading.map(|reading| Page::of(reading, &mut shortfalls.first));
    let files = quality::file_anchors_only(state, lint_limit(LintKind::FileAnchorsOnly), under);
    let counts = LintCounts {
        duplicates: pairs.total,
        quality: shortfalls.total,
        file_anchors_only: files.total,
        patterns: page.as_ref().map(Page::counts),
    };
    let duplicates = pairs.best.into_iter().map(|pair| LintItem::Duplicate {
        decision: pair.first.to_owned(),
        other: pair.second.to_owned(),
        score: pair.score.to_string(),
    });
    let quality = shortfalls
        .first
        .into_iter()
        .map(|(name, shortfall)| quality_item(name, shortfall));
    let anchored = files
        .first
        .into_iter()
        .map(|name| LintItem::FileAnchorsOnly {
            decision: name.to_owned(),
        });
    let kinds = vec![
        (LintKind::Duplicate, Listed::of(pairs.total, duplicates)),
        (LintKind::Quality, Listed::of(shortfalls.total, quality)),
        (LintKind::FileAnchorsOnly, Listed::of(files.total, anchored)),
    ];
    Gathered {
        items: LintItems { kinds, page },
        counts,
    }
}

/// The `lint` section of `items`, each kind fitted by `fit`. A page of
/// quality findings ends at a decision's boundary, and `counts` names the
/// decision where the next page starts.
pub(super) fn fitted(
    items: LintItems<'_>,
    fit: impl Fn(LintKind, Listed<LintItem>) -> Part<LintItem>,
    mut counts: Option<&mut LintCounts>,
) -> Part<LintItem> {
    let LintItems { kinds, page } = items;
    let mut section = Part {
        total: 0,
        items: Vec::new(),
        omitted: 0,
        more: None,
    };
    for (kind, listed) in kinds {
        let mut part = fit(kind, listed);
        if let (LintKind::Quality, Some(page)) = (kind, &page) {
            let listed_to = page.end(&mut part);
            if let Some(counts) = counts.as_deref_mut() {
                counts.patterns = Some(page.reading.counts(listed_to));
            }
        }
        section.total += part.total;
        section.omitted += part.omitted;
        section.items.extend(part.items);
        section.more = section.more.or(part.more);
    }
    section
}

impl<'s> Page<'s> {
    /// The page `reading` read, of the findings it kept, `first`, which it
    /// cuts to the decisions it lists whole. The pass's last decision may
    /// hold more findings than it kept; a first decision that does keeps
    /// those, and the page moves on past it.
    fn of(reading: Reading<'s>, first: &mut Vec<(&'s str, Shortfall)>) -> Self {
        let partial = reading.page_end();
        if first.iter().any(|&(name, _)| Some(name) != partial) {
            first.retain(|&(name, _)| Some(name) != partial);
        }
        let decisions = first.iter().map(|&(name, _)| name).collect();
        Self { reading, decisions }
    }

    /// The counts of the page before the budget cuts its listing.
    fn counts(&self) -> PatternCounts {
        self.reading.counts(self.reading.page_end())
    }

    /// End `part`, the quality findings the budget kept, at a decision's
    /// boundary: the first decision whose findings it does not hold whole.
    /// A page whose first decision's findings alone pass the room lists
    /// those that fit and ends after it, so every page moves on.
    fn end<'p>(&'p self, part: &mut Part<LintItem>) -> Option<&'p str> {
        let kept = part.items.len();
        let listed_to = self
            .decisions
            .get(kept)
            .copied()
            .or_else(|| self.reading.page_end())?;
        let whole = self
            .decisions
            .get(..kept)
            .map_or(kept, |kept| kept.partition_point(|&name| name < listed_to));
        if whole == 0 {
            return self.reading.after(listed_to);
        }
        part.items.truncate(whole);
        part.omitted = part.total.saturating_sub(part.items.len());
        Some(listed_to)
    }
}

fn quality_item(decision: &str, shortfall: Shortfall) -> LintItem {
    let Shortfall {
        field,
        element,
        level,
        fault,
    } = shortfall;
    let field = match element {
        Some(index) => format!("{}[{index}]", field.as_str()),
        None => field.as_str().to_owned(),
    };
    let (limit, pattern) = measure(&fault);
    LintItem::Quality(QualityItem {
        decision: decision.to_owned(),
        field,
        rule: fault.as_str(),
        level,
        limit,
        pattern,
    })
}

/// What a finding of `fault` shows: a limit's measure, or its pattern.
const fn measure(fault: &QualityFault) -> (Option<Measure>, Option<Matched>) {
    match fault {
        &QualityFault::Limit {
            limit,
            measured,
            bound,
        } => {
            let unit = limit.unit();
            (
                Some(Measure {
                    measured,
                    bound,
                    unit,
                }),
                None,
            )
        }
        QualityFault::Pattern(found) => {
            let (index, at) = (found.index, Some(found.at));
            (None, Some(Matched { index, at }))
        }
        &QualityFault::GaveUp { index } => (None, Some(Matched { index, at: None })),
        QualityFault::OriginRequired => (None, None),
    }
}

/// The most findings of `kind` one result can show: the budget over the
/// smallest finding of that kind, with the comma before it. A kind
/// gathered to it keeps every finding of it a result can show, and no more.
fn lint_limit(kind: LintKind) -> usize {
    let smallest = match kind {
        LintKind::Duplicate => LintItem::Duplicate {
            decision: "a".into(),
            other: "b".into(),
            score: Score::THRESHOLD.to_string(),
        },
        LintKind::Quality => LintItem::Quality(QualityItem {
            decision: "a".into(),
            field: "tags".into(),
            rule: "pattern",
            level: Level::Warn,
            limit: None,
            pattern: Some(Matched {
                index: 0,
                at: Some(0),
            }),
        }),
        LintKind::FileAnchorsOnly => LintItem::FileAnchorsOnly {
            decision: "a".into(),
        },
    };
    MAX_TOOL_RESULT_BYTES / json_len(&to_value(&smallest)).saturating_add(1)
}

const LEVEL: Tag<Level> = Tag {
    all: &[Level::Refuse, Level::Warn],
    name: Level::as_str,
};

impl Output for LintItem {
    fn declare(fields: &mut Shown<'_, Self>) {
        let kind = Tag {
            all: &LintKind::ALL,
            name: LintKind::as_str,
        };
        let about = "The finding's kind: `duplicate`, two decisions whose choice and reason \
                     have a TF-IDF cosine of 0.5 or more; `quality`, a rule of the bar the \
                     stored decision breaks; `file_anchors_only`, a decision whose anchors all \
                     name a file and no symbol, so none says what part of it the decision \
                     constrains.";
        fields.required("kind", about, &kind, |item| item.kind());
        let decision = "The decision it is about; a duplicate's first by name.";
        fields.required("decision", decision, &Str, |item| match item {
            LintItem::Duplicate { decision, .. } | LintItem::FileAnchorsOnly { decision } => {
                decision
            }
            LintItem::Quality(found) => &found.decision,
        });
        declare_duplicate(fields);
        declare_quality(fields);
    }
}

fn declare_duplicate(fields: &mut Shown<'_, LintItem>) {
    let other = "A duplicate's other decision.";
    fields.optional("other", other, &Str, |item| match item {
        LintItem::Duplicate { other, .. } => Some(other),
        LintItem::Quality(_) | LintItem::FileAnchorsOnly { .. } => None,
    });
    let score = "A duplicate's cosine, to six decimals.";
    fields.optional("score", score, &Str, |item| match item {
        LintItem::Duplicate { score, .. } => Some(score),
        LintItem::Quality(_) | LintItem::FileAnchorsOnly { .. } => None,
    });
}

/// The quality finding `item` is, if it is one.
const fn quality(item: &LintItem) -> Option<&QualityItem> {
    match item {
        LintItem::Quality(found) => Some(found),
        LintItem::Duplicate { .. } | LintItem::FileAnchorsOnly { .. } => None,
    }
}

fn declare_quality(fields: &mut Shown<'_, LintItem>) {
    let field = "A quality finding's field, an element by its index: `alternatives[1]`.";
    fields.optional("field", field, &Str, |item| {
        quality(item).map(|found| found.field.as_str())
    });
    let rule = "The rule as a refusal names it: a limit's, `pattern`, or `pattern_gave_up` where \
                the pattern's cache filled before it could judge the field, which a write of it \
                refuses.";
    fields.optional("rule", rule, &Str, |item| {
        quality(item).map(|found| found.rule)
    });
    let level = "`refuse` for a limit, a give-up and a `refuse` pattern, `warn` for a `warn` one.";
    fields.optional("level", level, &LEVEL, |item| {
        quality(item).map(|found| &found.level)
    });
    let limit = Nested::<Measure>::new();
    let about = "A limit's measure and bound, in the rule's unit.";
    fields.optional("limit", about, &limit, |item| {
        quality(item).and_then(|found| found.limit.as_ref())
    });
    let pattern = "A pattern's index in `[quality] patterns`.";
    fields.optional("pattern", pattern, &Count, |item| {
        quality(item).and_then(|found| found.pattern.as_ref().map(|matched| &matched.index))
    });
    let at = "The byte offset of the pattern's first match in the field's text.";
    fields.optional("at", at, &Count, |item| {
        quality(item).and_then(|found| found.pattern.as_ref()?.at.as_ref())
    });
}

impl Output for Measure {
    fn declare(fields: &mut Shown<'_, Self>) {
        fields.required("measured", "What the field measures.", &Count, |m| {
            &m.measured
        });
        fields.required("bound", "The limit it passes.", &Count, |m| &m.bound);
        let unit = "What both count: `words`, `alternatives` or `anchors`.";
        fields.required("unit", unit, &Str, |m| m.unit);
    }
}

impl Output for LintCounts {
    fn declare(fields: &mut Shown<'_, Self>) {
        let duplicates = "Likely duplicate pairs.";
        fields.required("duplicates", duplicates, &Count, |c| &c.duplicates);
        let quality = "Rules of the bar the stored decisions break: every limit's, and the \
                       patterns' in the decisions `patterns` says they read.";
        fields.required("quality", quality, &Count, |c| &c.quality);
        let files = "Decisions whose anchors all name a file and no symbol.";
        fields.required("file_anchors_only", files, &Count, |c| &c.file_anchors_only);
        let patterns = "How far the `[quality]` patterns were judged, when it has any.";
        let kind = Nested::<PatternCounts>::new();
        fields.optional("patterns", patterns, &kind, |c| c.patterns.as_ref());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each kind is gathered to as many of its smallest findings as one
    /// result holds with their commas, and no more: a kind with larger
    /// findings keeps fewer.
    #[test]
    fn each_kind_keeps_what_a_result_can_show_of_it_and_no_more() {
        let mut limits = Vec::new();
        for kind in LintKind::ALL {
            let limit = lint_limit(kind);
            let held = |count: usize| {
                let items: Vec<LintItem> = (0..count)
                    .map(|_| match kind {
                        LintKind::Duplicate => LintItem::Duplicate {
                            decision: "a".into(),
                            other: "b".into(),
                            score: Score::THRESHOLD.to_string(),
                        },
                        LintKind::Quality => quality_item(
                            "a",
                            Shortfall {
                                field: crate::store::DecisionField::Tags,
                                element: None,
                                level: Level::Warn,
                                fault: QualityFault::Pattern(crate::store::PatternMatch {
                                    index: 0,
                                    at: 0,
                                    found: String::new(),
                                    message: String::new(),
                                }),
                            },
                        ),
                        LintKind::FileAnchorsOnly => LintItem::FileAnchorsOnly {
                            decision: "a".into(),
                        },
                    })
                    .collect();
                let bytes: usize = items.iter().map(|item| json_len(&to_value(item)) + 1).sum();
                bytes <= MAX_TOOL_RESULT_BYTES
            };
            assert!(held(limit), "{kind:?}");
            assert!(!held(limit + 1), "{kind:?}");
            limits.push(limit);
        }
        assert!(limits[0] < limits[2] && limits[1] < limits[2], "{limits:?}");
    }
}
