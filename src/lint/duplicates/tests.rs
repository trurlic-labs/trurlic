use std::collections::BTreeMap;

use super::score::likely;
use super::similarity::{Neighbour, Pair};
use super::*;
use crate::store::ProjectState;
use crate::store::seeded::SplitMix64;
use crate::store::testing::{empty_state, sample_decision};

/// Eight decisions: two pairs at the threshold or above, one a little below,
/// and one with no term at all. The reference cosines, the rule of the
/// module doc in double precision (Python's `re.findall` for the terms,
/// `math.log` for the inverse frequencies, `math.fsum` for the sums), to
/// nine decimals: 0.643889186 for the cache pair, 0.519110087 for the lock
/// pair and 0.489697192 for the journal pair.
const FIXTURE: [(&str, &str, &str); 8] = [
    (
        "cache-node-reads",
        "Reads of a node file go through the cache",
        "The cache holds the parsed node file, so a second read skips the parse.",
    ),
    (
        "cache-reads",
        "Cache every read of a node file",
        "A read that hits the cache skips the parse of the node file and returns the value it \
         held.",
    ),
    (
        "journal-commits",
        "A commit writes its journal before its renames",
        "A crash after the journal leaves a write that the next opener finishes.",
    ),
    (
        "journal-renames",
        "Renames follow the journal of a commit",
        "Once the journal is on disk, a stopped commit is finished by the next opener.",
    ),
    (
        "lock-reads",
        "Every read takes the shared lock",
        "A read during a write sees half a commit without the shared lock.",
    ),
    (
        "lock-writes",
        "Every write takes the exclusive lock",
        "A write during another write loses half a commit without the exclusive lock.",
    ),
    (
        "retry-requests",
        "Retry a failed request with backoff",
        "A network blip should not fail the whole sync.",
    ),
    ("silent", "A", "It is the 1."),
];

fn graph(decisions: &[(&str, &str, &str)]) -> ProjectState {
    let mut state = empty_state();
    for &(name, choice, reason) in decisions {
        let mut decision = sample_decision("project");
        decision.choice = choice.into();
        decision.reason = reason.into();
        state.decisions.insert(name.into(), decision);
    }
    state
}

fn built(state: &ProjectState) -> Similarity<'_> {
    Similarity::of(&state.decisions)
}

/// Every likely duplicate pair, best first: one pass bounded by nothing.
fn every<'s>(similarity: &Similarity<'s>) -> Vec<Pair<'s>> {
    let ranked = similarity.pairs(Among::Every, usize::MAX, usize::MAX);
    assert_eq!((ranked.total, ranked.uncompared), (ranked.best.len(), 0));
    ranked.best
}

fn listed(state: &ProjectState) -> Vec<(&str, &str, String)> {
    every(&built(state))
        .into_iter()
        .map(|pair| (pair.first, pair.second, pair.score.to_string()))
        .collect()
}

/// A score in billionths, rounded down.
fn nanos(score: Score) -> u128 {
    (u128::from(score.q32()) * 1_000_000_000) >> 32
}

#[test]
fn the_pairs_at_the_threshold_are_the_reference_cosines_best_first() {
    let state = graph(&FIXTURE);

    assert_eq!(
        listed(&state),
        [
            ("cache-node-reads", "cache-reads", "0.643889".to_owned()),
            ("lock-reads", "lock-writes", "0.519110".to_owned()),
        ]
    );
    let scores: Vec<u128> = every(&built(&state))
        .into_iter()
        .map(|pair| nanos(pair.score))
        .collect();
    // Within 2^-29 (1.9 billionths) of the real-valued cosine, cut to whole
    // billionths, and the reference rounded to them: at most three apart.
    for (score, reference) in scores.into_iter().zip([643_889_186, 519_110_087]) {
        assert!(
            score.abs_diff(reference) <= 3,
            "{score} against {reference}"
        );
    }
}

/// Four terms each two decisions hold, so all four weights are equal: `a0`
/// holds the four, each other decision one of them. Each pair of `a0` and
/// another has a cosine of exactly 1/2, with norms of one and two weights:
/// listed, as `>=` reads. One unit less of dot product is below it.
#[test]
fn a_pair_at_exactly_the_threshold_is_listed_and_one_a_unit_below_is_not() {
    let state = graph(&[
        ("a0", "alpha beta gamma delta", ""),
        ("a1", "alpha", ""),
        ("a2", "beta", ""),
        ("a3", "delta", ""),
        ("a4", "gamma", ""),
    ]);
    let similarity = built(&state);
    // `a1`'s one term is `a0`'s at the same weight: their dot product is
    // `a1`'s square.
    let (all, one) = (&similarity.vectors()[0], &similarity.vectors()[1]);

    let half = "0.500000".to_owned();
    assert_eq!(
        listed(&state),
        ["a1", "a2", "a3", "a4"].map(|other| ("a0", other, half.clone()))
    );
    let score = |dot| likely(all, one, dot).map(|score| score.to_string());
    assert_eq!(score(one.square), Some(half));
    assert_eq!(score(one.square - 1), None);
}

