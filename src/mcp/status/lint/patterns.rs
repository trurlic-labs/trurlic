//! How far a `status` call judges the `[quality]` patterns: the default
//! call judges the limits alone, and the `lint` section asked for alone
//! judges the patterns too, over the decisions by name while their work
//! stays within [`MAX_REPORT_PATTERN_WORK`]. The counts say how many
//! decisions the patterns read, why they read no more, and the decision
//! where a call with `from` reads on.

use crate::lint::quality::{self, Found, Reach};
use crate::mcp::spec::output::{Count, Nested, Str, Tag};
use crate::mcp::spec::{Output, Shown};
use crate::store::limits::MAX_REPORT_PATTERN_WORK;
use crate::store::{Bar, Decision, PatternFault, ProjectState, Shortfall};

use crate::mcp::result::ToolError;
use crate::mcp::status::Section;

/// The decisions the `[quality]` patterns read, and why they read no more.
pub(crate) struct PatternCounts {
    pub(crate) judged: usize,
    pub(crate) unjudged: usize,
    /// Why some decisions went unjudged, when some did.
    pub(crate) cut: Option<Cut>,
}

/// Why the `[quality]` patterns left decisions unjudged.
pub(crate) enum Cut {
    /// The call judged the limits alone: only `section: "lint"` judges the
    /// patterns.
    NotAsked,
    /// The decisions after the last judged, by name, would take the work
    /// of the patterns past the bound.
    WorkLimit(Work),
    /// A pattern does not compile, so none is judged.
    Uncompiled(Uncompiled),
}

/// The work of the patterns over every decision wanted, the most one call
/// does, in state-bytes, and where the reading stopped.
pub(crate) struct Work {
    pub(crate) measured: usize,
    pub(crate) bound: usize,
    /// The first decision the bound left unread: a call with `from` set to
    /// it reads on.
    pub(crate) next: Option<String>,
    /// The decisions whose own work passes the bound, which no call reads.
    pub(crate) too_costly: usize,
}

/// A `[quality]` pattern that does not compile: no pattern is judged, the
/// limits are, and `trurlic check` reports it.
pub(crate) struct Uncompiled {
    pub(crate) index: usize,
    pub(crate) message: String,
}

/// The kind of a [`Cut`], one per variant.
#[derive(Debug, Clone, Copy)]
enum CutKind {
    NotAsked,
    WorkLimit,
    Uncompiled,
}

impl CutKind {
    const ALL: [Self; 3] = [Self::NotAsked, Self::WorkLimit, Self::Uncompiled];

    const fn as_str(self) -> &'static str {
        match self {
            Self::NotAsked => "not_asked",
            Self::WorkLimit => "work_limit",
            Self::Uncompiled => "uncompiled",
        }
    }
}

impl Cut {
    const fn kind(&self) -> &'static CutKind {
        match self {
            Self::NotAsked => &CutKind::NotAsked,
            Self::WorkLimit(_) => &CutKind::WorkLimit,
            Self::Uncompiled(_) => &CutKind::Uncompiled,
        }
    }

    /// How the decisions left unjudged come to be judged.
    const fn more(&self) -> &'static str {
        match self {
            Self::NotAsked => {
                "the call judges the limits alone; `section: \"lint\"` judges the patterns \
                 too, within a work limit"
            }
            Self::WorkLimit(_) => {
                "the patterns read the decisions by name while their work stays within \
                 `work.bound`: call again with `from` set to `next` to read on from there, \
                 until no `next` is left; a decision in `too_costly` passes the bound \
                 alone, so no call reads it"
            }
            Self::Uncompiled(_) => "no pattern is judged while one does not compile",
        }
    }
}

/// Whether a call judges the `[quality]` patterns, which cost the bytes of
/// every decision they read, and from which decision on: the `lint`
/// section asked for alone does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::mcp::status) enum Patterns<'a> {
    /// The decisions named `from` or after, all when `None`.
    Judged {
        from: Option<&'a str>,
    },
    NotAsked,
}

