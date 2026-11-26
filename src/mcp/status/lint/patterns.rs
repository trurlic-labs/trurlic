//! How far a `status` call judges the `[quality]` patterns: the default
//! call judges the limits alone, and the `lint` section asked for alone
//! judges the patterns too, over the decisions by name while the bytes
//! they read stay within [`MAX_REPORT_PATTERN_BYTES`]. The counts say how
//! many decisions the patterns read, why they read no more, and the
//! decision where a call with `from` reads on.

use crate::lint::quality::{self, Found, Reach, Stop};
use crate::mcp::spec::output::{Count, Nested, Str, Tag};
use crate::mcp::spec::{Output, Shown};
use crate::store::limits::MAX_REPORT_PATTERN_BYTES;
use crate::store::{Bar, Decision, ProjectState, Shortfall};

use crate::mcp::result::ToolError;
use crate::mcp::status::Section;

/// How far a page read the decisions, the `[quality]` patterns' or the
/// likely duplicates' probes: why it read no more, and where the next
/// page starts.
pub(crate) struct PatternCounts {
    pub(crate) judged: usize,
    pub(crate) unjudged: usize,
    /// The first decision whose quality findings this page did not list
    /// wholly: a call with `from` set to it reads on.
    pub(crate) next: Option<String>,
    /// Why some decisions went unjudged, when some did.
    pub(crate) cut: Option<Cut>,
}

/// Why the `[quality]` patterns left decisions unjudged.
pub(crate) enum Cut {
    /// The call judged the limits alone: only `section: "lint"` judges the
    /// patterns.
    NotAsked,
    /// The decisions from `next` on, by name, would take the bytes the
    /// patterns read past the bound.
    WorkLimit(Work),
    /// A pattern's cache, or the caches together, filled on `next`, over
    /// the decisions before it on the page.
    GaveUp,
    /// The result's room ended the listing at `next`.
    Page,
    /// A pattern does not compile, so none is judged.
    Uncompiled(Uncompiled),
    /// The decisions' texts alone take the pairs' work past the bound, so
    /// no pair is compared.
    Unbuilt(Work),
}

/// The work a pass would do over every decision wanted from the page's
/// first on, the most one call does, and what both count.
#[derive(Debug)]
pub(crate) struct Work {
    pub(crate) measured: usize,
    pub(crate) bound: usize,
    /// `bytes` the patterns read, or `units` of the pairs' work.
    pub(crate) unit: &'static str,
}

/// A `[quality]` pattern that does not compile: no pattern is judged, the
/// limits are, and `trurlic check` reports it.
#[derive(Clone)]
pub(crate) struct Uncompiled {
    pub(crate) index: usize,
    pub(crate) message: String,
}

/// How a caller reads the rest of the section when a page ends.
pub(super) const PAGE_MORE: &str = "the page lists the findings of the decisions before `next`, \
     a pair on the page of the later-named of its decisions the pass probed: call again with \
     `section: \"lint\"` and `from` set to `next` until no `next` is left";

/// The kind of a [`Cut`], one per variant.
#[derive(Debug, Clone, Copy)]
enum CutKind {
    NotAsked,
    WorkLimit,
    GaveUp,
    Page,
    Uncompiled,
    Unbuilt,
}

impl CutKind {
    const ALL: [Self; 6] = [
        Self::NotAsked,
        Self::WorkLimit,
        Self::GaveUp,
        Self::Page,
        Self::Uncompiled,
        Self::Unbuilt,
    ];

    const fn as_str(self) -> &'static str {
        match self {
            Self::NotAsked => "not_asked",
            Self::WorkLimit => "work_limit",
            Self::GaveUp => "gave_up",
            Self::Page => "page",
            Self::Uncompiled => "uncompiled",
            Self::Unbuilt => "unbuilt",
        }
    }
}

