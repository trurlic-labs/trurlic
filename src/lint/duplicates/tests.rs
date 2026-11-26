use std::collections::BTreeMap;

use super::*;
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

/// The vectors of `state`, built with no bound on the work.
fn built(state: &ProjectState) -> Similarity<'_> {
    Similarity::of(state, usize::MAX).unwrap()
}

/// The first page `among`, keeping `limit` pairs, with no bound on its work.
fn pairs_of<'s>(similarity: &Similarity<'s>, among: Among<'_>, limit: usize) -> Page<'s> {
    similarity.page(among, None, limit)
}

/// Every likely duplicate pair: one page bounded by nothing.
fn every<'s>(similarity: &Similarity<'s>) -> Vec<Pair<'s>> {
    let page = pairs_of(similarity, Among::Every, usize::MAX);
    assert_eq!((page.total, page.next), (page.pairs.len(), None));
    page.pairs
}

/// A pair as its two decisions and its score, whatever its page.
fn named<'s>(pairs: &[Pair<'s>]) -> Vec<(&'s str, &'s str, Score)> {
    pairs.iter().map(|p| (p.first, p.second, p.score)).collect()
}

fn listed(state: &ProjectState) -> Vec<(&str, &str, String)> {
    every(&built(state))
        .into_iter()
        .map(|pair| (pair.first, pair.second, pair.score.to_string()))
        .collect()
}

/// A score in billionths, rounded down.
fn nanos(score: Score) -> u128 {
    (u128::from(score.0) * 1_000_000_000) >> 32
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
    let (all, one) = (&similarity.vectors[0], &similarity.vectors[1]);

    let half = "0.500000".to_owned();
    assert_eq!(
        listed(&state),
        ["a1", "a2", "a3", "a4"].map(|other| ("a0", other, half.clone()))
    );
    let score = |dot| likely(all, one, dot).map(|score| score.to_string());
    assert_eq!(score(one.square), Some(half));
    assert_eq!(score(one.square - 1), None);
}

/// A page holds the pairs of its probes in name order while they fit its
/// limit, best first, and names the first probe left out; a first probe
/// whose pairs alone pass the limit keeps its best and the page moves on.
/// A walk from `next` to `next` lists every pair once and counts each.
#[test]
fn a_walk_by_next_lists_every_pair_once_a_page_within_its_limit() {
    let names: Vec<String> = (0..60).map(|index| format!("d{index:02}")).collect();
    let decisions: Vec<(&str, &str, &str)> = names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let choice = if index % 2 == 0 {
                "Cache every read"
            } else {
                "Cache every read of a node"
            };
            (name.as_str(), choice, "The cache skips the parse.")
        })
        .collect();
    let state = graph(&decisions);
    let similarity = built(&state);
    let whole = every(&similarity);

    // d01 has 1 pair, d02 2, d03 3: 6 within 7, and d04 would pass it.
    let first = pairs_of(&similarity, Among::Every, 7);
    let mut expected: Vec<Pair<'_>> = whole
        .iter()
        .filter(|pair| pair.probe <= "d03")
        .cloned()
        .collect();
    expected.sort_by_key(|pair| (Reverse(pair.score), pair.first, pair.second));
    assert_eq!((first.total, first.pairs), (6, expected));
    assert_eq!(first.next, Some(("d04", PageEnd::Pairs)));
    let crowded = similarity.page(Among::Every, Some("d59"), 7);
    assert_eq!((crowded.total, crowded.pairs.len()), (59, 7));
    assert_eq!(crowded.next, None);

    let (mut walked, mut counted, mut from) = (Vec::new(), 0, None);
    loop {
        let page = similarity.page(Among::Every, from, 100);
        counted += page.total;
        walked.extend(named(&page.pairs));
        let Some((next, _)) = page.next else { break };
        from = Some(next);
    }
    walked.sort();
    let mut whole = named(&whole);
    whole.sort();
    assert_eq!(whole.len(), 60 * 59 / 2);
    assert_eq!((counted, walked), (whole.len(), whole));
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
    let mut nearest = similarity.nearest(&[name], count).unwrap();
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
    let both = similarity.nearest(&["absent", "zero"], 1).unwrap();
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
        assert!(similarity.vectors.iter().any(|vector| vector.square == 0));
        assert_eq!(found, exact);
        for &name in &similarity.names {
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
        let page = pairs_of(&similarity, Among::Touching(names), usize::MAX);
        let pairs: Vec<(&str, &str)> = page.pairs.iter().map(|p| (p.first, p.second)).collect();
        (page.total, pairs)
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
            .names
            .iter()
            .copied()
            .filter(|_| rng.below(3) == 0)
            .collect();
        let holds =
            |pair: &Pair<'_>| admitted.contains(&pair.first) || admitted.contains(&pair.second);
        let expected: Vec<Pair<'_>> = every_pair(&similarity).into_iter().filter(holds).collect();

        let page = pairs_of(&similarity, Among::Touching(&admitted), usize::MAX);

        assert!(!expected.is_empty());
        assert_eq!(
            (page.total, named(&page.pairs)),
            (expected.len(), named(&expected))
        );
    }
}