impl<'a> Patterns<'a> {
    /// What a call for `section` judges: the patterns, from `from` on, for
    /// the `lint` section alone, else the limits alone. `from` beside any
    /// other section is refused, since only the patterns read the
    /// decisions a page at a time.
    pub(in crate::mcp::status) fn of(
        section: Option<Section>,
        from: Option<&'a str>,
    ) -> Result<Self, ToolError> {
        match (section, from) {
            (Some(Section::Lint), from) => Ok(Self::Judged { from }),
            (Some(Section::Issues | Section::History | Section::Components) | None, None) => {
                Ok(Self::NotAsked)
            }
            (Some(Section::Issues | Section::History | Section::Components) | None, Some(_)) => {
                Err(ToolError::invalid(
                    "from",
                    "`from` reads the `[quality]` patterns on from a decision, which only \
                     `section: \"lint\"` judges: give it with that section",
                ))
            }
        }
    }

    /// Whether `name` is among the decisions whose quality findings the
    /// call reports.
    pub(super) fn reads(self, name: &str) -> bool {
        match self {
            Self::Judged { from } => from.is_none_or(|from| name >= from),
            Self::NotAsked => true,
        }
    }
}

/// The quality findings of the decisions `wanted` admits by name, the
/// first `limit` of them. When `[quality]` has patterns, also how far the
/// patterns read: as far as the work limit lets when `patterns` is
/// `Judged`, else not at all, with nothing compiled.
pub(super) fn shortfalls<'s>(
    state: &'s ProjectState,
    patterns: Patterns<'_>,
    limit: usize,
    wanted: impl Fn(&str, &Decision) -> bool,
) -> (Found<(&'s str, Shortfall)>, Option<PatternCounts>) {
    let table = &state.project.quality;
    let (bar, uncompiled) = match patterns {
        Patterns::Judged { .. } => Bar::for_report(table),
        Patterns::NotAsked => (Bar::limits_only(table), None),
    };
    let found = quality::shortfalls(state, &bar, limit, MAX_REPORT_PATTERN_WORK, wanted);
    let read =
        (!table.patterns.is_empty()).then(|| pattern_counts(patterns, uncompiled, &found.reach));
    (found.found, read)
}

/// How far the patterns read the decisions wanted: as far as `reach`
/// says when the call judged them, else not at all.
fn pattern_counts(
    patterns: Patterns<'_>,
    uncompiled: Option<PatternFault>,
    reach: &Reach<'_>,
) -> PatternCounts {
    let (judged, cut) = match (patterns, uncompiled) {
        (Patterns::NotAsked, _) => (0, Cut::NotAsked),
        (Patterns::Judged { .. }, Some(fault)) => (
            0,
            Cut::Uncompiled(Uncompiled {
                index: fault.index,
                message: fault.message,
            }),
        ),
        (Patterns::Judged { .. }, None) => (
            reach.judged,
            Cut::WorkLimit(Work {
                measured: reach.work,
                bound: MAX_REPORT_PATTERN_WORK,
                next: reach.next.map(str::to_owned),
                too_costly: reach.too_costly,
            }),
        ),
    };
    let unjudged = reach.wanted.saturating_sub(judged);
    PatternCounts {
        judged,
        unjudged,
        cut: (unjudged > 0).then_some(cut),
    }
}

/// The [`Cut`] of `counts`, if one left decisions unjudged.
const fn cut(counts: &PatternCounts) -> Option<&Cut> {
    counts.cut.as_ref()
}

/// The work limit's cut of `counts`, if it cut.
const fn work(counts: &PatternCounts) -> Option<&Work> {
    match cut(counts) {
        Some(Cut::WorkLimit(work)) => Some(work),
        Some(Cut::NotAsked | Cut::Uncompiled(_)) | None => None,
    }
}

impl Output for PatternCounts {
    fn declare(fields: &mut Shown<'_, Self>) {
        let judged = "Decisions the patterns read, by name.";
        fields.required("judged", judged, &Count, |p| &p.judged);
        let unjudged = "Decisions they did not read.";
        fields.required("unjudged", unjudged, &Count, |p| &p.unjudged);
        let kind = Tag {
            all: &CutKind::ALL,
            name: CutKind::as_str,
        };
        let why = "Why some went unjudged: `not_asked`, the call judged the limits alone; \
                   `work_limit`, the patterns' work reached the bound; `uncompiled`, a \
                   pattern does not compile.";
        fields.optional("why", why, &kind, |p| cut(p).map(Cut::kind));
        let about = "The patterns' work over every decision and the bound, in state-bytes: \
                     the bytes each pattern reads times its NFA's states.";
        fields.optional("work", about, &Nested::<Work>::new(), work);
        let next = "The first decision the bound left unread: `from` set to it reads on.";
        fields.optional("next", next, &Str, |p| work(p)?.next.as_deref());
        let costly = "Decisions whose own work passes the bound, which no call reads.";
        fields.optional("too_costly", costly, &Count, |p| {
            work(p)
                .map(|work| &work.too_costly)
                .filter(|&&count| count > 0)
        });
        let uncompiled = "The pattern that does not compile; `trurlic check` reports it.";
        let kind = Nested::<Uncompiled>::new();
        fields.optional("uncompiled", uncompiled, &kind, |p| match cut(p) {
            Some(Cut::Uncompiled(uncompiled)) => Some(uncompiled),
            Some(Cut::NotAsked | Cut::WorkLimit(_)) | None => None,
        });
        let more = "How the decisions left unjudged come to be judged.";
        fields.optional("more", more, &Str, |p| cut(p).map(Cut::more));
    }
}

impl Output for Work {
    fn declare(fields: &mut Shown<'_, Self>) {
        let measured = "The patterns' work over every decision.";
        fields.required("measured", measured, &Count, |w| &w.measured);
        fields.required("bound", "The most one call does.", &Count, |w| &w.bound);
    }
}

impl Output for Uncompiled {
    fn declare(fields: &mut Shown<'_, Self>) {
        let index = "Its index in `[quality] patterns`.";
        fields.required("index", index, &Count, |u| &u.index);
        fields.required("message", "Why it does not compile.", &Str, |u| &u.message);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::Value;
    use tempfile::TempDir;

    use super::*;
    use crate::mcp::result::refusal_text;
    use crate::mcp::spec::to_value;
    use crate::mcp::status::{StatusArgs, status};
    use crate::store::Store;
    use crate::store::corpus::Corpus;
    use crate::store::limits::MAX_QUALITY_PATTERNS;

    /// A call that judged the patterns says how far they read and why no
    /// further; one that did not judged none; a whole reading says nothing
    /// more.
    #[test]
    fn the_counts_say_how_far_the_patterns_read_and_why_no_further() {
        let reach = |judged| Reach {
            judged,
            wanted: 5,
            work: 99,
            next: (judged < 5).then_some("d3"),
            too_costly: 1,
        };
        let asked = Patterns::Judged { from: None };
        let shown = |counts: PatternCounts| {
            let value = to_value(&counts);
            (counts.judged, counts.unjudged, value)
        };

        let (judged, unjudged, value) = shown(pattern_counts(asked, None, &reach(3)));
        assert_eq!((judged, unjudged), (3, 2));
        assert_eq!(value["why"], "work_limit");
        assert_eq!(value["work"]["measured"], 99);
        assert_eq!(value["work"]["bound"], MAX_REPORT_PATTERN_WORK);
        assert_eq!(
            (&value["next"], &value["too_costly"]),
            (&"d3".into(), &1.into())
        );
        let whole = shown(pattern_counts(asked, None, &reach(5)));
        assert_eq!(whole.2, serde_json::json!({ "judged": 5, "unjudged": 0 }));
        let none = shown(pattern_counts(Patterns::NotAsked, None, &reach(5)));
        assert_eq!(
            (none.0, none.1, &none.2["why"]),
            (0, 5, &"not_asked".into())
        );
        let fault = PatternFault {
            index: 2,
            message: "bad".into(),
        };
        let broken = shown(pattern_counts(asked, Some(fault), &reach(5)));
        assert_eq!((broken.0, broken.1), (0, 5));
        assert_eq!(broken.2["uncompiled"]["index"], 2);
    }

    /// `from` pages the patterns, which only the lint section judges.
    #[test]
    fn from_is_refused_beside_any_section_but_lint() {
        let judged = Patterns::of(Some(Section::Lint), Some("d1")).unwrap();
        assert_eq!(judged, Patterns::Judged { from: Some("d1") });
        assert!(judged.reads("d1") && judged.reads("d2") && !judged.reads("d0"));
        assert_eq!(Patterns::of(None, None).unwrap(), Patterns::NotAsked);
        for section in [None, Some(Section::Issues), Some(Section::Components)] {
            let refused = Patterns::of(section, Some("d1")).unwrap_err();
            let text: Value = serde_json::from_str(&refusal_text(&refused)).unwrap();
            assert_eq!(text["error"]["field"], "from", "{text}");
        }
    }

    /// `[quality]` at its bounds: every limit set, and the most patterns,
    /// each over every field: the first, `^` over `choice`, matches every
    /// decision, and each other is the slowest a pattern can be, a counted
    /// repetition at the compile limit; the pattern at `broken`, if any,
    /// one that does not compile.
    fn worst_table(broken: Option<usize>) -> String {
        let mut table = String::from(
            "[quality]\nreason_words = { min = 1, max = 100000 }\nalternatives_max = 100\n\
             alternative_words_max = 1000\nanchors_max = 100\n",
        );
        for index in 0..MAX_QUALITY_PATTERNS {
            let pattern = if broken == Some(index) {
                "("
            } else if index == 0 {
                "^"
            } else {
                r"[\x00-\x7f]{0,9343}q"
            };
            table.push_str(&format!(
                "\n[[quality.patterns]]\npattern = '{pattern}'\n\
                 fields = [\"alternatives\", \"anchors\", \"choice\", \"origin\", \"reason\", \
                 \"tags\"]\nlevel = \"warn\"\nmessage = \"M\"\n"
            ));
        }
        table
    }

    /// What `status` returns for `store` with no argument.
    fn default_call(store: &Store) -> Value {
        let state = store.load_state().unwrap();
        let arguments = StatusArgs {
            section: None,
            origin: None,
            from: None,
        };
        to_value(&status(store, &state, arguments).unwrap())
    }

    /// The counts of `answer` but those of `[quality]` and `project.toml`:
    /// how far the patterns read, and whether the hand-written table is
    /// in canonical form.
    fn graph_counts(answer: &Value) -> Value {
        let mut counts = answer["counts"].clone();
        counts["lint"]["patterns"] = Value::Null;
        counts["non_canonical"] = Value::Null;
        counts
    }

    /// No legal `[quality]` makes the default call slower. The table is at
    /// its bounds over the seeded corpus: the patterns' work passes the
    /// lint section's limit many times over, and one pattern matches every
    /// decision. The default call lists no pattern finding and counts what
    /// the limits alone give. A pattern that does not compile changes
    /// nothing in its answer, so it compiles no pattern and runs none.
    #[test]
    fn no_legal_table_costs_the_default_call_a_pattern() {
        let tmp = TempDir::new().unwrap();
        let store = Corpus::new(600).write(tmp.path()).unwrap();
        let path = store.root().join("project.toml");
        let header = fs::read_to_string(&path).unwrap();
        let with = |table: &str| fs::write(&path, format!("{header}\n{table}")).unwrap();

        with(&worst_table(None));
        let state = store.load_state().unwrap();
        let (bar, uncompiled) = Bar::for_report(&state.project.quality);
        assert!(uncompiled.is_none());
        let work: usize = state
            .decisions
            .values()
            .map(|decision| bar.pattern_work(decision))
            .sum();
        assert!(work > 100 * MAX_REPORT_PATTERN_WORK, "{work}");
        let worst = default_call(&store);
        assert_eq!(worst["counts"]["lint"]["patterns"]["why"], "not_asked");
        let items = worst["lint"]["items"].as_array().unwrap();
        assert!(!items.is_empty());
        assert!(
            items.iter().all(|item| item["rule"] != "pattern"),
            "{worst}"
        );

        with(&worst_table(Some(MAX_QUALITY_PATTERNS - 1)));
        assert_eq!(default_call(&store), worst);
        let limits = worst_table(None);
        let limits = limits.split("\n[[quality.patterns]]").next().unwrap();
        with(limits);
        assert_eq!(graph_counts(&default_call(&store)), graph_counts(&worst));
    }
}