impl Cut {
    const fn kind(&self) -> &'static CutKind {
        match self {
            Self::NotAsked => &CutKind::NotAsked,
            Self::WorkLimit(_) => &CutKind::WorkLimit,
            Self::GaveUp => &CutKind::GaveUp,
            Self::Page => &CutKind::Page,
            Self::Uncompiled(_) => &CutKind::Uncompiled,
            Self::Unbuilt(_) => &CutKind::Unbuilt,
        }
    }

    /// How the decisions left unjudged come to be judged.
    const fn more(&self) -> &'static str {
        match self {
            Self::NotAsked => {
                "the call judges the limits alone; `section: \"lint\"` judges the patterns \
                 too, within a bound on the bytes they read"
            }
            Self::WorkLimit(_) | Self::GaveUp | Self::Page => PAGE_MORE,
            Self::Uncompiled(_) => "no pattern is judged while one does not compile",
            Self::Unbuilt(_) => {
                "no call compares the pairs while the decisions' texts alone pass the bound"
            }
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
            (
                Some(Section::Issues | Section::History | Section::Components | Section::Anchors)
                | None,
                None,
            ) => Ok(Self::NotAsked),
            (
                Some(Section::Issues | Section::History | Section::Components | Section::Anchors)
                | None,
                Some(_),
            ) => Err(ToolError::invalid(
                "from",
                "`from` reads the `[quality]` patterns on from a decision, which only \
                     `section: \"lint\"` judges: give it with that section",
            )),
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

    /// Whether the call reads a page of the decisions, from `from` on.
    pub(super) const fn pages(self) -> bool {
        match self {
            Self::Judged { .. } => true,
            Self::NotAsked => false,
        }
    }

    /// The decision a page starts at, when the call reads one from it.
    pub(super) const fn from(self) -> Option<&'a str> {
        match self {
            Self::Judged { from } => from,
            Self::NotAsked => None,
        }
    }
}

/// The quality findings of the decisions `wanted` admits by name, the
/// first `limit` of them, and, when `[quality]` has patterns, how far the
/// patterns read: as far as their bound lets when `patterns` is `Judged`,
/// else not at all, with nothing compiled.
pub(super) fn shortfalls<'s>(
    state: &'s ProjectState,
    patterns: Patterns<'_>,
    limit: usize,
    wanted: impl Fn(&str, &Decision) -> bool,
) -> (Found<(&'s str, Shortfall)>, Option<Reading<'s>>) {
    let table = &state.project.quality;
    let (bar, uncompiled) = match patterns {
        Patterns::Judged { .. } => Bar::for_report(table),
        Patterns::NotAsked => (Bar::limits_only(table), None),
    };
    let found = quality::shortfalls(state, &bar, limit, MAX_REPORT_PATTERN_BYTES, wanted);
    let verdict = match (patterns, uncompiled) {
        (Patterns::NotAsked, _) => Verdict::NotAsked,
        (Patterns::Judged { .. }, Some(fault)) => Verdict::Uncompiled(Uncompiled {
            index: fault.index,
            message: fault.message,
        }),
        (Patterns::Judged { .. }, None) => Verdict::Read,
    };
    let reading = (!table.patterns.is_empty()).then_some(Reading {
        verdict,
        beyond: found.beyond,
        reach: found.reach,
    });
    (found.found, reading)
}

/// How far a call's patterns read, before the result's room is known.
pub(in crate::mcp::status) struct Reading<'s> {
    verdict: Verdict,
    /// The decision of the first finding past those the pass kept.
    beyond: Option<&'s str>,
    reach: Reach<'s>,
}

/// Whether the patterns read the decisions of a call.
enum Verdict {
    /// The call judged the limits alone.
    NotAsked,
    /// A pattern does not compile, so none read.
    Uncompiled(Uncompiled),
    /// They read as far as [`Reach`] says.
    Read,
}