#[test]
fn identical_texts_score_one_and_a_text_without_terms_pairs_with_none() {
    let state = graph(&[
        ("copy", "Retry with backoff", "A blip passes."),
        ("empty", "A", "It is 1."),
        ("original", "Retry with backoff", "A blip passes."),
        ("void", "The", "Of 2."),
    ]);

    assert_eq!(
        listed(&state),
        [("copy", "original", "1.000000".to_owned())]
    );
    assert_eq!(nearest_of(&built(&state), "empty", 3), []);
}

/// The likely duplicates of the one decision `name`.
fn nearest_of<'s>(similarity: &Similarity<'s>, name: &str, count: usize) -> Vec<Neighbour<'s>> {
    let mut nearest = similarity.nearest(&[name], count);
    assert_eq!(nearest.len(), 1);
    nearest.remove(0)
}

#[test]
fn nearest_lists_the_likely_duplicates_best_first_up_to_its_count() {
    let state = graph(&FIXTURE);
    let similarity = built(&state);
    let nearest = |name, count| -> Vec<(&str, String)> {
        nearest_of(&similarity, name, count)
            .into_iter()
            .map(|neighbour| (neighbour.name, neighbour.score.to_string()))
            .collect()
    };

    assert_eq!(
        nearest("cache-reads", 3),
        [("cache-node-reads", "0.643889".to_owned())]
    );
    assert_eq!(nearest("lock-writes", 0), []);
    assert_eq!(nearest("journal-commits", 3), []);
    assert_eq!(nearest("absent", 3), []);

    let copies = graph(&[
        (
            "base",
            "Sign every token with the rotating key",
            "Tokens stay short.",
        ),
        (
            "one",
            "Sign every token with the rotating key",
            "Tokens stay short.",
        ),
        ("three", "Sign every token with a key", "Tokens stay short."),
        (
            "two",
            "Sign every token with the rotating key",
            "Tokens stay.",
        ),
        ("zero", "Sign every token", "Short tokens."),
    ]);
    let similarity = built(&copies);
    let names: Vec<&str> = nearest_of(&similarity, "base", 3)
        .into_iter()
        .map(|neighbour| neighbour.name)
        .collect();
    assert_eq!(names, ["one", "two", "three"]);
    let both = similarity.nearest(&["absent", "zero"], 1);
    assert_eq!(both.len(), 2);
    assert_eq!(both[0], []);
}

/// Texts of 2 to 9 words drawn from a small vocabulary, so many pairs sit
/// near the threshold: forty decisions, the first empty and the second stop
/// words alone.
fn drawn(rng: &mut SplitMix64) -> Vec<(String, String)> {
    const WORDS: [&str; 14] = [
        "lock", "read", "write", "commit", "journal", "cache", "node", "file", "graph", "store",
        "batch", "retry", "the", "of",
    ];
    let mut texts: Vec<(String, String)> = (0..40)
        .map(|index| {
            let words = 2 + rng.below(8);
            let text: Vec<&str> = (0..words).map(|_| WORDS[rng.below(WORDS.len())]).collect();
            (format!("d{index:02}"), text.join(" "))
        })
        .collect();
    texts[0].1 = String::new();
    texts[1].1 = "The of the.".into();
    texts
}

fn graph_of(texts: &[(String, String)]) -> ProjectState {
    let decisions: Vec<(&str, &str, &str)> = texts
        .iter()
        .map(|(name, text)| (name.as_str(), text.as_str(), ""))
        .collect();
    graph(&decisions)
}

/// The index loses no pair. On drawn graphs, the index finds exactly the
/// pairs the exact test over every pair finds, and each decision's nearest
/// are its pairs.
#[test]
fn the_index_finds_every_pair_the_exact_test_over_all_pairs_finds() {
    let mut rng = SplitMix64::new(11);
    for _ in 0..20 {
        let state = graph_of(&drawn(&mut rng));
        let similarity = built(&state);

        let found = every(&similarity);
        let exact = every_pair(&similarity);
        assert!(!exact.is_empty());
        assert!(similarity.vectors().iter().any(|vector| vector.square == 0));
        assert_eq!(found, exact);
        for &name in similarity.names() {
            let mut paired: Vec<&str> = found
                .iter()
                .filter_map(|pair| match (pair.first == name, pair.second == name) {
                    (true, _) => Some(pair.second),
                    (false, true) => Some(pair.first),
                    (false, false) => None,
                })
                .collect();
            let mut nearest: Vec<&str> = nearest_of(&similarity, name, usize::MAX)
                .into_iter()
                .map(|neighbour| neighbour.name)
                .collect();
            paired.sort_unstable();
            nearest.sort_unstable();
            assert_eq!(nearest, paired, "{name}");
        }
    }
}

