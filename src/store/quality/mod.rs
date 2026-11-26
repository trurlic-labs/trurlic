//! The quality bar a write holds a decision to: the built-in rules and the
//! project's `[quality]` table.
//!
//! Two kinds of rule govern a decision's text and shape. What a file must
//! hold to be read as written (`store::model::text`: a non-blank choice on one
//! line within 200 bytes, a reason of real text, no invisible, reordering
//! or control character, no tool-call markup) is a property of every graph:
//! validation reports each, so `check` fails on a hand edit that breaks
//! one. The bar is a property of a write: at least one alternative and one
//! anchor, and the project's limits and patterns. A graph migrated or edited
//! by hand below it loads, passes `check` and stays revisable, since an op
//! is held to the bar only in the fields it writes. A write holds each
//! decision an op writes to the text rules, the limits and the patterns as
//! the op runs, in one order ([`Bar::judge`]).
//!
//! Words are counted one way everywhere: the runs of non-whitespace
//! between Unicode whitespace ([`words`]). Bytes are bytes of UTF-8.

mod bar;

use std::fmt;

use crate::store::format::Store;
use crate::store::model::Quality;
use crate::store::model::limits::{MAX_PATTERN_CACHE_BYTES, MAX_PATTERN_CACHE_TOTAL_BYTES};
use crate::{Error, Result};

pub(crate) use bar::Shortfall;
pub(crate) use bar::{Bar, Breach, Broken, Hit, Place, Scan, element, texts};

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
    pub(crate) const fn as_str(self) -> &'static str {
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
    /// The bar, when it was compiled from `quality`.
    pub(crate) fn of(&self, quality: &Quality) -> Option<&Bar> {
        (self.quality == *quality).then_some(&self.bar)
    }
}

impl Store {
    /// The rules of `quality` compiled for a write: every pattern, once per
    /// batch. A pattern the lazy DFA does not run is
    /// [`Error::QualityPattern`].
    pub(crate) fn bar(&self, quality: &Quality) -> Result<Bar> {
        Bar::compile(quality).map_err(|fault| Error::QualityPattern {
            path: self.project_path(),
            index: fault.index,
            message: fault.message,
        })
    }

    /// The bar of the `[quality]` the store holds now, compiled holding no
    /// lock for the write that follows ([`Writer::apply`](crate::store::Writer::apply)).
    /// A pattern that does not compile is [`Error::QualityPattern`].
    pub(crate) fn prepare_bar(&self) -> Result<PreparedBar> {
        let quality = self.load_project()?.quality;
        let bar = self.bar(&quality)?;
        Ok(PreparedBar { quality, bar })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;
    use crate::store::format::{canonical, is_canonical};
    use crate::store::model::{Anchor, Decision, DecisionField};
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