impl<'s> Reading<'s> {
    /// The first decision wanted after `name`, by name.
    pub(super) fn after(&self, name: &str) -> Option<&'s str> {
        let wanted = &self.reach.wanted;
        let at = wanted.partition_point(|&wanted| wanted <= name);
        wanted.get(at).copied()
    }

    /// The decision of the first finding past those the pass kept, when
    /// the patterns read a page: where its listing ends at the most.
    pub(super) fn page_end(&self) -> Option<&'s str> {
        match self.verdict {
            Verdict::Read => self.beyond,
            Verdict::NotAsked | Verdict::Uncompiled(_) => None,
        }
    }

    /// What the counts say when the listing of quality findings ended
    /// before `listed_to`, the first decision not listed wholly. The page
    /// ends there, or where the patterns stopped, whichever is first.
    pub(super) fn counts(&self, listed_to: Option<&str>) -> PatternCounts {
        let reach = &self.reach;
        let stop = match (listed_to, reach.next) {
            (Some(listed), Some(next)) if next.decision <= listed => {
                Some((next.decision, next.why))
            }
            (Some(listed), Some(_) | None) => Some((listed, Stop::Listing)),
            (None, Some(next)) => Some((next.decision, next.why)),
            (None, None) => None,
        };
        let cut = match (&self.verdict, stop) {
            (Verdict::NotAsked, _) => Some(Cut::NotAsked),
            // clone: the counts the frame measures and the final ones each
            // own the message.
            (Verdict::Uncompiled(fault), _) => Some(Cut::Uncompiled(fault.clone())),
            (Verdict::Read, Some((_, Stop::Bytes))) => Some(Cut::WorkLimit(Work {
                measured: reach.bytes,
                bound: MAX_REPORT_PATTERN_BYTES,
                unit: "bytes",
            })),
            (Verdict::Read, Some((_, Stop::GaveUp))) => Some(Cut::GaveUp),
            (Verdict::Read, Some((_, Stop::Listing))) => Some(Cut::Page),
            (Verdict::Read, None) => None,
        };
        let (judged, next) = match (&self.verdict, stop) {
            (Verdict::NotAsked | Verdict::Uncompiled(_), _) => (0, None),
            (Verdict::Read, Some((name, _))) => (
                reach.read.partition_point(|&read| read < name),
                Some(name.to_owned()),
            ),
            (Verdict::Read, None) => (reach.read.len(), None),
        };
        PatternCounts {
            judged,
            unjudged: reach.wanted.len().saturating_sub(judged),
            next,
            cut,
        }
    }
}

/// The [`Cut`] of `counts`, if one left decisions unjudged.
const fn cut(counts: &PatternCounts) -> Option<&Cut> {
    counts.cut.as_ref()
}

/// The work bound's cut of `counts`, if it cut.
const fn work(counts: &PatternCounts) -> Option<&Work> {
    match cut(counts) {
        Some(Cut::WorkLimit(work) | Cut::Unbuilt(work)) => Some(work),
        Some(Cut::NotAsked | Cut::GaveUp | Cut::Page | Cut::Uncompiled(_)) | None => None,
    }
}

impl Output for PatternCounts {
    fn declare(fields: &mut Shown<'_, Self>) {
        let judged = "Decisions this page read, by name: the patterns', or the pairs' probes.";
        fields.required("judged", judged, &Count, |p| &p.judged);
        let unjudged = "Decisions it did not read.";
        fields.required("unjudged", unjudged, &Count, |p| &p.unjudged);
        let kind = Tag {
            all: &CutKind::ALL,
            name: CutKind::as_str,
        };
        let why = "Why some went unread: `not_asked`, the call judged the limits alone; \
                   `work_limit`, the work reached the bound; `gave_up`, a pattern's cache, or \
                   the caches together, filled over the page before `next`; `page`, the \
                   result's room, or another kind's page, ended the listing; `uncompiled`, a \
                   pattern does not compile; `unbuilt`, the decisions' texts alone pass the \
                   pairs' bound.";
        fields.optional("why", why, &kind, |p| cut(p).map(Cut::kind));
        let about = "The work over every decision from the page's first on and the bound: \
                     for the patterns the bytes of each text each reads, one more per text; \
                     for the pairs the units their pass charges.";
        fields.optional("work", about, &Nested::<Work>::new(), work);
        let next = "The first decision whose findings this page did not list wholly: \
                    `from` set to it reads on.";
        fields.optional("next", next, &Str, |p| p.next.as_deref());
        let uncompiled = "The pattern that does not compile; `trurlic check` reports it.";
        let kind = Nested::<Uncompiled>::new();
        fields.optional("uncompiled", uncompiled, &kind, |p| match cut(p) {
            Some(Cut::Uncompiled(uncompiled)) => Some(uncompiled),
            Some(Cut::NotAsked | Cut::WorkLimit(_) | Cut::GaveUp | Cut::Page | Cut::Unbuilt(_))
            | None => None,
        });
        let more = "How the decisions left unjudged come to be judged.";
        fields.optional("more", more, &Str, |p| cut(p).map(Cut::more));
    }
}

