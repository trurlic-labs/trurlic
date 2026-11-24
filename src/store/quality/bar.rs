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
use std::fmt;

use regex::{Regex, RegexBuilder};

use super::{DecisionField, Level, Limit, PatternMatch, Quality, QualityFault, WordRange, words};
use crate::store::limits::{MAX_PATTERN_COMPILED_BYTES, MAX_QUOTED_MATCH_BYTES};
use crate::store::schema::Decision;
use crate::store::text::{TextFault, TextField, fault};

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
    fields: BTreeSet<DecisionField>,
    level: Level,
    message: String,
}

/// A pattern that does not compile: its index and regex's message.
#[derive(Debug)]
pub(crate) struct PatternFault {
    pub(crate) index: usize,
    pub(crate) message: String,
}

/// A field of a decision, and the element of a list field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Place {
    pub(crate) field: DecisionField,
    pub(crate) index: Option<usize>,
}

impl Place {
    const fn whole(field: DecisionField) -> Self {
        Self { field, index: None }
    }
}

impl fmt::Display for Place {
    /// `reason`, `alternatives[2]`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.field.as_str())?;
        match self.index {
            Some(index) => write!(f, "[{index}]"),
            None => Ok(()),
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

/// A `warn` pattern's match, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Hit {
    pub(crate) place: Place,
    pub(crate) found: PatternMatch,
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
                let regex = RegexBuilder::new(&pattern.pattern)
                    .size_limit(MAX_PATTERN_COMPILED_BYTES)
                    .dfa_size_limit(MAX_PATTERN_COMPILED_BYTES)
                    .build()
                    .map_err(|e| PatternFault {
                        index,
                        message: e.to_string(),
                    })?;
                // clone: the bar outlives the borrow of the graph a batch
                // changes, so it owns each pattern's fields and message.
                let (fields, message) = (pattern.fields.clone(), pattern.message.clone());
                Ok(Compiled {
                    regex,
                    fields,
                    level: pattern.level,
                    message,
                })
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            reason_words: quality.reason_words,
            alternatives_max: quality.alternatives_max,
            alternative_words_max: quality.alternative_words_max,
            anchors_max: quality.anchors_max,
            require_origin: quality.require_origin,
            patterns,
        })
    }

    /// `decision` against the bar in the fields of `held`: the first rule
    /// it breaks, or the warnings of the `warn` patterns that match.
    pub(crate) fn judge(
        &self,
        decision: &Decision,
        held: &[DecisionField],
    ) -> Result<Vec<Hit>, Breach> {
        for field in DecisionField::IN_FILE_ORDER {
            if held.contains(&field) {
                self.limits(decision, field)?;
            }
        }
        let mut hits = Vec::new();
        for (index, pattern) in self.patterns.iter().enumerate() {
            let texts = DecisionField::IN_FILE_ORDER
                .into_iter()
                .filter(|field| held.contains(field) && pattern.fields.contains(field))
                .flat_map(|field| texts(decision, field));
            if let Some(hit) = pattern.first_match(index, texts) {
                pattern.verdict(hit, &mut hits)?;
            }
        }
        Ok(hits)
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
            if let Some(hit) = pattern.first_match(index, [(place, Cow::Borrowed(origin))]) {
                pattern.verdict(hit, &mut hits)?;
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
                    let place = Place {
                        field,
                        index: Some(index),
                    };
                    let measured = words(alternative);
                    at_most(
                        place,
                        Limit::AlternativeWordsMax,
                        measured,
                        self.alternative_words_max,
                    )?;
                }
                Ok(())
            }
            DecisionField::Anchors => {
                let count = decision.anchors.len();
                at_least(place, Limit::AnchorRequired, count, Some(1))?;
                at_most(place, Limit::AnchorsMax, count, self.anchors_max)
            }
            // No limit counts tags; the origin is the batch's.
            DecisionField::Tags | DecisionField::Origin => Ok(()),
        }
    }
}

impl Compiled {
    /// The first of `texts` this pattern matches, as a warning would name it.
    fn first_match<'t>(
        &self,
        index: usize,
        texts: impl IntoIterator<Item = (Place, Cow<'t, str>)>,
    ) -> Option<Hit> {
        texts.into_iter().find_map(|(place, text)| {
            let found = self.regex.find(&text)?;
            Some(Hit {
                place,
                found: PatternMatch {
                    index,
                    at: found.start(),
                    found: quoted(found.as_str()),
                    // clone: each match carries the message it reports.
                    message: self.message.clone(),
                },
            })
        })
    }

    /// Refuse for `hit` at `refuse`, else keep it among `hits`.
    fn verdict(&self, hit: Hit, hits: &mut Vec<Hit>) -> Result<(), Breach> {
        match self.level {
            Level::Refuse => Err(Breach {
                place: hit.place,
                broken: Broken::Quality(QualityFault::Pattern(hit.found)),
            }),
            Level::Warn => {
                hits.push(hit);
                Ok(())
            }
        }
    }
}

/// The texts of `field` a pattern reads, each with its place: an anchor
/// as `file::symbol`.
fn texts(decision: &Decision, field: DecisionField) -> Vec<(Place, Cow<'_, str>)> {
    let each = |index: usize| Place {
        field,
        index: Some(index),
    };
    match field {
        DecisionField::Choice => vec![(Place::whole(field), Cow::Borrowed(&*decision.choice))],
        DecisionField::Reason => vec![(Place::whole(field), Cow::Borrowed(&*decision.reason))],
        DecisionField::Alternatives => decision
            .alternatives
            .iter()
            .enumerate()
            .map(|(index, text)| (each(index), Cow::Borrowed(text.as_str())))
            .collect(),
        DecisionField::Tags => decision
            .tags
            .iter()
            .enumerate()
            .map(|(index, text)| (each(index), Cow::Borrowed(text.as_str())))
            .collect(),
        DecisionField::Anchors => decision
            .anchors
            .iter()
            .enumerate()
            .map(|(index, anchor)| (each(index), Cow::Owned(anchor.to_string())))
            .collect(),
        // The batch's origin is judged once, by `judge_origin`.
        DecisionField::Origin => Vec::new(),
    }
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
        assert_eq!(hits[0].found.at, 5);
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
