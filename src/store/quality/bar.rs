//! The bar compiled for one batch, and its judgement of each decision an
//! op writes.
//!
//! A decision is held to the bar only in the fields the op writes, so a
//! field below the bar that an op leaves as it is stays as it was. Within
//! them the limits come first, field by field in the order of the file's
//! keys, then the patterns in the order `[quality]` lists them; the first
//! rule broken refuses the decision. A `warn` pattern gives one warning at
//! its first match and never refuses. The patterns of one write share one
//! [`Scan`]: a bound on the bytes they read, a cache per pattern and a
//! total of their growth. A bar compiled before a write takes its lock is
//! a [`PreparedBar`].

use std::borrow::Cow;

use super::pattern::{Compiled, Scan};
use super::verdict::{Breach, Broken, Hit, Limit, Place, QualityFault};
use crate::Error;
use crate::store::format::Store;
use crate::store::model::limits::{MAX_PATTERN_CACHE_TOTAL_BYTES, MAX_WRITE_PATTERN_BYTES};
use crate::store::model::{Decision, DecisionField, Quality, TextField, WordRange, fault};

/// The rules of one `[quality]` table, its patterns compiled.
#[derive(Debug)]
pub(crate) struct Bar {
    reason_words: WordRange,
    alternatives_max: Option<usize>,
    alternative_words_max: Option<usize>,
    anchors_max: Option<usize>,
    require_origin: bool,
    pub(super) patterns: Vec<Compiled>,
}

/// A pattern the lazy DFA does not run: its index and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PatternFault {
    pub(crate) index: usize,
    pub(crate) message: String,
}

impl Bar {
    /// `quality` with each pattern compiled for the lazy DFA, or the first
    /// that does not run on it ([`Compiled::new`]).
    pub(super) fn compile(quality: &Quality) -> Result<Self, PatternFault> {
        let patterns = quality
            .patterns
            .iter()
            .enumerate()
            .map(|(index, pattern)| {
                Compiled::new(pattern).map_err(|message| PatternFault { index, message })
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            patterns,
            ..Self::limits_only(quality)
        })
    }

    /// The limits of `quality`, with no pattern compiled: what a report
    /// judges when it judges no pattern.
    pub(crate) const fn limits_only(quality: &Quality) -> Self {
        Self {
            reason_words: quality.reason_words,
            alternatives_max: quality.alternatives_max,
            alternative_words_max: quality.alternative_words_max,
            anchors_max: quality.anchors_max,
            require_origin: quality.require_origin,
            patterns: Vec::new(),
        }
    }

    /// What one write's patterns may read and build: an empty cache for
    /// each, the bytes of `MAX_WRITE_PATTERN_BYTES` and the growth of
    /// `MAX_PATTERN_CACHE_TOTAL_BYTES`.
    pub(in crate::store) fn scan(&self) -> Scan {
        let (bytes, cache) = (MAX_WRITE_PATTERN_BYTES, MAX_PATTERN_CACHE_TOTAL_BYTES);
        Scan::of(&self.patterns, bytes, cache)
    }

    /// What one report page's patterns may build: an empty cache for each
    /// and the growth of `MAX_PATTERN_CACHE_TOTAL_BYTES`. No byte: a page
    /// charges each decision's bytes before any pattern runs
    /// ([`Bar::pattern_bytes`]), so its patterns search, never charge.
    pub(crate) fn page_scan(&self) -> Scan {
        Scan::of(&self.patterns, 0, MAX_PATTERN_CACHE_TOTAL_BYTES)
    }

    /// `decision` against the bar in the fields of `held`: the first rule
    /// it breaks, or the warnings of the `warn` patterns that match, each
    /// with where it matched. The text rules and the limits come first, in
    /// the order of the file's keys, then the patterns over `scan`.
    pub(in crate::store) fn judge(
        &self,
        decision: &Decision,
        held: &[DecisionField],
        scan: &mut Scan,
    ) -> Result<Vec<(Place, Hit)>, Breach> {
        let fields = || {
            DecisionField::IN_FILE_ORDER
                .into_iter()
                .filter(|field| held.contains(field))
        };
        fields()
            .flat_map(|field| self.checks(decision, field))
            .collect::<Result<(), _>>()?;
        let texts: Vec<_> = fields().flat_map(|field| texts(decision, field)).collect();
        self.judge_patterns(&texts, scan)
    }