/// A pair touches the decisions named when either is one of them: each of
/// the cache pair's decisions alone lists it, a decision in no pair lists
/// none. On drawn graphs and a third of their decisions drawn, the pass
/// lists exactly the pairs of every pair that hold one, each once.
#[test]
fn the_pairs_touching_some_decisions_are_those_that_hold_one_of_them() {
    let state = graph(&FIXTURE);
    let similarity = built(&state);
    let touching = |names: &[&str]| {
        let ranked = similarity.pairs(Among::Touching(names), usize::MAX, usize::MAX);
        let pairs: Vec<(&str, &str)> = ranked.best.iter().map(|p| (p.first, p.second)).collect();
        (ranked.total, pairs)
    };
    let cache = (1, vec![("cache-node-reads", "cache-reads")]);
    assert_eq!(touching(&["cache-node-reads"]), cache);
    assert_eq!(touching(&["cache-reads"]), cache);
    assert_eq!(touching(&["silent"]), (0, vec![]));
    assert_eq!(touching(&["absent"]), (0, vec![]));

    let mut rng = SplitMix64::new(5);
    for _ in 0..20 {
        let state = graph_of(&drawn(&mut rng));
        let similarity = built(&state);
        let admitted: Vec<&str> = similarity
            .names()
            .iter()
            .copied()
            .filter(|_| rng.below(3) == 0)
            .collect();
        let holds =
            |pair: &Pair<'_>| admitted.contains(&pair.first) || admitted.contains(&pair.second);
        let expected: Vec<Pair<'_>> = every_pair(&similarity).into_iter().filter(holds).collect();

        let ranked = similarity.pairs(Among::Touching(&admitted), usize::MAX, usize::MAX);

        assert!(!expected.is_empty());
        assert_eq!((ranked.total, ranked.best), (expected.len(), expected));
    }
}

/// A probe's work is the posting entries it reads and 16 per pair it may
/// test. Over every pair, `a` reads none, `b` reads 2 entries and reaches
/// 1 decision, 18 units, and `c` reads 2 and reaches 2, 34. `c` alone,
/// against every other, reads 3 entries and reaches 2, 35. The probes run
/// by name while their work fits the bound, and those left are counted.
#[test]
fn the_probes_run_by_name_while_their_work_is_within_the_bound() {
    let state = graph(&[
        ("a", "alpha beta", ""),
        ("b", "alpha beta", ""),
        ("c", "alpha", ""),
    ]);
    let similarity = built(&state);
    let pass = |among, bound| similarity.pairs(among, usize::MAX, bound);
    let whole = pass(Among::Every, usize::MAX);
    assert_eq!((whole.work, whole.total, whole.compared), (52, 3, 3));
    assert_eq!(pass(Among::Touching(&["c"]), usize::MAX).work, 35);

    let stopped = pass(Among::Every, 51);
    let pairs: Vec<(&str, &str)> = stopped.best.iter().map(|p| (p.first, p.second)).collect();
    assert_eq!((stopped.compared, stopped.uncompared), (2, 1));
    assert_eq!(
        (stopped.work, stopped.total, pairs),
        (52, 1, vec![("a", "b")])
    );
    let none = pass(Among::Touching(&["c"]), 34);
    assert_eq!((none.compared, none.uncompared, none.total), (0, 1, 0));
}

/// A probe may test no more pairs than the entries it reads: `x` reads 1
/// entry of the 1 decision before it, `y` and `z` none of the 2 and 3.
#[test]
fn a_probe_is_charged_no_pair_it_reads_no_entry_for() {
    let state = graph(&[
        ("w", "alpha", ""),
        ("x", "alpha", ""),
        ("y", "beta", ""),
        ("z", "gamma", ""),
    ]);
    let ranked = built(&state).pairs(Among::Every, usize::MAX, usize::MAX);

    assert_eq!(ranked.work, 17);
}

