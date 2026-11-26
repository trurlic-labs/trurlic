//! A finding of the `lint` section, of one of three kinds, declared once
//! for the output schema and the JSON, and the most findings of each kind
//! one result can show.

use crate::budget::MAX_TOOL_RESULT_BYTES;
use crate::lint::duplicates::Score;
use crate::store::{Level, QualityFault, Shortfall};
use crate::tools::spec::output::{Count, Nested, Str, Tag};
use crate::tools::spec::{Output, Shown, to_value};
use crate::tools::truncate::json_len;

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
pub(in crate::tools::status) enum LintKind {
    Duplicate,
    Quality,
    FileAnchorsOnly,
}

impl LintKind {
    const ALL: [Self; 3] = [Self::Duplicate, Self::Quality, Self::FileAnchorsOnly];

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

pub(super) fn quality_item(decision: &str, shortfall: Shortfall) -> LintItem {
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
        &QualityFault::GaveUp { index, .. } | &QualityFault::CacheTotal { largest: index, .. } => {
            (None, Some(Matched { index, at: None }))
        }
        QualityFault::OriginRequired => (None, None),
    }
}

/// The most findings of `kind` one result can show: the budget over the
/// smallest finding of that kind, with the comma before it. A kind
/// gathered to it keeps every finding of it a result can show, and no more.
pub(super) fn lint_limit(kind: LintKind) -> usize {
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
    let rule = "The rule as a refusal names it: a limit's, `pattern`, `pattern_gave_up` where \
                the pattern's cache filled before it could judge the field, or \
                `pattern_cache_total` where the patterns' caches together passed their total \
                over the decision, `pattern` naming the one that grew the most; a write of it \
                refuses both.";
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