    /// The patterns, in their order, each over those of `texts` in the
    /// fields it reads: the first `refuse` match, or the `warn` matches,
    /// each with where it matched. Each pattern charges `scan` the bytes it
    /// would read before it reads them, and searches over its cache there.
    /// Past the bound the write is refused, as it is at the text where a
    /// cache gives up or the growth of the caches passes their total; no
    /// pattern after it runs.
    pub(in crate::store) fn judge_patterns(
        &self,
        texts: &[(Place, Cow<'_, str>)],
        scan: &mut Scan,
    ) -> Result<Vec<(Place, Hit)>, Breach> {
        let mut hits = Vec::new();
        for (index, pattern) in self.patterns.iter().enumerate() {
            let read = pattern.read(texts);
            scan.charge(&read)?;
            let found = scan.search(index, pattern, &read);
            let found = found.map_err(|(place, fault)| Breach {
                place,
                broken: Broken::Quality(fault),
            })?;
            if let Some((place, found)) = found {
                hits.push((place, pattern.verdict(index, place, &found)?));
            }
        }
        Ok(hits)
    }

    /// The batch's `origin` against the patterns that read it, over `scan`:
    /// the first `refuse` match, or the `warn` matches.
    pub(in crate::store) fn judge_origin(
        &self,
        origin: Option<&str>,
        scan: &mut Scan,
    ) -> Result<Vec<Hit>, Breach> {
        let texts: Vec<_> = origin
            .map(|origin| (Place::whole(DecisionField::Origin), Cow::Borrowed(origin)))
            .into_iter()
            .collect();
        let hits = self.judge_patterns(&texts, scan)?;
        Ok(hits.into_iter().map(|(_, hit)| hit).collect())
    }

    /// The message of the pattern at `index` of `[quality] patterns`.
    pub(in crate::store) fn message(&self, index: usize) -> Option<&str> {
        self.patterns
            .get(index)
            .map(|pattern| pattern.message.as_str())
    }

    /// The batch's `origin` against `require_origin`, when the batch
    /// `records` a decision.
    pub(in crate::store) fn origin_required(
        &self,
        origin: Option<&str>,
        records: bool,
    ) -> Result<(), Breach> {
        if origin.is_none() && records && self.require_origin {
            return Err(Breach {
                place: Place::whole(DecisionField::Origin),
                broken: Broken::Quality(QualityFault::OriginRequired),
            });
        }
        Ok(())
    }

    /// The text rules and the limits of `field` held to `decision`, in the
    /// order a write reads them: a breach of each it breaks. An
    /// alternative's text rule comes before its word count.
    pub(super) fn checks(
        &self,
        decision: &Decision,
        field: DecisionField,
    ) -> Vec<Result<(), Breach>> {
        let place = Place::whole(field);
        match field {
            DecisionField::Choice => vec![text(place, TextField::Choice, &decision.choice)],
            DecisionField::Reason => {
                let measured = words(&decision.reason);
                let WordRange { min, max } = self.reason_words;
                vec![
                    text(place, TextField::Reason, &decision.reason),
                    at_least(place, Limit::ReasonWordsMin, measured, min),
                    at_most(place, Limit::ReasonWordsMax, measured, max),
                ]
            }
            DecisionField::Alternatives => self.alternative_checks(decision),
            DecisionField::Anchors => {
                let count = decision.anchors.len();
                vec![
                    at_least(place, Limit::AnchorRequired, count, Some(1)),
                    at_most(place, Limit::AnchorsMax, count, self.anchors_max),
                ]
            }
            DecisionField::Tags => {
                let each = decision.tags.iter().enumerate();
                each.map(|(index, tag)| text(Place::element(field, index), TextField::Tag, tag))
                    .collect()
            }
            // The origin is the batch's, judged once (`judge_origin`) and
            // held to the text rules with the batch's trailers.
            DecisionField::Origin => Vec::new(),
        }
    }

    /// The count of `decision`'s alternatives, then each one's text rule
    /// and word count.
    fn alternative_checks(&self, decision: &Decision) -> Vec<Result<(), Breach>> {
        let field = DecisionField::Alternatives;
        let place = Place::whole(field);
        let count = decision.alternatives.len();
        let (limit, bound) = (Limit::AlternativeWordsMax, self.alternative_words_max);
        let each = decision.alternatives.iter().enumerate();
        let elements = each.flat_map(|(index, alternative)| {
            let place = Place::element(field, index);
            [
                text(place, TextField::Alternative, alternative),
                at_most(place, limit, words(alternative), bound),
            ]
        });
        [
            at_least(place, Limit::AlternativeRequired, count, Some(1)),
            at_most(place, Limit::AlternativesMax, count, self.alternatives_max),
        ]
        .into_iter()
        .chain(elements)
        .collect()
    }
}

/// The texts of `field` a pattern reads, each with its place: an anchor
/// as `file::symbol`. None of the origin, which is the batch's.
pub(crate) fn texts(decision: &Decision, field: DecisionField) -> Vec<(Place, Cow<'_, str>)> {
    let each = |(index, text)| (Place::element(field, index), text);
    match field {
        DecisionField::Choice => vec![(Place::whole(field), Cow::Borrowed(&*decision.choice))],
        DecisionField::Reason => vec![(Place::whole(field), Cow::Borrowed(&*decision.reason))],
        DecisionField::Alternatives => {
            let texts = decision
                .alternatives
                .iter()
                .map(|text| Cow::Borrowed(text.as_str()));
            texts.enumerate().map(each).collect()
        }
        DecisionField::Tags => {
            let texts = decision
                .tags
                .iter()
                .map(|text| Cow::Borrowed(text.as_str()));
            texts.enumerate().map(each).collect()
        }
        DecisionField::Anchors => {
            let texts = decision
                .anchors
                .iter()
                .map(|anchor| Cow::Owned(anchor.to_string()));
            texts.enumerate().map(each).collect()
        }
        // The batch's origin is judged once, by `judge_origin`.
        DecisionField::Origin => Vec::new(),
    }
}

/// The element of `decision` that `place` names, as the bar reads it:
/// `None` for a whole field.
pub(crate) fn element(decision: &Decision, place: Place) -> Option<Cow<'_, str>> {
    let index = place.index?;
    texts(decision, place.field)
        .into_iter()
        .nth(index)
        .map(|(_, text)| text)
}

fn text(place: Place, field: TextField, text: &str) -> Result<(), Breach> {
    match fault(field, text) {
        Some(fault) => Err(Breach {
            place,
            broken: Broken::Text { field, fault },
        }),
        None => Ok(()),
    }
}

fn at_least(
    place: Place,
    limit: Limit,
    measured: usize,
    bound: Option<usize>,
) -> Result<(), Breach> {
    match bound {
        Some(bound) if measured < bound => Err(exceeded(place, limit, measured, bound)),
        Some(_) | None => Ok(()),
    }
}

fn at_most(
    place: Place,
    limit: Limit,
    measured: usize,
    bound: Option<usize>,
) -> Result<(), Breach> {
    match bound {
        Some(bound) if measured > bound => Err(exceeded(place, limit, measured, bound)),
        Some(_) | None => Ok(()),
    }
}

const fn exceeded(place: Place, limit: Limit, measured: usize, bound: usize) -> Breach {
    Breach {
        place,
        broken: Broken::Quality(QualityFault::Limit {
            limit,
            measured,
            bound,
        }),
    }
}

/// The words of `text`: its runs of non-whitespace, split at every Unicode
/// whitespace character. A hyphenated or slashed run is one word.
#[must_use]
fn words(text: &str) -> usize {
    text.split_whitespace().count()
}

/// The bar of a store's `[quality]` as a read found it, compiled before a
/// write takes the lock: compiling holds no other caller back, and the
/// write uses it when the table it loads under the lock is this one.
#[derive(Debug)]
pub(crate) struct PreparedBar {
    quality: Quality,
    bar: Bar,
}

/// The bar of an absent `[quality]`, which compiles no pattern: a write
/// whose graph holds a table compiles that one under the lock.
impl Default for PreparedBar {
    fn default() -> Self {
        let quality = Quality::default();
        Self {
            bar: Bar::limits_only(&quality),
            quality,
        }
    }
}

impl PreparedBar {
    /// `quality` compiled for the write that follows. A pattern that does
    /// not compile is [`Error::QualityPattern`].
    pub(in crate::store) fn compile(store: &Store, quality: Quality) -> crate::Result<Self> {
        let bar = store.bar(&quality)?;
        Ok(Self { quality, bar })
    }