/// Decisions that share every term, each holding each a drawn number of
/// times: every pair is a likely duplicate and many tie. The pass keeps
/// exactly the exact test's best pairs by score, then by names, and
/// counts every pair.
#[test]
fn on_a_dense_graph_the_best_pairs_are_kept_and_every_pair_counted() {
    const WORDS: [&str; 5] = ["lock", "read", "write", "commit", "cache"];
    let mut rng = SplitMix64::new(7);
    let texts: Vec<(String, String)> = (0..200)
        .map(|index| {
            let words = WORDS
                .iter()
                .flat_map(|&word| std::iter::repeat_n(word, 1 + rng.below(3)));
            (format!("d{index:03}"), words.collect::<Vec<_>>().join(" "))
        })
        .collect();
    let state = graph_of(&texts);
    let similarity = built(&state);
    let exact = every_pair(&similarity);

    let ranked = similarity.pairs(Among::Every, 100, usize::MAX);

    assert_eq!(exact.len(), 200 * 199 / 2);
    assert_eq!(ranked.total, exact.len());
    assert_eq!(ranked.best, exact[..100]);
}

/// Every pair the exact test passes, tried one by one with its dot
/// product summed over a map of the weights, best first.
fn every_pair<'s>(similarity: &Similarity<'s>) -> Vec<Pair<'s>> {
    let (vectors, names) = (similarity.vectors(), similarity.names());
    let weights: Vec<BTreeMap<usize, u64>> = vectors
        .iter()
        .map(|vector| vector.terms.iter().copied().collect())
        .collect();
    let mut pairs = Vec::new();
    for (a, first) in weights.iter().enumerate() {
        for (b, second) in weights.iter().enumerate().skip(a + 1) {
            let dot: u128 = first
                .iter()
                .filter_map(|(term, &weight)| {
                    Some(u128::from(weight) * u128::from(*second.get(term)?))
                })
                .sum();
            if let Some(score) = likely(&vectors[a], &vectors[b], dot) {
                let (first, second) = (names[a], names[b]);
                pairs.push(Pair {
                    first,
                    second,
                    score,
                });
            }
        }
    }
    pairs.sort_by(|x, y| {
        y.score
            .cmp(&x.score)
            .then_with(|| (x.first, x.second).cmp(&(y.first, y.second)))
    });
    pairs
}

/// The pass runs on the thread pool, and what it returns does not depend
/// on how many threads there are. Over a graph where many pairs tie at the
/// cut and the bound stops the probes, one thread and eight give the same
/// count, the same kept pairs and the same stop.
#[test]
fn the_pairs_are_the_same_on_any_number_of_threads() {
    let names: Vec<String> = (0..300).map(|index| format!("d{index:03}")).collect();
    let mut rng = SplitMix64::new(3);
    const WORDS: [&str; 6] = ["lock", "read", "write", "commit", "cache", "node"];
    let texts: Vec<String> = (0..300)
        .map(|_| {
            let words: Vec<&str> = (0..4).map(|_| WORDS[rng.below(WORDS.len())]).collect();
            words.join(" ")
        })
        .collect();
    let decisions: Vec<(&str, &str, &str)> = names
        .iter()
        .zip(&texts)
        .map(|(name, text)| (name.as_str(), text.as_str(), ""))
        .collect();
    let state = graph(&decisions);
    let whole = built(&state).pairs(Among::Every, 0, usize::MAX).work;
    let on = |threads: usize| {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap();
        pool.install(|| built(&state).pairs(Among::Every, 100, whole / 2))
    };

    let one = on(1);
    assert!(one.total > 1000, "{}", one.total);
    assert!(one.uncompared > 0 && one.compared > 0, "{one:?}");
    assert_eq!(one, on(8));
}

/// The seeded corpus draws its texts from a long-tailed vocabulary. It
/// restates one decision in 25 by opening its reason with the first half
/// of an earlier one's. Its likely duplicates are some of those, and no
/// pair of unrelated texts, so the benches time a pass with something to
/// find and little to list.
#[test]
fn the_seeded_corpus_pairs_only_its_restatements() {
    use crate::store::corpus::Corpus;

    let tmp = tempfile::TempDir::new().unwrap();
    let store = Corpus::new(600).write(tmp.path()).unwrap();
    let state = store.load_state().unwrap();

    let similarity = built(&state);
    let pairs = every(&similarity);
    assert!((4..=24).contains(&pairs.len()), "{}", pairs.len());
    for pair in pairs {
        let reason = |name: &str| state.decisions[name].reason.as_bytes();
        let (first, second) = (reason(pair.first), reason(pair.second));
        let opening = first.iter().zip(second).take_while(|(a, b)| a == b).count();
        assert!(opening >= 200, "{pair:?}");
    }
}

/// Two decisions at the 1 MiB a node file holds, one term over and over:
/// no sum overflows and the pair scores one.
#[test]
fn the_longest_texts_a_file_holds_overflow_nothing() {
    let reason = "ab ".repeat((1 << 20) / 3);
    let state = graph(&[("x", "ab", &reason), ("y", "ab", &reason), ("z", "cd", "")]);

    assert_eq!(listed(&state), [("x", "y", "1.000000".to_owned())]);
}
