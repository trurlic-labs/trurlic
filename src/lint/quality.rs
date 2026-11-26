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
    /// the order the bar reads them, then the patterns in their order. None
    /// of a decision at or past where the patterns stopped.
    pub(crate) found: Found<(&'s str, Shortfall)>,
    /// The decision of the first finding past the `limit` kept: the
    /// findings kept of it are only some of its own.
    pub(crate) beyond: Option<&'s str>,
    pub(crate) reach: Reach<'s>,
}

/// How far the bar's patterns read the decisions wanted, by name.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Reach<'s> {
    /// The decisions the patterns read, by name.
    pub(crate) read: Vec<&'s str>,
    /// The decisions wanted, by name.
    pub(crate) wanted: Vec<&'s str>,
    /// The bytes the patterns would read over every decision wanted.
    pub(crate) bytes: usize,
    /// Where the patterns stopped before the last decision wanted.
    pub(crate) next: Option<Next<'s>>,
}

/// The first decision the patterns left unread, where a call that starts
/// there reads on, and why they stopped before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Next<'s> {
    pub(crate) decision: &'s str,
    pub(crate) why: Stop,
}

/// Why the patterns stopped before a decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stop {
    /// Reading it would take their bytes past the bound.
    Bytes,
    /// A pattern's cache gave up on it, or the caches grew past their
    /// total over it, filled by the decisions before it on the page.
    GaveUp,
    /// The findings before it pass what a page lists.
    Listing,
}

/// Where a finding comes in its decision's: the limits in the order the
/// bar reads them, then the patterns' findings in their order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
    /// A limit's finding, by its order among the decision's.
    Limit(usize),
    /// A pattern's finding, by its order among the decision's.
    Pattern(usize),
}

/// A finding with its decision's position among those wanted.
type Placed = (usize, Rank, Shortfall);

/// Every limit of `bar` each decision `wanted` admits by name breaks, and
/// every match of its patterns in the decisions they read. They read
/// those by name while their bytes stay within `max_bytes`
/// ([`Bar::pattern_bytes`]), the first whatever its bytes, and no further
/// than the decision whose findings pass the first `limit` or that a
/// pattern's cache gives up on ([`pattern_runs`]). The count, the first
/// `limit` with their decisions' names, and the decision of the first
/// finding past them.
///
/// The limits are judged on the thread pool over the decisions, each task
/// a run of them in order, keeping its first `limit` findings, merged in
/// order: the pass holds a bounded number of findings, and nothing depends
/// on the threads. The patterns judge one decision after another over one
/// scan for the page, as a write judges its ops.
pub(crate) fn shortfalls<'s>(
    state: &'s ProjectState,
    bar: &Bar,
    limit: usize,
    max_bytes: usize,
    wanted: impl Fn(&str, &Decision) -> bool,
) -> Shortfalls<'s> {
    let decisions: Vec<(&'s str, &'s Decision)> = state
        .decisions
        .iter()
        .map(|(name, decision)| (name.as_str(), decision))
        .filter(|&(name, decision)| wanted(name, decision))
        .collect();
    let name = |position: usize| decisions.get(position).map(|&(name, _)| name);
    let (bytes, within) = reach(bar, &decisions, max_bytes);
    let read = decisions.get(..within).unwrap_or_default();
    // One more than kept, to find the decision of the first finding past
    // the kept: each part keeps its own first, so the merge holds it.
    let kept = limit.saturating_add(1);
    let limits = limit_findings(bar, &decisions, kept);
    let (patterns, judged, stop) = pattern_runs(bar, read, &limits, kept);
    let stop = stop
        .or(Some(Stop::Bytes))
        .filter(|_| judged < decisions.len());
    let mut found = merged(limits, patterns, kept);
    found.first.retain(|&(position, ..)| position < judged);
    let names = || decisions.iter().map(|&(name, _)| name);
    let reach = Reach {
        read: names().take(judged).collect(),
        wanted: names().collect(),
        bytes,
        next: stop.and_then(|why| {
            let decision = name(judged)?;
            Some(Next { decision, why })
        }),
    };
    let (found, beyond) = named(found, limit, name);
    Shortfalls {
        found,
        beyond,
        reach,
    }
}

