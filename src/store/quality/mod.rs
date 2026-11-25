//! The quality bar a write holds a decision to: the built-in rules and the
//! project's `[quality]` table.
//!
//! Two kinds of rule govern a decision's text and shape. What a file must
//! hold to be read as written (`store::text`: a non-blank choice on one
//! line within 200 bytes, a reason of real text, no invisible, reordering
//! or control character, no tool-call markup) is a property of every graph:
//! validation reports each, so `check` fails on a hand edit that breaks
//! one. The bar is a property of a write: at least one alternative and one
//! anchor, and the project's limits and patterns. A graph migrated or edited
//! by hand below it loads, passes `check` and stays revisable, since an op
//! is held to the bar only in the fields it writes. A write holds each
//! decision an op writes to the text rules and the limits as the op runs
//! ([`Bar::judge_limits`], field by field in one order), and to the
//! patterns once their work over the batch is charged
//! ([`Bar::judge_patterns`]).
//!
//! Words are counted one way everywhere: the runs of non-whitespace
//! between Unicode whitespace ([`words`]). Bytes are bytes of UTF-8.

mod bar;
mod config;

use std::fmt;

use serde::Deserialize;

use crate::store::Store;
use crate::{Error, Result};

pub(crate) use bar::{Bar, Broken, Hit, Place, element};
pub use bar::{PatternFault, Shortfall};
pub use config::{ConfigFault, Level, Pattern, Quality, WordRange};

/// The fields of a decision the bar reads, and the names a pattern's
/// `fields` gives them. `origin` is the batch's, read once per batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionField {
    Alternatives,
    Anchors,
    Choice,
    Origin,
    Reason,
    Tags,
}

impl DecisionField {
    /// The fields of a decision file, in the order of its keys: the order
    /// the bar reads them in.
    pub const IN_FILE_ORDER: [Self; 6] = [
        Self::Choice,
        Self::Reason,
        Self::Alternatives,
        Self::Tags,
        Self::Origin,
        Self::Anchors,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Alternatives => "alternatives",
            Self::Anchors => "anchors",
            Self::Choice => "choice",
            Self::Origin => "origin",
            Self::Reason => "reason",
            Self::Tags => "tags",
        }
    }
}

/// A count the bar bounds: built in, or a key of `[quality]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limit {
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
    pub const fn as_str(self) -> &'static str {
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
    pub const fn unit(self) -> &'static str {
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
pub enum QualityFault {
    /// `measured` in the limit's unit, against `bound`.
    Limit {
        limit: Limit,
        measured: usize,
        bound: usize,
    },
    /// A `refuse` pattern matched.
    Pattern(PatternMatch),
    /// The batch records a decision without an `origin`, and
    /// `require_origin` is set.
    OriginRequired,
}

impl QualityFault {
    /// The rule as a refusal names it.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Limit { limit, .. } => limit.as_str(),
            Self::Pattern(_) => "pattern",
            Self::OriginRequired => "origin_required",
        }
    }
}

impl fmt::Display for QualityFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Limit {
                limit,
                measured,
                bound,
            } => {
                let side = if limit.is_minimum() {
                    "under the minimum"
                } else {
                    "over the maximum"
                };
                write!(
                    f,
                    "{measured} {}, {side} of {bound} ({})",
                    limit.unit(),
                    limit.source()
                )
            }
            Self::Pattern(found) => write!(f, "{found}"),
            Self::OriginRequired => f.write_str(
                "is required: the batch records a decision, and `[quality] require_origin` \
                 is set",
            ),
        }
    }
}

/// Where a pattern matched, and what it says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternMatch {
    /// The pattern's index in `[quality] patterns`.
    pub index: usize,
    /// The byte offset of the match in the text.
    pub at: usize,
    /// The matched text, cut at a character boundary past
    /// `MAX_QUOTED_MATCH_BYTES`.
    pub found: String,
    pub message: String,
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

/// The words of `text`: its runs of non-whitespace, split at every Unicode
/// whitespace character. A hyphenated or slashed run is one word.
#[must_use]
fn words(text: &str) -> usize {
    text.split_whitespace().count()
}

impl Store {
    /// The rules of `quality` compiled for a write: every pattern, once per
    /// batch. A pattern that does not compile, or compiles past
    /// `MAX_PATTERN_COMPILED_BYTES`, is [`Error::QualityPattern`].
    pub(crate) fn bar(&self, quality: &Quality) -> Result<Bar> {
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
    use crate::store::canonical::{canonical, is_canonical};
    use crate::store::schema::{Anchor, Decision};
    use crate::store::testing::{sample_decision, setup_store};

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
        assert_eq!(
            bar.judge(&at_the_bar(), &all).map_err(|b| b.broken),
            Ok(vec![])
        );
        let mut we = at_the_bar();
        we.choice = "We take the file lock".into();
        assert!(bar.judge(&we, &all).is_err());

        let written = canonical(&state.project);
        assert!(!is_canonical(&state.project, header.as_bytes()));
        store.begin_write().unwrap().format().unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), written);
        assert_eq!(store.load_state().unwrap().project, state.project);
        assert!(
            written.contains("pattern = '(?i)\\b(?:we|our)\\b'\n"),
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