/// The work of the build is 16 units per byte of `choice` and `reason`,
/// a decision counted as 64 bytes more: 217 bytes here, 3472 units. A
/// probe's is the posting entries it reads and 16 per pair it may test:
/// over every pair, `b` reads 2 entries and reaches 1 decision, 18 units,
/// `c` reads 2 and reaches 2, 34; `c` alone, against every other, reads 3
/// entries and reaches 2, 35; `a` and `b` alone would do 37 each, more
/// than every pair costs, so that pass runs.
#[test]
fn each_step_runs_only_while_its_work_is_within_the_bound() {
    let state = graph(&[
        ("a", "alpha beta", ""),
        ("b", "alpha beta", ""),
        ("c", "alpha", ""),
    ]);
    assert_eq!(
        Similarity::of(&state, 3471).err(),
        Some(PastBound { work: 3472 })
    );
    let similarity = built(&state);
    let work = |among| pairs_of(&similarity, among, usize::MAX).work;
    assert_eq!(work(Among::Every), 3524);
    assert_eq!(work(Among::Touching(&["c"])), 3507);
    assert_eq!(work(Among::Touching(&["a", "b"])), 3524);

    let bounded = Similarity::of(&state, 3490).unwrap();
    let page = bounded.page(Among::Every, None, usize::MAX);
    assert_eq!(
        (page.run, page.next),
        (vec!["a", "b"], Some(("c", PageEnd::Work)))
    );
    let next = bounded.page(Among::Every, Some("c"), usize::MAX);
    assert_eq!((next.run, next.next), (vec!["c"], None));
    assert_eq!(
        Similarity::of(&state, 3506)
            .unwrap()
            .nearest(&["c"], 3)
            .err(),
        Some(PastBound { work: 3507 })
    );
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
    let page = pairs_of(&built(&state), Among::Every, usize::MAX);

    // Build: 19 bytes of text and 64 per decision, at 16 units: 4400.
    assert_eq!(page.work, 4417);
}

/// Every pair the exact test passes, tried one by one with its dot
/// product summed over a map of the weights, best first.
fn every_pair<'s>(similarity: &Similarity<'s>) -> Vec<Pair<'s>> {
    let (vectors, names) = (&similarity.vectors, &similarity.names);
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
                    probe: second,
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

/// Pinned bits, from 100-digit decimal arithmetic rounded to Q32.
#[test]
fn the_inverse_frequency_is_the_smoothed_one_in_q32() {
    let cases = [
        (1, 1, 1 << 32),
        (3, 1, 7_272_011_768),
        (3, 2, 5_530_552_389),
        (763, 1, 29_830_354_371),
        (763, 671, 4_846_052_182),
        (2000, 1, 33_965_697_255),
    ];
    for (decisions, holding, expected) in cases {
        assert_eq!(
            vectors::inverse_frequency(decisions, holding),
            expected,
            "{decisions} {holding}"
        );
    }
}

#[test]
fn a_score_prints_six_decimals_rounded_half_up() {
    let shown = |units: u64| Score(units).to_string();
    assert_eq!(shown(0), "0.000000");
    assert_eq!(shown(1 << 31), "0.500000");
    assert_eq!(shown(1 << 32), "1.000000");
    // 0.0000005 is 2147.48 units: one above it rounds up, one below down.
    assert_eq!(shown(2148), "0.000001");
    assert_eq!(shown(2147), "0.000000");
}

/// The pass runs on the thread pool, and what it returns does not depend
/// on how many threads there are. Over a graph where many pairs tie at the
/// cut, one thread and eight give the same count and the same kept pairs.
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
    let on = |threads: usize| {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap();
        pool.install(|| {
            // A walk at 100 pairs a page, its work bounded too, so the
            // pages end at the bound and at the limit.
            let build = Similarity::of(&state, 0).err().unwrap().work;
            let similarity = Similarity::of(&state, build + 5_000).unwrap();
            let (mut pages, mut from) = (Vec::new(), None);
            loop {
                let page = similarity.page(Among::Every, from, 100);
                let next = page.next;
                pages.push(page);
                let Some((next, _)) = next else { break };
                from = Some(next);
            }
            pages
        })
    };

    let one = on(1);
    let counted: usize = one.iter().map(|page| page.total).sum();
    let ends = |why| {
        one.iter()
            .filter(move |page| page.next.is_some_and(|(_, end)| end == why))
    };
    assert!(counted > 1000, "{counted}");
    assert!(ends(PageEnd::Work).count() > 0 && ends(PageEnd::Pairs).count() > 0);
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
