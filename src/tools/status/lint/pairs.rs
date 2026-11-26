//! How far a `status` call compares the likely duplicate pairs: only the
//! `lint` section asked for alone, from its first decision, runs the pass,
//! within [`MAX_DUPLICATE_WORK`]. The counts say how many decisions it
//! compared, why no more, and how the rest come to be compared.

use crate::lint::duplicates::{Among, Similarity};
use crate::store::ProjectState;
use crate::store::limits::MAX_DUPLICATE_WORK;
use crate::tools::spec::output::{Count, Nested, Str, Tag};
use crate::tools::spec::{Output, Shown};

use super::{LintItem, LintKind, Patterns, Work, is_under, lint_limit};

/// The decisions the pairs' pass compared and those it did not.
pub(crate) struct PairCounts {
    pub(crate) compared: usize,
    pub(crate) uncompared: usize,
    /// The work of the whole pass and the bound it passed; `None` when
    /// the call compares no pair.
    pub(crate) work: Option<Work>,
}

/// Why the pairs' pass left decisions uncompared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Uncompared {
    /// The call is not `section: "lint"` without `from`.
    NotAsked,
    /// The next decision's probe would take the work past the bound.
    WorkLimit,
}

impl PairCounts {
    const fn why(&self) -> &'static Uncompared {
        match self.work {
            None => &Uncompared::NotAsked,
            Some(_) => &Uncompared::WorkLimit,
        }
    }
}

impl Uncompared {
    const ALL: [Self; 2] = [Self::NotAsked, Self::WorkLimit];

    const fn as_str(self) -> &'static str {
        match self {
            Self::NotAsked => "not_asked",
            Self::WorkLimit => "work_limit",
        }
    }

    /// How the decisions left uncompared come to be compared.
    const fn more(self) -> &'static str {
        match self {
            Self::NotAsked => {
                "`section: \"lint\"` without `from` compares the pairs, within a bound on the \
                 work"
            }
            Self::WorkLimit => {
                "the pass compares the decisions by name while its work stays within `bound`; \
                 `origin` compares only the pairs of the decisions recorded under it, for less \
                 work"
            }
        }
    }
}

/// The likely duplicate pairs of a call: how many, the best, and how far
/// the pass compared the decisions when not every one.
pub(super) struct Duplicates {
    /// `None` when the call compares no pair.
    pub(super) total: Option<usize>,
    pub(super) counts: Option<PairCounts>,
    /// The best, as many as one result can show.
    pub(super) items: Vec<LintItem>,
}

/// The pairs a call for `patterns` compares: within [`MAX_DUPLICATE_WORK`]
/// for the lint section from its first decision, else none.
pub(super) fn compared(
    state: &ProjectState,
    origin: Option<&str>,
    patterns: Patterns<'_>,
) -> Duplicates {
    match patterns {
        Patterns::Judged { from: None } => duplicates(state, origin, MAX_DUPLICATE_WORK),
        // A page read on from a decision lists its quality findings alone.
        Patterns::Judged { from: Some(_) } | Patterns::NotAsked => unasked(state, origin),
    }
}

/// The decisions of `state` recorded under `origin`, every one when `None`.
fn named<'s>(state: &'s ProjectState, origin: Option<&str>) -> Vec<&'s str> {
    let under = state
        .decisions
        .iter()
        .filter(|(_, decision)| is_under(origin, decision));
    under.map(|(name, _)| name.as_str()).collect()
}

/// The pairs of `state`, those touching a decision recorded under
/// `origin` when one is given, from the decisions the pass compares within
/// `bound`.
fn duplicates(state: &ProjectState, origin: Option<&str>, bound: usize) -> Duplicates {
    let named = named(state, origin);
    let among = match origin {
        Some(_) => Among::Touching(&named),
        None => Among::Every,
    };
    let limit = lint_limit(LintKind::Duplicate);
    let ranked = Similarity::of(&state.decisions).pairs(among, limit, bound);
    let counts = (ranked.uncompared > 0).then_some(PairCounts {
        compared: ranked.compared,
        uncompared: ranked.uncompared,
        work: Some(Work {
            measured: ranked.work,
            bound,
        }),
    });
    let items = ranked.best.into_iter().map(|pair| LintItem::Duplicate {
        decision: pair.first.to_owned(),
        other: pair.second.to_owned(),
        score: pair.score.to_string(),
    });
    Duplicates {
        total: Some(ranked.total),
        counts,
        items: items.collect(),
    }
}

