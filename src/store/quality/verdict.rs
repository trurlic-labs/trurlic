//! What a judgement of the bar finds: the rule a decision breaks and
//! where ([`Breach`]), with what it measured ([`QualityFault`]), and the
//! first match of a `warn` pattern ([`Hit`]).

use std::fmt;

use crate::store::model::DecisionField;
use crate::store::model::limits::{MAX_PATTERN_CACHE_BYTES, MAX_PATTERN_CACHE_TOTAL_BYTES};
use crate::store::model::{TextFault, TextField};

/// A count the bar bounds: built in, or a key of `[quality]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Limit {
    /// At least one alternative, built in: a choice with no rejected
    /// option was no decision.
    AlternativeRequired,
    /// At least one anchor, built in: a decision applies somewhere.
    AnchorRequired,
    ReasonWordsMin,
    ReasonWordsMax,
    AlternativesMax,
    AlternativeWordsMax,
    AnchorsMax,
}

impl Limit {
    /// The rule as a refusal names it.
    #[must_use]
    const fn as_str(self) -> &'static str {
        match self {
            Self::AlternativeRequired => "alternative_required",
            Self::AnchorRequired => "anchor_required",
            Self::ReasonWordsMin => "reason_words_min",
            Self::ReasonWordsMax => "reason_words_max",
            Self::AlternativesMax => "alternatives_max",
            Self::AlternativeWordsMax => "alternative_words_max",
            Self::AnchorsMax => "anchors_max",
        }
    }

    /// What the limit counts.
    #[must_use]
    pub(crate) const fn unit(self) -> &'static str {
        match self {
            Self::AlternativeRequired | Self::AlternativesMax => "alternatives",
            Self::AnchorRequired | Self::AnchorsMax => "anchors",
            Self::ReasonWordsMin | Self::ReasonWordsMax | Self::AlternativeWordsMax => "words",
        }
    }

    const fn is_minimum(self) -> bool {
        match self {
            Self::AlternativeRequired | Self::AnchorRequired | Self::ReasonWordsMin => true,
            Self::ReasonWordsMax
            | Self::AlternativesMax
            | Self::AlternativeWordsMax
            | Self::AnchorsMax => false,
        }
    }

    /// Where the limit is set.
    const fn source(self) -> &'static str {
        match self {
            Self::AlternativeRequired | Self::AnchorRequired => "built in",
            Self::ReasonWordsMin => "`[quality] reason_words.min`",
            Self::ReasonWordsMax => "`[quality] reason_words.max`",
            Self::AlternativesMax => "`[quality] alternatives_max`",
            Self::AlternativeWordsMax => "`[quality] alternative_words_max`",
            Self::AnchorsMax => "`[quality] anchors_max`",
        }
    }
}

/// A rule of the bar a write breaks, with what it measured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum QualityFault {
    /// `measured` in the limit's unit, against `bound`.
    Limit {
        limit: Limit,
        measured: usize,
        bound: usize,
    },
    /// A `refuse` pattern matched.
    Pattern(PatternMatch),
    /// The pattern at `index` of `[quality] patterns` gave up, whatever its
    /// level: its automaton outgrew its cache over the texts the call read
    /// so far, so it judged no text after this one.
    GaveUp { index: usize },
    /// The automata of all the patterns grew past the total one call may
    /// build over this text; the pattern at `largest` grew the most.
    CacheTotal { largest: usize },
    /// The batch records a decision without an `origin`, and
    /// `require_origin` is set.
    OriginRequired,
}

impl QualityFault {
    /// The rule as a refusal names it.
    #[must_use]
    pub(crate) const fn as_str(&self) -> &'static str {
        match self {
            Self::Limit { limit, .. } => limit.as_str(),
            Self::Pattern(_) => "pattern",
            Self::GaveUp { .. } => "pattern_gave_up",
            Self::CacheTotal { .. } => "pattern_cache_total",
            Self::OriginRequired => "origin_required",
        }
    }
}

