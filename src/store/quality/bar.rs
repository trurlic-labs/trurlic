//! The bar compiled for one batch, and its judgement of each decision an
//! op writes.
//!
//! A decision is held to the bar only in the fields the op writes, so a
//! field below the bar that an op leaves as it is stays as it was. Within
//! them the limits come first, field by field in the order of the file's
//! keys, then the patterns in the order `[quality]` lists them; the first
//! rule broken refuses the decision. A `warn` pattern gives one warning at
//! its first match and never refuses.

use std::borrow::Cow;
use std::collections::BTreeSet;

use regex::{Match, Regex, RegexBuilder};
use regex_automata::nfa::thompson;

use super::{DecisionField, Level, Limit, PatternMatch, Quality, QualityFault, WordRange, words};
use crate::store::limits::{MAX_PATTERN_COMPILED_BYTES, MAX_QUOTED_MATCH_BYTES};
use crate::store::schema::Decision;
use crate::store::text::{TextFault, TextField, fault};

mod audit;

pub use audit::Shortfall;

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

#[derive(Debug)]
struct Compiled {
    regex: Regex,
    /// The states of the pattern's Thompson NFA: what its slowest engine
    /// walks per byte, at worst.
    states: usize,
    fields: BTreeSet<DecisionField>,
    level: Level,
    message: String,
}

/// A pattern that does not compile: its index and regex's message.
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