/// No pair compared: every decision of `state` under `origin` left.
fn unasked(state: &ProjectState, origin: Option<&str>) -> Duplicates {
    let counts = PairCounts {
        compared: 0,
        uncompared: named(state, origin).len(),
        work: None,
    };
    Duplicates {
        total: None,
        counts: Some(counts),
        items: Vec::new(),
    }
}

impl Output for PairCounts {
    fn declare(fields: &mut Shown<'_, Self>) {
        let compared = "Decisions the pass compared, the first by name: each with those before \
                        it, or under `origin` each recorded there with every other decision.";
        fields.required("compared", compared, &Count, |p| &p.compared);
        let uncompared = "Decisions it did not compare.";
        fields.required("uncompared", uncompared, &Count, |p| &p.uncompared);
        let kind = Tag {
            all: &Uncompared::ALL,
            name: Uncompared::as_str,
        };
        let why = "Why: `not_asked`, only `section: \"lint\"` without `from` compares the pairs; \
                   `work_limit`, the next decision's probe would take the work past the bound.";
        fields.required("why", why, &kind, |p| p.why());
        let work = "Past the bound: the work of the whole pass and the bound, in units, the \
                    posting entries the probes read and 16 for each pair they may test.";
        fields.optional("work", work, &Nested::<Work>::new(), |p| p.work.as_ref());
        let more = "How the decisions left uncompared come to be compared.";
        fields.required("more", more, &Str, |p| p.why().more());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::{empty_state, sample_decision};
    use crate::tools::spec::to_value;

    /// Three decisions of one text, three pairs at a score of one.
    fn copies() -> ProjectState {
        let mut state = empty_state();
        for name in ["a", "b", "c"] {
            let mut decision = sample_decision("project");
            decision.choice = "Retry with backoff".into();
            state.decisions.insert(name.into(), decision);
        }
        state
    }

    /// The default call and a page read on from a decision compare no pair
    /// and say so with every decision uncompared; the lint section from the
    /// first decision compares them all.
    #[test]
    fn only_the_lint_section_without_from_compares_the_pairs() {
        let state = copies();
        for patterns in [Patterns::NotAsked, Patterns::Judged { from: Some("b") }] {
            let unasked = compared(&state, None, patterns);
            let counts = to_value(&unasked.counts.unwrap());

            assert_eq!((unasked.total, unasked.items.len()), (None, 0));
            assert_eq!(
                counts,
                serde_json::json!({
                    "compared": 0, "uncompared": 3, "why": "not_asked",
                    "more": Uncompared::NotAsked.more(),
                })
            );
        }
        let asked = compared(&state, None, Patterns::Judged { from: None });
        let shown = (asked.total, asked.counts.is_none(), asked.items.len());
        assert_eq!(shown, (Some(3), true, 3));
    }

    /// Past the bound the pass compares the decisions it reached by name:
    /// the first, whose probe reads no entry, and none after it. The counts
    /// say how many it compared and did not, the work and the bound, and
    /// name `origin` as the way to narrow it.
    #[test]
    fn past_the_bound_the_counts_say_how_many_decisions_were_compared() {
        let cut = duplicates(&copies(), None, 0);
        let counts = cut.counts.unwrap();
        let value = to_value(&counts);

        assert_eq!((cut.total, cut.items.len()), (Some(0), 0));
        assert_eq!((counts.compared, counts.uncompared), (1, 2));
        assert_eq!(value["why"], "work_limit");
        assert!(value["work"]["measured"].as_u64().unwrap() > 0, "{value}");
        assert_eq!(value["work"]["bound"], 0);
        assert!(Uncompared::WorkLimit.more().contains("`origin`"));
        assert_eq!(value["more"], Uncompared::WorkLimit.more());
    }
}