impl Output for Work {
    fn declare(fields: &mut Shown<'_, Self>) {
        let measured = "The work over every decision from the page's first on.";
        fields.required("measured", measured, &Count, |w| &w.measured);
        fields.required("bound", "The most one call does.", &Count, |w| &w.bound);
        let unit = "What both count: `bytes`, or `units`.";
        fields.required("unit", unit, &Str, |w| w.unit);
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
    use crate::lint::quality::Next;
    use crate::mcp::result::refusal_text;
    use crate::mcp::spec::to_value;
    use crate::mcp::status::{StatusArgs, status};
    use crate::store::Store;
    use crate::store::corpus::Corpus;
    use crate::store::limits::{MAX_QUALITY_PATTERNS, MAX_TEXT_FIELD_BYTES};
    use crate::store::testing::OUTGROWS_ITS_CACHE;

    /// The patterns' reading of `d0` to `d5`, stopped for `why` before
    /// `next`.
    fn reading(verdict: Verdict, next: Option<&'static str>, why: Stop) -> Reading<'static> {
        let wanted = vec!["d0", "d1", "d2", "d3", "d4", "d5"];
        let stop = next.map_or(wanted.len(), |next| {
            wanted.partition_point(|&name| name < next)
        });
        Reading {
            verdict,
            beyond: None,
            reach: Reach {
                read: wanted.get(..stop).unwrap_or_default().to_vec(),
                wanted,
                bytes: 99,
                next: next.map(|decision| Next { decision, why }),
            },
        }
    }

    /// A call that judged the patterns says how far they read and why no
    /// further; one that did not judged none.
    #[test]
    fn the_counts_say_how_far_the_patterns_read_and_why_no_further() {
        let counts = reading(Verdict::Read, Some("d3"), Stop::Bytes).counts(None);
        let value = to_value(&counts);
        assert_eq!((counts.judged, counts.unjudged), (3, 3));
        assert_eq!(value["why"], "work_limit");
        assert_eq!(value["work"]["measured"], 99);
        assert_eq!(value["work"]["bound"], MAX_REPORT_PATTERN_BYTES);
        assert_eq!(value["next"], "d3");
        let gave_up = reading(Verdict::Read, Some("d3"), Stop::GaveUp).counts(None);
        let value = to_value(&gave_up);
        assert_eq!(
            (&value["why"], &value["next"]),
            (&"gave_up".into(), &"d3".into())
        );
        assert!(value.get("work").is_none(), "{value}");

        let none = reading(Verdict::NotAsked, None, Stop::Bytes).counts(None);
        assert_eq!((none.judged, none.unjudged), (0, 6));
        assert_eq!(to_value(&none)["why"], "not_asked");
        let fault = Uncompiled {
            index: 2,
            message: "bad".into(),
        };
        let broken = reading(Verdict::Uncompiled(fault), None, Stop::Bytes).counts(None);
        assert_eq!(to_value(&broken)["uncompiled"]["index"], 2);
    }

    /// A page the result's room ended before the bytes bound names the
    /// first decision it did not list wholly, and counts as judged only the
    /// decisions before it; the bound's `next` wins when it is first.
    #[test]
    fn a_page_ends_at_the_first_decision_it_does_not_list_wholly() {
        let read = reading(Verdict::Read, Some("d3"), Stop::Bytes);

        let page = read.counts(Some("d1"));
        let work = read.counts(Some("d4"));

        assert_eq!((page.judged, page.next.as_deref()), (1, Some("d1")));
        assert_eq!(to_value(&page)["why"], "page");
        assert_eq!((work.judged, work.next.as_deref()), (3, Some("d3")));
        assert_eq!(to_value(&work)["why"], "work_limit");
    }

    /// The decisions of a page's quality findings, fitted by keeping the
    /// first `room`: the decisions listed, what was omitted, and the counts.
    fn page_of(decisions: Vec<&'static str>, room: usize) -> (Vec<String>, usize, PatternCounts) {
        use crate::mcp::status::lint::{
            LintCounts, LintItem, LintItems, LintKind, Page, QualityItem, fitted,
        };
        use crate::mcp::status::{Listed, Part};
        use crate::store::Level;

        let item = |decision: &str| {
            LintItem::Quality(QualityItem {
                decision: decision.into(),
                field: "choice".into(),
                rule: "pattern",
                level: Level::Warn,
                limit: None,
                pattern: None,
            })
        };
        let total = decisions.len();
        let items = LintItems {
            kinds: vec![(
                LintKind::Quality,
                Listed::of(total, decisions.iter().map(|&decision| item(decision))),
            )],
            page: Some(Page {
                reading: reading(Verdict::Read, None, Stop::Bytes),
                decisions,
            }),
            pairs: None,
            paged: true,
        };
        let mut counts = LintCounts {
            duplicates: 0,
            pairs: None,
            quality: total,
            file_anchors_only: 0,
            patterns: None,
        };
        let fit = |_, listed: Listed<LintItem>| {
            let mut items = listed.items;
            items.truncate(room);
            Part {
                total: listed.total,
                omitted: listed.total - items.len(),
                items,
                more: Some("more"),
            }
        };
        let part = fitted(items, fit, Some(&mut counts));
        let listed = part.items.iter().map(|item| match item {
            LintItem::Quality(found) => found.decision.clone(),
            LintItem::Duplicate { .. } | LintItem::FileAnchorsOnly { .. } => String::new(),
        });
        (listed.collect(), part.omitted, counts.patterns.unwrap())
    }

    /// A page the budget cut inside a decision's findings ends before that
    /// decision: its listed findings go, and `next` names it.
    #[test]
    fn a_page_cut_inside_a_decision_ends_before_it() {
        let (listed, omitted, patterns) = page_of(vec!["d0", "d1", "d1", "d2"], 2);

        assert_eq!((listed, omitted), (vec!["d0".to_owned()], 3));
        assert_eq!((patterns.next.as_deref(), patterns.judged), (Some("d1"), 1));
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
    /// decision, and each other fills its cache; the pattern at `broken`,
    /// if any, one that does not compile.
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
                OUTGROWS_ITS_CACHE
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

    /// The lint section reads the decisions while the bytes its patterns
    /// read stay within `MAX_REPORT_PATTERN_BYTES`: the most patterns over
    /// reasons of the longest text, one more decision than the bound holds,
    /// end the page at the last.
    #[test]
    fn the_lint_section_stops_at_its_bytes_bound() {
        use crate::store::testing::{empty_state, sample_decision};
        use crate::store::{Anchor, DecisionField, Level, Pattern, Quality};

        let reason = "x".repeat(MAX_TEXT_FIELD_BYTES);
        let per = MAX_QUALITY_PATTERNS * (reason.len() + 1)
            + MAX_QUALITY_PATTERNS * ("Choice of the sample".len() + 1);
        let count = MAX_REPORT_PATTERN_BYTES / per + 1;
        let mut state = empty_state();
        let pattern = Pattern {
            pattern: "zz".into(),
            fields: [DecisionField::Choice, DecisionField::Reason].into(),
            level: Level::Warn,
            message: "M".into(),
        };
        state.project.quality = Quality {
            patterns: vec![pattern; MAX_QUALITY_PATTERNS],
            ..Quality::default()
        };
        for index in 0..count {
            let mut decision = sample_decision("project");
            // clone: each decision owns its reason.
            decision.reason = reason.clone();
            decision.alternatives = vec!["None".into()];
            decision.anchors = [Anchor::new("src/a.rs".into(), Some("f".into())).unwrap()].into();
            state.decisions.insert(format!("d{index:05}"), decision);
        }
        let patterns = Patterns::Judged { from: None };

        let (_, reading) = shortfalls(&state, patterns, usize::MAX, |_, _| true);

        let counts = reading.unwrap().counts(None);
        assert_eq!(counts.judged, count - 1);
        assert_eq!(to_value(&counts)["why"], "work_limit");
    }

    /// No legal `[quality]` makes the default call slower. The table is at
    /// its bounds over the seeded corpus, and one pattern matches every
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
        let (_, uncompiled) = Bar::for_report(&state.project.quality);
        assert!(uncompiled.is_none());
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
