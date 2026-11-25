//! The bar compiled for one batch, and its judgement of each decision an
//! op writes.
//!
//! A decision is held to the bar only in the fields the op writes, so a
//! field below the bar that an op leaves as it is stays as it was. Within
//! them the limits come first, field by field in the order of the file's
//! keys, then the patterns in the order `[quality]` lists them; the first
//! rule broken refuses the decision. A `warn` pattern gives one warning at
//! its first match and never refuses. The patterns of one write share one
//! [`Scan`]: a bound on the bytes they read, and a cache per pattern.

use std::borrow::Cow;

use super::{DecisionField, Limit, Quality, QualityFault, WordRange, words};
use crate::store::limits::MAX_QUOTED_MATCH_BYTES;
use crate::store::schema::{Anchor, Decision};
use crate::store::text::{TextFault, TextField, fault};

mod audit;
mod pattern;

pub use audit::Shortfall;
pub(crate) use pattern::Scan;
use pattern::{Compiled, searched};
pub(crate) use regex_automata::hybrid::regex::Cache as PatternCache;

/// The rules of one `[quality]` table, its patterns compiled.
#[derive(Debug)]
pub(crate) struct Bar {
    reason_words: WordRange,
    alternatives_max: Option<usize>,
    alternative_words_max: Option<usize>,
    anchors_max: Option<usize>,
    require_origin: bool,
    patterns: Vec<Compiled>,
}

/// A pattern the lazy DFA does not run: its index and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternFault {
    pub index: usize,
    pub message: String,
}

/// A field of a decision, and the element of a list field by its position
/// in the decision's list: the op that judged the decision names the
/// argument that brought it (`apply::check`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Place {
    pub(crate) field: DecisionField,
    pub(crate) index: Option<usize>,
}

impl Place {
    const fn whole(field: DecisionField) -> Self {
        Self { field, index: None }
    }

    const fn element(field: DecisionField, index: usize) -> Self {
        Self {
            field,
            index: Some(index),
        }
    }
}

/// What stops a write's judgement: a text rule or a rule of the bar the
/// decision breaks, or the bytes its patterns may read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Broken {
    Text {
        field: TextField,
        fault: TextFault,
    },
    Quality(QualityFault),
    /// The patterns would read past `bound` bytes over the write.
    Work {
        bound: usize,
    },
}

/// The first rule a judged decision breaks, and where.
#[derive(Debug)]
pub(crate) struct Breach {
    pub(crate) place: Place,
    pub(crate) broken: Broken,
}

