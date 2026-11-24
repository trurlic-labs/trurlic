//! Quality findings. Each decision of the graph is held whole to the bar
//! it would be written under, and every rule it breaks is a finding
//! (`Bar`, the store's one home of the rules). A decision whose anchors all
//! name a file and no symbol is a finding too: none of its anchors says
//! what part of its file the decision constrains. A write holds an op to
//! the bar only in the fields it changes, so a graph below its bar loads,
//! passes `check` and stays revisable; these findings are that graph made
//! visible, never an error.

use rayon::prelude::*;

use crate::store::{Bar, Decision, ProjectState, Shortfall};

/// The findings of one kind: how many there are, and the first of them, by
/// decision name, as many as asked for.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Found<T> {
    pub(crate) total: usize,
    pub(crate) first: Vec<T>,
}

impl<T> Found<T> {
    const fn new() -> Self {
        Self {
            total: 0,
            first: Vec::new(),
        }
    }

    fn add(&mut self, limit: usize, finding: T) {
        self.total = self.total.saturating_add(1);
        if self.first.len() < limit {
            self.first.push(finding);
        }
    }

    /// These findings, then `later`'s, which come after them in order.
    fn then(mut self, later: Self, limit: usize) -> Self {
        self.total = self.total.saturating_add(later.total);
        self.first.extend(later.first);
        self.first.truncate(limit);
        self
    }
}

/// The rules of the bar the decisions a report reads break, and how far
/// the bar's patterns read them.
pub(crate) struct Shortfalls<'s> {
    /// Each finding with its decision's name: by name, then the limits in
    /// the order the bar reads them, then the patterns in their order.
    pub(crate) found: Found<(&'s str, Shortfall)>,
    pub(crate) reach: Reach<'s>,
}

/// How far the bar's patterns read the decisions wanted, by name: each
/// while the work of those read stays within the bound, but one whose own
/// work passes it, which no call reads.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Reach<'s> {
    pub(crate) judged: usize,
    pub(crate) wanted: usize,
    /// The work of the patterns over every decision wanted.
    pub(crate) work: usize,
    /// The first decision the bound left unread, where a call that starts
    /// there reads on.
    pub(crate) next: Option<&'s str>,
    /// The decisions whose own work passes the bound.
    pub(crate) too_costly: usize,
}

/// Where a finding comes in its decision's: the limits in the order the
/// bar reads them, then each pattern's findings in its order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
    /// A limit's finding, by its order among the decision's.
    Limit(usize),
    /// A pattern's finding: the pattern's index, then the finding's order
    /// among that pattern's for the decision.
    Pattern { index: usize, order: usize },
}

/// A finding with its decision's position among those wanted.
type Placed = (usize, Rank, Shortfall);

/// Every limit of `bar` each decision `wanted` admits by name breaks, and
/// every match of its patterns in the decisions they read: those, by name,
/// while their work stays within `max_work` ([`Bar::pattern_work`]). The
/// count, and the first `limit` with their decisions' names.
///
/// The limits are judged on the thread pool over the decisions, each task
/// a run of them in order; the patterns one task per pattern, so a
/// compiled pattern runs on one thread at a time and keeps one cache
/// whatever the threads. Each task keeps its first `limit` findings and
/// they are merged in order, so the pass holds a bounded number of
/// findings, and nothing depends on the threads.
pub(crate) fn shortfalls<'s>(
    state: &'s ProjectState,
    bar: &Bar,
    limit: usize,
    max_work: usize,
    wanted: impl Fn(&str, &Decision) -> bool,
) -> Shortfalls<'s> {
    let decisions: Vec<(&'s str, &'s Decision)> = state
        .decisions
        .iter()
        .map(|(name, decision)| (name.as_str(), decision))
        .filter(|&(name, decision)| wanted(name, decision))
        .collect();
    let (reach, read) = reach(bar, &decisions, max_work);
    let read: Vec<(usize, &Decision)> = read
        .into_iter()
        .filter_map(|position| Some((position, decisions.get(position)?.1)))
        .collect();
    let found = merged(
        limit_findings(bar, &decisions, limit),
        pattern_findings(bar, &read, limit),
        limit,
    );
    let first = found
        .first
        .into_iter()
        .filter_map(|(position, _, shortfall)| {
            let &(name, _) = decisions.get(position)?;
            Some((name, shortfall))
        });
    Shortfalls {
        found: Found {
            total: found.total,
            first: first.collect(),
        },
        reach,
    }
}

/// The limits of `bar` each of `decisions` breaks, the first `limit`: on
/// the thread pool, each task a run of the decisions in order.
fn limit_findings(bar: &Bar, decisions: &[(&str, &Decision)], limit: usize) -> Found<Placed> {
    decisions
        .par_iter()
        .enumerate()
        .fold(Found::new, |mut found, (position, &(_, decision))| {
            let each = bar.audit_limits(decision).into_iter().enumerate();
            for (order, shortfall) in each {
                found.add(limit, (position, Rank::Limit(order), shortfall));
            }
            found
        })
        .reduce(Found::new, |earlier, later| earlier.then(later, limit))
}