/// A rule a decision breaks: a text rule or one of the bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Broken {
    Text { field: TextField, fault: TextFault },
    Quality(QualityFault),
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
    /// `quality` with each pattern compiled within
    /// [`MAX_PATTERN_COMPILED_BYTES`], or the first that does not compile.
    pub(super) fn compile(quality: &Quality) -> Result<Self, PatternFault> {
        let patterns = quality
            .patterns
            .iter()
            .enumerate()
            .map(|(index, pattern)| {
                let fault = |message: String| PatternFault { index, message };
                let regex = RegexBuilder::new(&pattern.pattern)
                    .size_limit(MAX_PATTERN_COMPILED_BYTES)
                    .dfa_size_limit(MAX_PATTERN_COMPILED_BYTES)
                    .build()
                    .map_err(|e| fault(e.to_string()))?;
                let states = nfa_states(&pattern.pattern).map_err(fault)?;
                // clone: the bar outlives the borrow of the graph a batch
                // changes, so it owns each pattern's fields and message.
                let (fields, message) = (pattern.fields.clone(), pattern.message.clone());
                Ok(Compiled {
                    regex,
                    states,
                    fields,
                    level: pattern.level,
                    message,
                })
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

    /// `decision` against the bar in the fields of `held`: the first rule
    /// it breaks, or the warnings of the `warn` patterns that match, each
    /// with where it matched.
    pub(crate) fn judge(
        &self,
        decision: &Decision,
        held: &[DecisionField],
    ) -> Result<Vec<(Place, Hit)>, Breach> {
        for field in DecisionField::IN_FILE_ORDER {
            if held.contains(&field) {
                self.limits(decision, field)?;
            }
        }
        if self.patterns.is_empty() {
            return Ok(Vec::new());
        }
        // Built once for every pattern to read: an anchor's text is rendered.
        let texts: Vec<(Place, Cow<'_, str>)> = DecisionField::IN_FILE_ORDER
            .into_iter()
            .filter(|field| held.contains(field))
            .flat_map(|field| texts(decision, field))
            .collect();
        let mut hits = Vec::new();
        for (index, pattern) in self.patterns.iter().enumerate() {
            let read = texts
                .iter()
                .filter(|(place, _)| pattern.fields.contains(&place.field))
                .map(|(place, text)| (*place, text.as_ref()));
            if let Some((place, found)) = pattern.first_match(read) {
                hits.push((place, pattern.verdict(index, place, &found)?));
            }
        }
        Ok(hits)
    }

    /// The message of the pattern at `index` of `[quality] patterns`.
    pub(crate) fn message(&self, index: usize) -> Option<&str> {
        self.patterns
            .get(index)
            .map(|pattern| pattern.message.as_str())
    }

    /// The batch's `origin` against `require_origin`, when the batch
    /// `records` a decision, and against the patterns that read it.
    pub(crate) fn judge_origin(
        &self,
        origin: Option<&str>,
        records: bool,
    ) -> Result<Vec<Hit>, Breach> {
        let place = Place::whole(DecisionField::Origin);
        let Some(origin) = origin else {
            if records && self.require_origin {
                return Err(Breach {
                    place,
                    broken: Broken::Quality(QualityFault::OriginRequired),
                });
            }
            return Ok(Vec::new());
        };
        let mut hits = Vec::new();
        for (index, pattern) in self.patterns.iter().enumerate() {
            if !pattern.fields.contains(&DecisionField::Origin) {
                continue;
            }
            if let Some((place, found)) = pattern.first_match([(place, origin)]) {
                hits.push(pattern.verdict(index, place, &found)?);
            }
        }
        Ok(hits)
    }

    /// The text rules and the limits of `field`.
    fn limits(&self, decision: &Decision, field: DecisionField) -> Result<(), Breach> {
        let place = Place::whole(field);
        match field {
            DecisionField::Choice => text(place, TextField::Choice, &decision.choice),
            DecisionField::Reason => {
                text(place, TextField::Reason, &decision.reason)?;
                let measured = words(&decision.reason);
                let WordRange { min, max } = self.reason_words;
                at_least(place, Limit::ReasonWordsMin, measured, min)?;
                at_most(place, Limit::ReasonWordsMax, measured, max)
            }
            DecisionField::Alternatives => {
                let count = decision.alternatives.len();
                at_least(place, Limit::AlternativeRequired, count, Some(1))?;
                at_most(place, Limit::AlternativesMax, count, self.alternatives_max)?;
                for (index, alternative) in decision.alternatives.iter().enumerate() {
                    let place = Place::element(field, index);
                    text(place, TextField::Alternative, alternative)?;
                    let (limit, bound) = (Limit::AlternativeWordsMax, self.alternative_words_max);
                    at_most(place, limit, words(alternative), bound)?;
                }
                Ok(())
            }
            DecisionField::Anchors => {
                let count = decision.anchors.len();
                at_least(place, Limit::AnchorRequired, count, Some(1))?;
                at_most(place, Limit::AnchorsMax, count, self.anchors_max)
            }
            DecisionField::Tags => {
                for (index, tag) in decision.tags.iter().enumerate() {
                    text(Place::element(field, index), TextField::Tag, tag)?;
                }
                Ok(())
            }
            // The origin is the batch's, judged once (`judge_origin`) and
            // held to the text rules with the batch's trailers.
            DecisionField::Origin => Ok(()),
        }
    }
}

impl Compiled {
    /// The match `found` of this pattern, the one at `index`, as a fault
    /// names it.
    fn matched(&self, index: usize, found: &Match<'_>) -> PatternMatch {
        PatternMatch {
            index,
            at: found.start(),
            found: quoted(found.as_str()),
            // clone: the fault carries the message it reports.
            message: self.message.clone(),
        }
    }

    /// The first of `texts` this pattern matches, and its place.
    fn first_match<'t>(
        &self,
        texts: impl IntoIterator<Item = (Place, &'t str)>,
    ) -> Option<(Place, Match<'t>)> {
        texts
            .into_iter()
            .find_map(|(place, text)| Some((place, self.regex.find(text)?)))
    }

    /// The match `found` at `place` of this pattern, the one at `index`:
    /// the refusal at `refuse`, else the warning.
    fn verdict(&self, index: usize, place: Place, found: &Match<'_>) -> Result<Hit, Breach> {
        match self.level {
            Level::Refuse => Err(Breach {
                place,
                broken: Broken::Quality(QualityFault::Pattern(self.matched(index, found))),
            }),
            Level::Warn => Ok(Hit {
                pattern: index,
                at: found.start(),
            }),
        }
    }
}

/// The states of the Thompson NFA of `pattern`, built as the `regex` crate
/// builds its own (the same syntax, Unicode on, within the same size), or
/// why it does not build.
fn nfa_states(pattern: &str) -> Result<usize, String> {
    let config = thompson::Config::new().nfa_size_limit(Some(MAX_PATTERN_COMPILED_BYTES));
    let nfa = thompson::Compiler::new()
        .configure(config)
        .build(pattern)
        .map_err(|e| e.to_string())?;
    Ok(nfa.states().len())
}

/// The texts of `field` a pattern reads, each with its place: an anchor
/// as `file::symbol`.
fn texts(decision: &Decision, field: DecisionField) -> Vec<(Place, Cow<'_, str>)> {
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
    use crate::store::quality::Pattern;

    fn quality(patterns: &[&str]) -> Quality {
        Quality {
            patterns: patterns
                .iter()
                .map(|&pattern| Pattern {
                    pattern: pattern.into(),
                    fields: [DecisionField::Choice].into(),
                    level: Level::Warn,
                    message: "M".into(),
                })
                .collect(),
            ..Quality::default()
        }
    }

    /// A short pattern can ask for a large automaton: it is refused at the
    /// stated size, with its index, before it costs a write more.
    #[test]
    fn a_pattern_past_its_compiled_size_is_refused_with_its_index() {
        let fault = Bar::compile(&quality(&["ok", r"\w{50}"])).unwrap_err();
        assert_eq!(fault.index, 1);
        assert!(
            fault
                .message
                .contains(&format!("size limit of {MAX_PATTERN_COMPILED_BYTES} bytes")),
            "{}",
            fault.message
        );
        assert!(Bar::compile(&quality(&[r"\w{5}"])).is_ok());
    }

    /// Word classes and case folding are Unicode's: `\b` does not split a
    /// word at a letter outside ASCII.
    #[test]
    fn patterns_read_text_in_any_script() {
        let bar = Bar::compile(&quality(&[r"(?i)\bwe\b"])).unwrap();
        let mut decision = crate::store::testing::sample_decision("project");
        let choice = [DecisionField::Choice];
        decision.choice = "Bierzemy we\u{17a}miemy".into();
        assert_eq!(
            bar.judge(&decision, &choice).map_err(|b| b.broken),
            Ok(vec![])
        );
        decision.choice = "Then WE lock".into();
        let hits = bar.judge(&decision, &choice).map_err(|b| b.broken).unwrap();
        assert_eq!(hits[0].1.at, 5);
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