/// The first `limit` of `found`, each with the name `name` gives its
/// decision's position, and the decision of the first finding past them.
fn named<'s>(
    found: Found<Placed>,
    limit: usize,
    name: impl Fn(usize) -> Option<&'s str>,
) -> (Found<(&'s str, Shortfall)>, Option<&'s str>) {
    let Found { total, mut first } = found;
    let beyond = first.get(limit).and_then(|&(position, ..)| name(position));
    first.truncate(limit);
    let first = first
        .into_iter()
        .filter_map(|(position, _, shortfall)| Some((name(position)?, shortfall)));
    let first = first.collect();
    (Found { total, first }, beyond)
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

/// The matches of the patterns of `bar` in `read`, the first `kept`,
/// decision by decision over one scan for the page ([`Bar::page_scan`]).
/// Once the findings before the next decision, `limits`' and the
/// patterns', pass `kept`, a page cannot list past them, so the rest go
/// unjudged. A cache that gives up on a decision, or caches that grow past
/// their total over it, end the judging before it, so the next page judges
/// it first, over fresh caches. On the first decision read, the judging
/// ends after it, the give-up or the total its finding. The matches, how
/// many of `read` the patterns judged, and why they stopped there.
fn pattern_runs(
    bar: &Bar,
    read: &[(&str, &Decision)],
    limits: &Found<Placed>,
    kept: usize,
) -> (Found<Placed>, usize, Option<Stop>) {
    let mut scan = bar.page_scan();
    let mut found = Found::new();
    for (position, &(_, decision)) in read.iter().enumerate() {
        let (shortfalls, stopped) = bar.audit_patterns(decision, &mut scan);
        if position > 0 && stopped.is_some() {
            return (found, position, Some(Stop::GaveUp));
        }
        let ended = stopped.is_some();
        let each = shortfalls.into_iter().chain(stopped).enumerate();
        for (order, shortfall) in each {
            found.add(kept, (position, Rank::Pattern(order), shortfall));
        }
        if ended {
            return (found, 1, Some(Stop::GaveUp));
        }
        let limited = limits.first.iter().take_while(|&&(at, ..)| at <= position);
        let listed = limited.count().saturating_add(found.total);
        if position + 1 < read.len() && listed >= kept {
            return (found, position + 1, Some(Stop::Listing));
        }
    }
    (found, read.len(), None)
}

/// The bytes the patterns of `bar` would read over every one of
/// `decisions`, and how many of them, in order, they read within
/// `max_bytes`: the first whatever its bytes, so a call that starts at a
/// decision reads at least it.
fn reach(bar: &Bar, decisions: &[(&str, &Decision)], max_bytes: usize) -> (usize, usize) {
    let bytes = decisions
        .iter()
        .map(|&(_, decision)| bar.pattern_bytes(decision));
    let (mut spent, mut within) = (0usize, None);
    for (position, cost) in bytes.enumerate() {
        spent = spent.saturating_add(cost);
        if position > 0 && spent > max_bytes && within.is_none() {
            within = Some(position);
        }
    }
    (spent, within.unwrap_or(decisions.len()))
}

/// The limits' findings and the patterns', in order, the first `limit` of
/// them: each part holds its own first `limit`, so the merge does.
fn merged(limits: Found<Placed>, patterns: Found<Placed>, limit: usize) -> Found<Placed> {
    let mut all = limits;
    all.total = all.total.saturating_add(patterns.total);
    all.first.extend(patterns.first);
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
    use crate::store::testing::{OUTGROWS_ITS_CACHE, coin_flips, empty_state, sample_decision};
    use crate::store::{Anchor, DecisionField, Level, Pattern, Quality, QualityFault};

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

    /// The pass counts every finding of the limits, keeps the first it is
    /// asked for by decision name, the limits before the patterns, and
    /// gives the same on one thread and on eight.
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
        assert_eq!(one.total, 540 * 2 + 1);
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
        assert_eq!(reach.wanted.len(), 600);
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

    /// The patterns read the decisions by name while their bytes stay
    /// within the bound, the first that would pass it named as `next`; the
    /// limits judge every decision, and the page lists none of the
    /// decisions from `next` on.
    #[test]
    fn the_patterns_read_the_decisions_by_name_within_the_bytes_bound() {
        let state = graph();
        let (bar, _) = Bar::for_report(&table());
        let bytes: Vec<usize> = state
            .decisions
            .values()
            .map(|decision| bar.pattern_bytes(decision))
            .collect();
        let whole: usize = bytes.iter().sum();
        // Exactly the first ten decisions: d000, a match, then d001 to d009.
        let ten: usize = bytes[..10].iter().sum();

        for (max_bytes, judged, next) in [
            (ten, 10, "d010"),
            (ten + 1, 10, "d010"),
            (ten - 1, 9, "d009"),
        ] {
            let found = shortfalls(&state, &bar, usize::MAX, max_bytes, |_, _| true);
            let reach = &found.reach;
            let read = (reach.read.len(), reach.wanted.len(), reach.bytes);
            assert_eq!(read, (judged, 600, whole), "{max_bytes}");
            let stop = Next {
                decision: next,
                why: Stop::Bytes,
            };
            assert_eq!(reach.next, Some(stop), "{max_bytes}");
            assert_eq!(found.found.total, 540 * 2 + 1, "{max_bytes}");
            let listed = found.found.first.iter();
            assert!(listed.clone().all(|&(name, _)| name < next), "{max_bytes}");
            assert_eq!(listed.count(), 1 + (judged - 1) * 2, "{max_bytes}");
        }
        let all = shortfalls(&state, &bar, usize::MAX, whole, |_, _| true);
        assert_eq!((all.reach.read.len(), all.reach.next), (600, None));
        let first = shortfalls(&state, &bar, usize::MAX, 0, |_, _| true);
        assert_eq!(first.reach.read, ["d000"]);
    }

    /// Every finding of a walk that starts each call at the last one's
    /// `next`, and the reasons the calls stopped.
    fn walk(
        state: &ProjectState,
        bar: &Bar,
        max_bytes: usize,
    ) -> (usize, Vec<(String, Shortfall)>, Vec<Stop>) {
        let (mut at, mut judged, mut listed, mut whys) = (None, 0, Vec::new(), Vec::new());
        loop {
            let found = shortfalls(state, bar, usize::MAX, max_bytes, from(at));
            judged += found.reach.read.len();
            let each = found.found.first.into_iter();
            listed.extend(each.map(|(name, shortfall)| (name.to_owned(), shortfall)));
            match found.reach.next {
                Some(next) => {
                    whys.push(next.why);
                    at = Some(next.decision);
                }
                None => return (judged, listed, whys),
            }
        }
    }

    /// A caller that starts each call at the last one's `next` reads every
    /// decision and lists every finding once, the limits' with the
    /// patterns'.
    #[test]
    fn a_walk_from_next_to_next_lists_every_finding_once() {
        let state = graph();
        let (bar, _) = Bar::for_report(&table());
        let cheap = bar.pattern_bytes(&state.decisions["d000"]);
        let whole = shortfalls(&state, &bar, usize::MAX, usize::MAX, |_, _| true);
        let whole: Vec<(String, Shortfall)> = whole
            .found
            .first
            .into_iter()
            .map(|(name, shortfall)| (name.to_owned(), shortfall))
            .collect();

        let (judged, listed, whys) = walk(&state, &bar, cheap * 25);

        assert_eq!(judged, 600);
        assert_eq!(listed, whole);
        assert!(whys.len() > 10 && whys.iter().all(|&why| why == Stop::Bytes));
    }

    /// The pass names the decision of the first finding past those it
    /// keeps, so a page can end at that decision's boundary.
    #[test]
    fn the_first_finding_past_the_kept_names_its_decision() {
        let state = graph();
        let (bar, _) = Bar::for_report(&table());

        let cut = shortfalls(&state, &bar, 4, usize::MAX, |_, _| true);
        let whole = shortfalls(&state, &bar, usize::MAX, usize::MAX, |_, _| true);

        // d000's pattern, then two findings each of d001, d002: d002's
        // second is the fifth, past the four kept.
        assert_eq!(cut.beyond, Some("d002"));
        assert_eq!(cut.found.first.len(), 4);
        assert_eq!(whole.beyond, None);
    }

    /// The patterns stop once the findings before them pass what a page
    /// lists: the decisions past them go unjudged, and the first of them is
    /// where a call reads on.
    #[test]
    fn the_patterns_stop_where_a_page_can_list_no_further() {
        let state = graph();
        let (bar, _) = Bar::for_report(&table());

        let page = shortfalls(&state, &bar, 5, usize::MAX, |_, _| true);
        let whole = shortfalls(&state, &bar, usize::MAX, usize::MAX, |_, _| true);

        assert_eq!(page.beyond, Some("d003"));
        assert!(page.reach.read.len() < 600, "{}", page.reach.read.len());
        let next = page.reach.next.unwrap();
        assert_eq!(next.why, Stop::Listing);
        assert!(next.decision > "d003", "{next:?}");
        assert!(!page.reach.read.contains(&next.decision), "{next:?}");
        assert_eq!(whole.reach.read.len(), 600);
    }

    /// Ten decisions under a pattern no cache holds over a long random
    /// text: `d001` and `d002` fit a cache each but not together, `d005`
    /// fits none, and `d006` fits one alone, which `d005` left full. A page
    /// ends before the decision a cache gave up on, which the next page
    /// judges first over fresh caches; one a fresh cache gives up on is the
    /// give-up's finding, at `refuse`, and the page ends after it. A walk
    /// judges every decision and lists what judging each alone lists.
    #[test]
    fn a_give_up_ends_the_page_and_a_walk_lists_each_finding_once() {
        let mut state = empty_state();
        for index in 0..10 {
            let mut decision = sample_decision("project");
            decision.reason = match index {
                1 | 2 | 6 => coin_flips(2_500, index),
                5 => coin_flips(12_000, index),
                _ => decision.reason,
            };
            state.decisions.insert(format!("d{index:03}"), decision);
        }
        let (bar, _) = Bar::for_report(&table_of(OUTGROWS_ITS_CACHE));

        let (judged, listed, whys) = walk(&state, &bar, usize::MAX);

        assert_eq!(judged, 10);
        assert_eq!(whys, [Stop::GaveUp, Stop::GaveUp, Stop::GaveUp]);
        let gave_up: Vec<(&str, Level)> = listed
            .iter()
            .filter(|(_, shortfall)| shortfall.fault == QualityFault::GaveUp { index: 0 })
            .map(|(name, shortfall)| (name.as_str(), shortfall.level))
            .collect();
        assert_eq!(gave_up, [("d005", Level::Refuse)]);
        let alone: Vec<(String, Shortfall)> = state
            .decisions
            .keys()
            .flat_map(|name| {
                let found = shortfalls(&state, &bar, usize::MAX, usize::MAX, |n, _| n == name);
                let each = found.found.first.into_iter();
                each.map(|(name, shortfall)| (name.to_owned(), shortfall))
                    .collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(listed, alone);
    }

    /// The patterns judge no further than the decision whose findings reach
    /// what a page keeps: with one finding a decision, a page that keeps
    /// three judges three decisions and reads on at the fourth.
    #[test]
    fn the_patterns_stop_at_the_decision_whose_findings_reach_the_kept() {
        let mut state = empty_state();
        for index in 0..10 {
            let mut decision = sample_decision("project");
            decision.alternatives = vec!["None".into()];
            decision.anchors = [Anchor::new("src/a.rs".into(), Some("f".into())).unwrap()].into();
            decision.reason = "A reason with zz.".into();
            state.decisions.insert(format!("d{index:03}"), decision);
        }
        let (bar, _) = Bar::for_report(&table());

        let page = shortfalls(&state, &bar, 2, usize::MAX, |_, _| true);

        assert_eq!(page.reach.read, ["d000", "d001", "d002"]);
        let next = page.reach.next.map(|next| (next.decision, next.why));
        assert_eq!(next, Some(("d003", Stop::Listing)));
    }

    /// Decisions over which the patterns' caches together grow past
    /// `MAX_PATTERN_CACHE_TOTAL_BYTES` end a page before them; the first of
    /// a page that does so alone is the total's finding, at `refuse`, naming
    /// the pattern that grew the most, and the page ends after it.
    #[test]
    fn caches_past_their_total_end_the_page() {
        let mut state = empty_state();
        for index in 0..3 {
            let mut decision = sample_decision("project");
            decision.reason = coin_flips(3_000, index);
            state.decisions.insert(format!("d{index:03}"), decision);
        }
        let table = Quality {
            patterns: vec![table_of(OUTGROWS_ITS_CACHE).patterns[0].clone(); 8],
            ..Quality::default()
        };
        let (bar, _) = Bar::for_report(&table);

        let page = shortfalls(&state, &bar, usize::MAX, usize::MAX, |_, _| true);

        assert_eq!(page.reach.read, ["d000"]);
        let next = page.reach.next.map(|next| (next.decision, next.why));
        assert_eq!(next, Some(("d001", Stop::GaveUp)));
        let total = QualityFault::CacheTotal { largest: 0 };
        let found: Vec<&Shortfall> = page.found.first.iter().map(|(_, s)| s).collect();
        assert!(
            found
                .iter()
                .any(|s| s.fault == total && s.level == Level::Refuse),
            "{found:?}"
        );
    }

    /// A page's caches grow past their total by one pattern's search at
    /// most, as a write's do: the patterns judge a decision in their order,
    /// and none after the one whose search passed the total runs, so a
    /// last pattern that would match gives no finding.
    #[test]
    fn a_page_runs_no_pattern_after_the_search_that_passes_the_total() {
        let mut state = empty_state();
        let mut decision = sample_decision("project");
        decision.reason = coin_flips(3_000, 0);
        state.decisions.insert("d000".into(), decision);
        let mut patterns = vec![table_of(OUTGROWS_ITS_CACHE).patterns[0].clone(); 8];
        patterns.extend(table_of("ab").patterns);
        let table = Quality {
            patterns,
            ..Quality::default()
        };
        let (bar, _) = Bar::for_report(&table);

        let page = shortfalls(&state, &bar, usize::MAX, usize::MAX, |_, _| true);

        let found = page.found.first.iter().map(|(_, s)| &s.fault);
        let last = found.clone().find(|fault| match fault {
            QualityFault::Pattern(found) => found.index == 8,
            QualityFault::Limit { .. }
            | QualityFault::GaveUp { .. }
            | QualityFault::CacheTotal { .. }
            | QualityFault::OriginRequired => false,
        });
        assert_eq!(last, None);
        let rules: Vec<&str> = found.map(QualityFault::as_str).collect();
        assert_eq!(rules.last(), Some(&"pattern_cache_total"), "{rules:?}");
    }
}