/// The matches of each pattern of `bar` in `decisions`, each at its
/// position, the first `limit` of each: on the thread pool, one task per
/// pattern.
fn pattern_findings(
    bar: &Bar,
    decisions: &[(usize, &Decision)],
    limit: usize,
) -> Vec<Found<Placed>> {
    (0..bar.pattern_count())
        .into_par_iter()
        .map(|index| {
            let mut found = Found::new();
            for &(position, decision) in decisions {
                let each = bar.audit_pattern(index, decision).into_iter().enumerate();
                for (order, shortfall) in each {
                    found.add(limit, (position, Rank::Pattern { index, order }, shortfall));
                }
            }
            found
        })
        .collect()
}

/// How far the patterns of `bar` read `decisions`, in order, and the
/// positions of those they read: each while the work summed over those
/// read stays within `max_work`, passing over one whose own work passes
/// it. The first that would take the sum past it is where the reading
/// stops, so a call that starts there reads at least it.
fn reach<'s>(
    bar: &Bar,
    decisions: &[(&'s str, &Decision)],
    max_work: usize,
) -> (Reach<'s>, Vec<usize>) {
    let costs: Vec<usize> = decisions
        .iter()
        .map(|&(_, decision)| bar.pattern_work(decision))
        .collect();
    let (mut spent, mut read, mut next, mut too_costly) = (0usize, Vec::new(), None, 0);
    for (position, &cost) in costs.iter().enumerate() {
        if cost > max_work {
            too_costly += 1;
        } else if next.is_none() && spent.saturating_add(cost) <= max_work {
            spent += cost;
            read.push(position);
        } else if next.is_none() {
            next = decisions.get(position).map(|&(name, _)| name);
        }
    }
    let reach = Reach {
        judged: read.len(),
        wanted: decisions.len(),
        work: costs.into_iter().fold(0, usize::saturating_add),
        next,
        too_costly,
    };
    (reach, read)
}

/// The limits' findings and each pattern's, in order, the first `limit`
/// of them: each part holds its own first `limit`, so the merge does.
fn merged(limits: Found<Placed>, patterns: Vec<Found<Placed>>, limit: usize) -> Found<Placed> {
    let mut all = limits;
    for found in patterns {
        all.total = all.total.saturating_add(found.total);
        all.first.extend(found.first);
    }
    all.first
        .sort_unstable_by_key(|&(position, rank, _)| (position, rank));
    all.first.truncate(limit);
    all
}

/// Each decision `wanted` admits whose anchors, one at least, all name a
/// file and no symbol, by name: the count, and the first `limit`. One with
/// no anchor at all breaks the bar instead (`anchor_required`).
pub(crate) fn file_anchors_only(
    state: &ProjectState,
    limit: usize,
    wanted: impl Fn(&Decision) -> bool,
) -> Found<&str> {
    let mut found = Found::new();
    for (name, decision) in &state.decisions {
        let anchors = &decision.anchors;
        let files_only = !anchors.is_empty() && anchors.iter().all(|a| a.symbol().is_none());
        if files_only && wanted(decision) {
            found.add(limit, name.as_str());
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::{empty_state, sample_decision};
    use crate::store::{Anchor, DecisionField, Level, Pattern, Quality};

    /// Six hundred decisions, each lacking an alternative and an anchor
    /// but every tenth, which names a file alone and a reason with `zz`.
    fn graph() -> ProjectState {
        let mut state = empty_state();
        for index in 0..600 {
            let mut decision = sample_decision("project");
            if index % 10 == 0 {
                decision.alternatives = vec!["None".into()];
                decision.anchors = [Anchor::new(format!("src/{index}.rs"), None).unwrap()].into();
                decision.reason = "A reason with zz.".into();
            }
            state.decisions.insert(format!("d{index:03}"), decision);
        }
        state
    }

    /// A table whose one pattern, `zz` at `warn`, reads the reason.
    fn table() -> Quality {
        table_of("zz")
    }

    /// A table whose one pattern, `pattern` at `warn`, reads the reason.
    fn table_of(pattern: &str) -> Quality {
        Quality {
            patterns: vec![Pattern {
                pattern: pattern.into(),
                fields: [DecisionField::Reason].into(),
                level: Level::Warn,
                message: "M".into(),
            }],
            ..Quality::default()
        }
    }

    fn on<T: Send>(threads: usize, run: impl FnOnce() -> T + Send) -> T {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap();
        pool.install(run)
    }

    /// The pass counts every finding, keeps the first it is asked for by
    /// decision name, the limits before the patterns, and gives the same on
    /// one thread and on eight.
    #[test]
    fn the_findings_are_counted_whole_kept_first_and_alike_on_any_threads() {
        let state = graph();
        let (bar, _) = Bar::for_report(&table());
        let run = |threads: usize| {
            on(threads, || {
                let found = shortfalls(&state, &bar, 5, usize::MAX, |_, _| true);
                (found.found, found.reach)
            })
        };

        let (one, reach) = run(1);
        assert_eq!(one.total, 540 * 2 + 60);
        let first: Vec<(&str, &str)> = one
            .first
            .iter()
            .map(|(name, shortfall)| (*name, shortfall.fault.as_str()))
            .collect();
        assert_eq!(
            first,
            [
                ("d000", "pattern"),
                ("d001", "alternative_required"),
                ("d001", "anchor_required"),
                ("d002", "alternative_required"),
                ("d002", "anchor_required"),
            ]
        );
        assert_eq!((reach.judged, reach.wanted), (600, 600));
        assert_eq!(run(8), (one, reach));

        let files = file_anchors_only(&state, 2, |_| true);
        assert_eq!((files.total, files.first), (60, vec!["d000", "d010"]));
        let none = file_anchors_only(&state, 2, |decision| decision.origin.is_some());
        assert_eq!(none.total, 0);
    }

    /// The decisions from `from` on, by name.
    fn from(from: Option<&str>) -> impl Fn(&str, &Decision) -> bool + '_ {
        move |name, _| from.is_none_or(|from| name >= from)
    }

    /// The patterns read the decisions by name while their work stays
    /// within the limit, the first that would pass it named as `next`;
    /// the limits judge every decision.
    #[test]
    fn the_patterns_read_the_decisions_by_name_within_the_work_limit() {
        let state = graph();
        let (bar, _) = Bar::for_report(&table());
        let work: Vec<usize> = state
            .decisions
            .values()
            .map(|decision| bar.pattern_work(decision))
            .collect();
        let whole: usize = work.iter().sum();
        // Exactly the first ten decisions: d000, a match, then d001 to d009.
        let ten: usize = work[..10].iter().sum();

        for (max_work, judged, next) in [
            (ten, 10, "d010"),
            (ten + 1, 10, "d010"),
            (ten - 1, 9, "d009"),
        ] {
            let found = shortfalls(&state, &bar, usize::MAX, max_work, |_, _| true);
            let reach = &found.reach;
            let read = (reach.judged, reach.wanted, reach.work, reach.next);
            assert_eq!(read, (judged, 600, whole, Some(next)), "{max_work}");
            assert_eq!(found.found.total, 540 * 2 + 1, "{max_work}");
        }
        let all = shortfalls(&state, &bar, usize::MAX, whole, |_, _| true);
        assert_eq!((all.reach.judged, all.reach.next), (600, None));
    }

    /// A caller that starts each call at the last one's `next` reads every
    /// decision; one whose own work passes the limit is counted, passed
    /// over, and never stops the walk.
    #[test]
    fn a_walk_from_next_to_next_reads_every_decision_it_can() {
        let state = graph();
        let (bar, _) = Bar::for_report(&table());
        let cheap = bar.pattern_work(&state.decisions["d000"]);
        let walk = |max_work: usize| {
            let (mut at, mut judged, mut matches, mut costly) = (None, 0, 0, Vec::new());
            loop {
                let found = shortfalls(&state, &bar, usize::MAX, max_work, from(at));
                judged += found.reach.judged;
                let first = found.found.first.iter();
                matches += first.filter(|(_, s)| s.fault.as_str() == "pattern").count();
                costly.push(found.reach.too_costly);
                match found.reach.next {
                    Some(next) => at = Some(next),
                    None => return (judged, matches, costly),
                }
            }
        };

        let (judged, matches, costly) = walk(cheap * 25);
        assert_eq!(
            (judged, matches, costly.iter().sum::<usize>()),
            (600, 60, 0)
        );
        assert!(costly.len() > 10, "{}", costly.len());
        // Only the decisions with the short reason fit alone.
        let (judged, matches, costly) = walk(cheap);
        assert_eq!((judged, matches, costly.len()), (60, 60, 60));
        assert_eq!(costly[0], 540);
    }

    /// The work of a pattern is the bytes it reads times its NFA's states:
    /// over the same text, a pattern of many states reads fewer decisions
    /// within one limit than a pattern of few.
    #[test]
    fn a_pattern_of_more_states_reads_fewer_decisions_within_one_limit() {
        let state = graph();
        let (few, _) = Bar::for_report(&table());
        let (many, _) = Bar::for_report(&table_of(r"[\x00-\x7f]{0,1000}q"));
        let decision = &state.decisions["d001"];
        assert!(many.pattern_work(decision) > 100 * few.pattern_work(decision));
        let max_work: usize = state.decisions.values().map(|d| few.pattern_work(d)).sum();

        let read = |bar: &Bar| {
            shortfalls(&state, bar, 0, max_work, |_, _| true)
                .reach
                .judged
        };

        assert_eq!(read(&few), 600);
        assert!(read(&many) < 6, "{}", read(&many));
    }
}