/// A `warn` pattern's first match: the pattern's index in `[quality]
/// patterns` and the byte offset of the match in its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Hit {
    pub(crate) pattern: usize,
    pub(crate) at: usize,
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

    /// An empty cache for each pattern, in their order.
    pub(crate) fn caches(&self) -> Vec<PatternCache> {
        self.patterns.iter().map(Compiled::cache).collect()
    }

    /// What one write's patterns may read and build: `bound` bytes, an
    /// empty cache for each, and `cache_bound` bytes the caches may grow by
    /// together.
    pub(crate) fn scan(&self, bound: usize, cache_bound: usize) -> Scan {
        Scan {
            caches: self.caches(),
            left: bound,
            bound,
            cache_left: cache_bound,
        }
    }

    /// `decision` against the bar in the fields of `held`: the first rule
    /// it breaks, or the warnings of the `warn` patterns that match, each
    /// with where it matched. The text rules and the limits come first, in
    /// the order of the file's keys, then the patterns over `scan`.
    pub(crate) fn judge(
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
    /// Past the bound the write is refused, as it is when a cache gives up
    /// or the growth of the caches passes their total: then at most one
    /// pattern's two caches past it.
    pub(crate) fn judge_patterns(
        &self,
        texts: &[(Place, Cow<'_, str>)],
        scan: &mut Scan,
    ) -> Result<Vec<(Place, Hit)>, Breach> {
        let Scan {
            caches,
            left,
            bound,
            cache_left,
        } = scan;
        let mut hits = Vec::new();
        for (index, (pattern, cache)) in self.patterns.iter().zip(caches).enumerate() {
            let read: Vec<(Place, &str)> = texts
                .iter()
                .filter(|(place, _)| pattern.fields.contains(&place.field))
                .map(|(place, text)| (*place, text.as_ref()))
                .collect();
            let (Some(&(first, _)), Some(&(last, _))) = (read.first(), read.last()) else {
                continue;
            };
            let bytes = searched(read.iter().map(|&(_, text)| text));
            *left = left.checked_sub(bytes).ok_or(Breach {
                place: first,
                broken: Broken::Work { bound: *bound },
            })?;
            let before = cache.memory_usage();
            let found = pattern
                .first_match(cache, read)
                .map_err(|place| gave_up(index, place))?;
            let grown = cache.memory_usage().saturating_sub(before);
            let read_to = found.as_ref().map_or(last, |&(place, _)| place);
            *cache_left = (cache_left.checked_sub(grown)).ok_or(gave_up(index, read_to))?;
            if let Some((place, found)) = found {
                hits.push((place, pattern.verdict(index, place, &found)?));
            }
        }
        Ok(hits)
    }

    /// The batch's `origin` against the patterns that read it, over `scan`:
    /// the first `refuse` match, or the `warn` matches.
    pub(crate) fn judge_origin(
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

    /// `anchor` against the patterns, as the anchors field of a decision
    /// that holds it alone, over `scan`: the first `refuse` match, or the
    /// `warn` matches.
    pub(crate) fn judge_anchor(
        &self,
        anchor: &Anchor,
        scan: &mut Scan,
    ) -> Result<Vec<Hit>, Breach> {
        let place = Place::element(DecisionField::Anchors, 0);
        let hits = self.judge_patterns(&[(place, Cow::Owned(anchor.to_string()))], scan)?;
        Ok(hits.into_iter().map(|(_, hit)| hit).collect())
    }

    /// The message of the pattern at `index` of `[quality] patterns`.
    pub(crate) fn message(&self, index: usize) -> Option<&str> {
        self.patterns
            .get(index)
            .map(|pattern| pattern.message.as_str())
    }

    /// The batch's `origin` against `require_origin`, when the batch
    /// `records` a decision.
    pub(crate) fn origin_required(
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
    fn checks(&self, decision: &Decision, field: DecisionField) -> Vec<Result<(), Breach>> {
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

/// The breach of the pattern at `index` whose cache gave up at `place`.
const fn gave_up(index: usize, place: Place) -> Breach {
    Breach {
        place,
        broken: Broken::Quality(QualityFault::GaveUp { index }),
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

/// `found`, cut past [`MAX_QUOTED_MATCH_BYTES`] at a character boundary.
fn quoted(found: &str) -> String {
    if found.len() <= MAX_QUOTED_MATCH_BYTES {
        return found.to_owned();
    }
    let end = (0..=MAX_QUOTED_MATCH_BYTES)
        .rev()
        .find(|&end| found.is_char_boundary(end))
        .unwrap_or(0);
    format!("{}...", found.get(..end).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::limits::{MAX_PATTERN_CACHE_BYTES, MAX_PATTERN_COMPILED_BYTES};
    use crate::store::quality::{Level, Pattern};
    use crate::store::testing::{OUTGROWS_ITS_CACHE, coin_flips, sample_decision};

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
        let mut scan = bar.scan(bound, usize::MAX);
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
                r"\w{8}",
                format!("given cache capacity ({MAX_PATTERN_CACHE_BYTES})"),
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
        let ascii = Bar::compile(&quality(&[r"(?-u:\b)we(?-u:\b)", r"\w{3}y"]));
        assert!(ascii.is_ok());
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
    /// text, before it reads them: a write that may read exactly that
    /// passes, one byte less is refused at the pattern's first text.
    #[test]
    fn a_pattern_charges_its_texts_before_it_reads_them() {
        let bar = Bar::compile(&quality(&["zz", "yy"])).unwrap();
        let mut decision = sample_decision("project");
        decision.choice = "Keep zz".into();
        decision.reason = "Because it holds.".into();
        let bytes = 2 * ("Keep zz".len() + 1 + "Because it holds.".len() + 1);

        let hits = judged(&bar, &decision, bytes).unwrap();
        let refused = judged(&bar, &decision, bytes - 1);

        assert_eq!(hits.len(), 1);
        assert_eq!(refused, Err(Broken::Work { bound: bytes - 1 }));
    }

    /// A search whose automaton outgrows its cache gives up instead of
    /// clearing it, and the write is refused at the text, naming the
    /// pattern. A fresh cache judges a short text.
    #[test]
    fn a_pattern_whose_cache_fills_gives_up_naming_its_index() {
        let bar = Bar::compile(&quality(&["ok", OUTGROWS_ITS_CACHE])).unwrap();
        let mut decision = sample_decision("project");
        decision.reason = coin_flips(4_000, 7);
        let mut scan = bar.scan(usize::MAX, usize::MAX);
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

    #[test]
    fn a_long_match_is_quoted_cut_at_a_character_boundary() {
        let long = format!("{}\u{17c}tail", "a".repeat(MAX_QUOTED_MATCH_BYTES - 1));
        let shown = quoted(&long);
        assert_eq!(
            shown,
            format!("{}...", "a".repeat(MAX_QUOTED_MATCH_BYTES - 1))
        );
        assert_eq!(quoted("short"), "short");
    }
}