    /// The bar, when it was compiled from `quality`.
    pub(in crate::store) fn of(&self, quality: &Quality) -> Option<&Bar> {
        (self.quality == *quality).then_some(&self.bar)
    }
}

impl Store {
    /// The rules of `quality` compiled for a write: every pattern, once per
    /// batch. A pattern the lazy DFA does not run is
    /// [`Error::QualityPattern`].
    pub(crate) fn bar(&self, quality: &Quality) -> crate::Result<Bar> {
        Bar::compile(quality).map_err(|fault| Error::QualityPattern {
            path: self.project_path(),
            index: fault.index,
            message: fault.message,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;
    use crate::store::format::{canonical, is_canonical};
    use crate::store::limits::{
        MAX_PATTERN_CACHE_BYTES, MAX_PATTERN_COMPILED_BYTES, MAX_PATTERN_FOLDED_CODE_POINTS,
    };
    use crate::store::model::{Anchor, Level, Pattern};
    use crate::store::testing::{OUTGROWS_ITS_CACHE, coin_flips, sample_decision, setup_store};

    fn quality(patterns: &[&str]) -> Quality {
        Quality {
            patterns: patterns
                .iter()
                .map(|&pattern| Pattern {
                    pattern: pattern.into(),
                    fields: [DecisionField::Choice, DecisionField::Reason].into(),
                    level: Level::Warn,
                    message: "M".into(),
                })
                .collect(),
            ..Quality::default()
        }
    }

    /// `decision` judged in its choice and reason under `bar`, which may
    /// read `bound` bytes.
    fn judged(bar: &Bar, decision: &Decision, bound: usize) -> Result<Vec<(Place, Hit)>, Broken> {
        let held = [DecisionField::Choice, DecisionField::Reason];
        let mut scan = Scan::of(&bar.patterns, bound, usize::MAX);
        bar.judge(decision, &held, &mut scan).map_err(|b| b.broken)
    }

    /// Each pattern the lazy DFA cannot run is refused at compile with its
    /// index: a syntax error and an NFA past its size with the engine's
    /// message, a Unicode word boundary and an automaton that cannot start
    /// within its cache with the rewrite that runs.
    #[test]
    fn a_pattern_the_lazy_dfa_cannot_run_is_refused_with_its_index() {
        let refused = |pattern: &str| Bar::compile(&quality(&["ok", pattern])).unwrap_err();
        for (pattern, said) in [
            ("(zz", "unclosed group".to_owned()),
            (
                r"\w{100}",
                format!("exceeded limit of {MAX_PATTERN_COMPILED_BYTES}"),
            ),
            (r"(?i)\bwe\b", r"`(?:^|\W)word(?:\W|$)`".to_owned()),
            (r"x\B", r"`(?-u:\b)`".to_owned()),
            (
                r"\w{9}",
                format!("given cache capacity ({MAX_PATTERN_CACHE_BYTES})"),
            ),
            (
                r"(?i)x[\s\S][\s\S]",
                format!("past the {MAX_PATTERN_FOLDED_CODE_POINTS}"),
            ),
        ] {
            let fault = refused(pattern);
            assert_eq!(fault.index, 1, "{pattern}");
            assert!(
                fault.message.contains(&said),
                "{pattern}: {}",
                fault.message
            );
        }
        let legal = [
            r"(?-u:\b)we(?-u:\b)",
            r"\w{3}y",
            r"(?i)[\s\S]",
            r"[\s\S][\s\S][\s\S]",
        ];
        assert!(Bar::compile(&quality(&legal)).is_ok());
    }

    /// Word classes and case folding are Unicode's: a whole word written
    /// with `\W` is not split at a letter outside ASCII.
    #[test]
    fn patterns_read_text_in_any_script() {
        let bar = Bar::compile(&quality(&[r"(?i)(?:^|\W)we(?:\W|$)"])).unwrap();
        let mut decision = sample_decision("project");
        decision.choice = "Bierzemy we\u{17a}miemy".into();
        assert_eq!(judged(&bar, &decision, usize::MAX), Ok(vec![]));
        decision.choice = "Then WE lock".into();
        let hits = judged(&bar, &decision, usize::MAX).unwrap();
        assert_eq!(hits[0].1.at, 4);
    }

    /// A pattern charges the bytes of each text it would read, one more per
    /// text, before it reads them: a write that may read exactly that is
    /// judged, and one byte less is refused for the bytes at the pattern's
    /// first text, before its search could find a match that refuses.
    #[test]
    fn a_pattern_charges_its_texts_before_it_reads_them() {
        let mut table = quality(&["zz", "yy"]);
        table.patterns[0].level = Level::Refuse;
        let bar = Bar::compile(&table).unwrap();
        let mut decision = sample_decision("project");
        decision.choice = "Keep zz".into();
        decision.reason = "Because it holds.".into();
        let bytes = "Keep zz".len() + 1 + "Because it holds.".len() + 1;

        let judged_within = judged(&bar, &decision, bytes);
        let refused = judged(&bar, &decision, bytes - 1);

        let Err(Broken::Quality(QualityFault::Pattern(found))) = judged_within else {
            panic!("{judged_within:?}")
        };
        assert_eq!(found.index, 0);
        assert_eq!(refused, Err(Broken::Work));
    }

    /// Caches that pass their total stop the patterns at the one whose
    /// search passed it: no later pattern runs, so a later `refuse` match
    /// does not stand in for the total, and the total names the pattern of
    /// the op's texts that grew the most.
    #[test]
    fn the_patterns_stop_at_the_search_that_passes_the_total() {
        let mut table = quality(&["yy", "zz"]);
        table.patterns[1].level = Level::Refuse;
        let bar = Bar::compile(&table).unwrap();
        let mut decision = sample_decision("project");
        decision.choice = "Keep zz".into();
        let held = [DecisionField::Choice, DecisionField::Reason];
        let mut scan = Scan::of(&bar.patterns, usize::MAX, 1);

        let judged = bar.judge(&decision, &held, &mut scan).map_err(|b| b.broken);

        let total = QualityFault::CacheTotal { largest: 0 };
        assert_eq!(judged, Err(Broken::Quality(total)));
    }

    /// Caches that pass their total over one of a pattern's texts are
    /// refused at that text, not at the last the pattern reads.
    #[test]
    fn the_total_is_refused_at_the_text_whose_search_passed_it() {
        let bar = Bar::compile(&quality(&["[a-x]y"])).unwrap();
        let mut decision = sample_decision("project");
        decision.choice = "Keep the lock".into();
        decision.reason = "Because it holds.".into();
        let held = [DecisionField::Choice, DecisionField::Reason];
        let mut scan = Scan::of(&bar.patterns, usize::MAX, 1);

        let breach = bar.judge(&decision, &held, &mut scan).unwrap_err();

        assert_eq!(breach.place, Place::whole(DecisionField::Choice));
    }

    /// A search whose automaton outgrows its cache gives up instead of
    /// clearing it, and the write is refused at the text, naming the
    /// pattern. A fresh cache judges a short text.
    #[test]
    fn a_pattern_whose_cache_fills_gives_up_naming_its_index() {
        let bar = Bar::compile(&quality(&["ok", OUTGROWS_ITS_CACHE])).unwrap();
        let mut decision = sample_decision("project");
        decision.reason = coin_flips(12_000, 7);
        let mut scan = bar.scan();
        let held = [DecisionField::Reason];

        let breach = bar.judge(&decision, &held, &mut scan).unwrap_err();
        decision.reason = "ab".repeat(10);

        assert_eq!(breach.place, Place::whole(DecisionField::Reason));
        assert_eq!(
            breach.broken,
            Broken::Quality(QualityFault::GaveUp { index: 1 })
        );
        assert_eq!(judged(&bar, &decision, usize::MAX), Ok(vec![]));
    }

    /// A cache that gave up is emptied: the same scan judges a later text,
    /// its states built again and charged.
    #[test]
    fn a_cache_that_gave_up_is_emptied_for_the_texts_after_it() {
        let bar = Bar::compile(&quality(&[OUTGROWS_ITS_CACHE])).unwrap();
        let mut decision = sample_decision("project");
        decision.reason = coin_flips(12_000, 7);
        let mut scan = bar.scan();
        let held = [DecisionField::Reason];
        bar.judge(&decision, &held, &mut scan).unwrap_err();
        let spent = scan.cache_left;
        decision.reason = coin_flips(300, 8);

        let judged = bar.judge(&decision, &held, &mut scan);

        let judged = judged.map(|_warnings| ()).map_err(|breach| breach.broken);
        assert_eq!(judged, Ok(()));
        assert!(scan.cache_left < spent, "{spent}");
    }

    /// The example config the repository ships.
    const EXAMPLE: &str = include_str!("../../../examples/quality.toml");

    /// A decision that meets the example's bar in every field.
    fn at_the_bar() -> Decision {
        let mut decision = sample_decision("project");
        decision.choice = "Every write takes the file lock and validates the graph it loads".into();
        decision.reason = "Validating a graph loaded before the lock lets a commit of another \
                           process vanish. Loading under the lock makes the validated graph \
                           the one the commit replaces."
            .into();
        decision.alternatives = vec!["Validate the cached graph - rejected: loses a commit".into()];
        decision.anchors = [Anchor::new("src/store/access.rs".into(), None).unwrap()].into();
        decision
    }

    /// The example loads through the reader of every `project.toml`,
    /// compiles, lets a decision at its bar through without a warning,
    /// and refuses one that breaks it; `fmt` writes it canonical, and the
    /// canonical text reads back as the same table.
    #[test]
    fn the_shipped_example_loads_compiles_and_holds_its_bar() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store(tmp.path());
        let path = store.project_path();
        let header = fs::read_to_string(&path).unwrap();
        fs::write(&path, format!("{header}\n{EXAMPLE}")).unwrap();

        let state = store.load_state().unwrap();
        let quality = &state.project.quality;
        assert_eq!(quality.patterns.len(), 11);
        assert!(quality.require_origin);
        let bar = store.bar(quality).unwrap();
        let all = DecisionField::IN_FILE_ORDER;
        let judged = |decision: &Decision| {
            let mut scan = bar.scan();
            bar.judge(decision, &all, &mut scan).map_err(|b| b.broken)
        };
        assert_eq!(judged(&at_the_bar()), Ok(vec![]));
        let mut we = at_the_bar();
        we.choice = "We take the file lock".into();
        let refused = judged(&we).unwrap_err();
        assert!(
            matches!(&refused, Broken::Quality(QualityFault::Pattern(found)) if found.index == 0),
            "{refused:?}"
        );

        let written = canonical(&state.project);
        assert!(!is_canonical(&state.project, header.as_bytes()));
        store.begin_write().unwrap().format().unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), written);
        assert_eq!(store.load_state().unwrap().project, state.project);
        assert!(
            written.contains("pattern = '(?i)(?:^|\\W)(?:we|our)(?:\\W|$)'\n"),
            "{written}"
        );
    }

    #[test]
    fn words_are_runs_between_unicode_whitespace() {
        for (text, count) in [
            ("", 0),
            (" \t\n ", 0),
            ("one", 1),
            ("  one  two  ", 2),
            ("one-line a/b x::y", 3),
            ("tab\tand\nline", 3),
            // NO-BREAK SPACE and IDEOGRAPHIC SPACE are Unicode whitespace.
            ("a\u{a0}b\u{3000}c", 3),
            // ZERO WIDTH SPACE is not: it joins.
            ("a\u{200b}b", 1),
            ("za\u{17c}\u{f3}\u{142}\u{107} g\u{119}\u{15b}l\u{105}", 2),
            ("end.", 1),
        ] {
            assert_eq!(words(text), count, "{text:?}");
        }
    }
}