/// What fixes a cache that gave up or the caches past their total: less
/// text over one call, or a pattern that builds fewer states.
const REMEDY: &str = "shorten the text, split the batch, or simplify";

/// A `measured` count past the `bound` of `limit`, with its unit and its
/// source.
fn past(f: &mut fmt::Formatter<'_>, limit: &Limit, measured: usize, bound: usize) -> fmt::Result {
    let side = if limit.is_minimum() {
        "under the minimum"
    } else {
        "over the maximum"
    };
    let (unit, source) = (limit.unit(), limit.source());
    write!(f, "{measured} {unit}, {side} of {bound} ({source})")
}

impl fmt::Display for QualityFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Limit {
                limit,
                measured,
                bound,
            } => past(f, limit, *measured, *bound),
            Self::Pattern(found) => write!(f, "{found}"),
            Self::GaveUp { index } => write!(
                f,
                "`[quality] patterns[{index}]` gave up here: its automaton outgrew the \
                 {MAX_PATTERN_CACHE_BYTES} bytes of cache one pattern may fill; {REMEDY} the \
                 pattern"
            ),
            Self::CacheTotal { largest } => write!(
                f,
                "the `[quality]` patterns' automata grew here past the \
                 {MAX_PATTERN_CACHE_TOTAL_BYTES} bytes of cache one write may build, \
                 `[quality] patterns[{largest}]` the most; {REMEDY} that pattern"
            ),
            Self::OriginRequired => f.write_str(
                "is required: the batch records a decision, and `[quality] require_origin` \
                 is set",
            ),
        }
    }
}

/// Where a pattern matched, and what it says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PatternMatch {
    /// The pattern's index in `[quality] patterns`.
    pub(crate) index: usize,
    /// The byte offset of the match in the text.
    pub(crate) at: usize,
    /// The matched text, cut at a character boundary past
    /// `MAX_QUOTED_MATCH_BYTES`.
    pub(crate) found: String,
    pub(crate) message: String,
}

impl fmt::Display for PatternMatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "matches `[quality] patterns[{}]` at byte {} (`{}`): {}",
            self.index, self.at, self.found, self.message
        )
    }
}

/// A field of a decision, and the element of a list field by its position
/// in the decision's list: the op that judged the decision names the
/// argument that brought it (`apply::check`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::store) struct Place {
    pub(in crate::store) field: DecisionField,
    pub(super) index: Option<usize>,
}

impl Place {
    pub(super) const fn whole(field: DecisionField) -> Self {
        Self { field, index: None }
    }

    pub(super) const fn element(field: DecisionField, index: usize) -> Self {
        Self {
            field,
            index: Some(index),
        }
    }
}

/// What stops a write's judgement: a text rule or a rule of the bar the
/// decision breaks, or the bytes its patterns may read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::store) enum Broken {
    Text {
        field: TextField,
        fault: TextFault,
    },
    Quality(QualityFault),
    /// The patterns would read past `MAX_WRITE_PATTERN_BYTES` over the
    /// write.
    Work,
}

/// The first rule a judged decision breaks, and where.
#[derive(Debug)]
pub(in crate::store) struct Breach {
    pub(in crate::store) place: Place,
    pub(in crate::store) broken: Broken,
}

/// A `warn` pattern's first match: the pattern's index in `[quality]
/// patterns` and the byte offset of the match in its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::store) struct Hit {
    pub(in crate::store) pattern: usize,
    pub(in crate::store) at: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_limit_states_the_measured_value_the_bound_and_where_it_is_set() {
        let fault = QualityFault::Limit {
            limit: Limit::ReasonWordsMax,
            measured: 190,
            bound: 180,
        };
        assert_eq!(fault.as_str(), "reason_words_max");
        assert_eq!(
            fault.to_string(),
            "190 words, over the maximum of 180 (`[quality] reason_words.max`)"
        );
        let fault = QualityFault::Limit {
            limit: Limit::AnchorRequired,
            measured: 0,
            bound: 1,
        };
        assert_eq!(
            fault.to_string(),
            "0 anchors, under the minimum of 1 (built in)"
        );
    }
}
