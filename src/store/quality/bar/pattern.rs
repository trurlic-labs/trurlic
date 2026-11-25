//! One `[quality]` pattern on regex-automata's lazy DFA, its only engine,
//! and what judging the patterns may spend. A search costs time linear in
//! the bytes it reads once the states it walks are built, and it builds
//! them in a cache of `MAX_PATTERN_CACHE_BYTES` per direction that gives up
//! when full instead of clearing. So a call builds at most one cache of
//! states per pattern and direction, a write no more than its total of
//! cache besides ([`Scan`]), and what it reads is the rest of its cost,
//! charged in bytes ([`searched`]).

use std::collections::BTreeSet;
use std::error::Error;

use regex_automata::hybrid::dfa;
use regex_automata::hybrid::regex::{Cache, Regex};
use regex_automata::nfa::thompson;
use regex_automata::{Input, Match};

use super::{Breach, Broken, Hit, Place};
use crate::store::limits::{MAX_PATTERN_CACHE_BYTES, MAX_PATTERN_COMPILED_BYTES};
use crate::store::quality::{DecisionField, Level, Pattern, PatternMatch, QualityFault};

/// The refusal of a pattern that holds a Unicode word boundary, which the
/// lazy DFA runs only over ASCII text, with the rewrites that run on it.
const UNICODE_WORD_BOUNDARY: &str = "holds a Unicode word boundary (`\\b`, `\\B`), which the \
     lazy DFA runs over ASCII text alone: write `(?-u:\\b)` over ASCII text, or \
     `(?:^|\\W)word(?:\\W|$)` in any script";

/// A pattern of `[quality]`, compiled.
#[derive(Debug)]
pub(super) struct Compiled {
    regex: Regex,
    pub(super) fields: BTreeSet<DecisionField>,
    pub(super) level: Level,
    pub(super) message: String,
}

/// A pattern's match in a text: its byte offset and the text it matched.
pub(super) struct Found<'t> {
    pub(super) at: usize,
    pub(super) text: &'t str,
}

impl Compiled {
    /// `pattern` on the lazy DFA, or why it does not run there: the
    /// engine's message for a syntax error or an NFA past
    /// `MAX_PATTERN_COMPILED_BYTES`, and the rewrite for a Unicode word
    /// boundary or an automaton that cannot start within its cache.
    pub(super) fn new(pattern: &Pattern) -> Result<Self, String> {
        let regex = Regex::builder()
            .thompson(thompson::Config::new().nfa_size_limit(Some(MAX_PATTERN_COMPILED_BYTES)))
            .dfa(
                dfa::Config::new()
                    .cache_capacity(MAX_PATTERN_CACHE_BYTES)
                    .minimum_cache_clear_count(Some(0))
                    // Built so that it is refused below with its rewrite,
                    // not with the engine's advice.
                    .unicode_word_boundary(true),
            )
            .build(&pattern.pattern)
            .map_err(|error| match error.source() {
                // A fault of the NFA: a syntax error, a size past the limit.
                Some(nfa) => innermost(nfa),
                // The lazy DFA's own: its cache cannot hold the states a
                // search starts from.
                None => format!(
                    "its automaton does not fit the cache of a pattern ({error}): repeat a \
                     Unicode class fewer times, or write an ASCII one such as `(?-u:\\w)`"
                ),
            })?;
        if regex
            .forward()
            .get_nfa()
            .look_set_any()
            .contains_word_unicode()
        {
            return Err(UNICODE_WORD_BOUNDARY.to_owned());
        }
        // clone: the bar outlives the borrow of the graph a batch changes,
        // so it owns each pattern's fields and message.
        let (fields, message) = (pattern.fields.clone(), pattern.message.clone());
        Ok(Self {
            regex,
            fields,
            level: pattern.level,
            message,
        })
    }

    /// An empty cache for this pattern's automaton.
    pub(super) fn cache(&self) -> Cache {
        self.regex.create_cache()
    }

    /// The first of `texts` this pattern matches, with its place, searched
    /// in order over `cache`; the place of the text it gave up on, its
    /// cache full.
    pub(super) fn first_match<'t>(
        &self,
        cache: &mut Cache,
        texts: impl IntoIterator<Item = (Place, &'t str)>,
    ) -> Result<Option<(Place, Found<'t>)>, Place> {
        for (place, text) in texts {
            // The only error the configuration admits is a give-up: no byte
            // quits the search, and it is unanchored over the whole text.
            let found = self
                .regex
                .try_search(cache, &Input::new(text))
                .map_err(|_gave_up| place)?;
            if let Some(found) = found {
                return Ok(Some((place, Found::of(text, found))));
            }
        }
        Ok(None)
    }

    /// The match `found` at `place` of this pattern, the one at `index`:
    /// the refusal at `refuse`, else the warning.
    pub(super) fn verdict(
        &self,
        index: usize,
        place: Place,
        found: &Found<'_>,
    ) -> Result<Hit, Breach> {
        match self.level {
            Level::Refuse => Err(Breach {
                place,
                broken: Broken::Quality(QualityFault::Pattern(self.matched(index, found))),
            }),
            Level::Warn => Ok(Hit {
                pattern: index,
                at: found.at,
            }),
        }
    }

    /// The match `found` of this pattern, the one at `index`, as a fault
    /// names it.
    pub(super) fn matched(&self, index: usize, found: &Found<'_>) -> PatternMatch {
        PatternMatch {
            index,
            at: found.at,
            found: super::quoted(found.text),
            // clone: the fault carries the message it reports.
            message: self.message.clone(),
        }
    }
}

impl<'t> Found<'t> {
    fn of(text: &'t str, found: Match) -> Self {
        Self {
            at: found.start(),
            text: text.get(found.range()).unwrap_or_default(),
        }
    }
}

/// The message of the deepest error under `error`: the engine names its
/// fault there, and only a wrapper's words above it.
fn innermost(error: &(dyn Error + 'static)) -> String {
    let mut deepest = error;
    while let Some(source) = deepest.source() {
        deepest = source;
    }
    deepest.to_string()
}

/// The bytes a search over each of `texts` charges: its length, and one
/// more for the search itself, so that empty texts are not free.
pub(super) fn searched<'t>(texts: impl IntoIterator<Item = &'t str>) -> usize {
    texts
        .into_iter()
        .map(|text| text.len().saturating_add(1))
        .fold(0, usize::saturating_add)
}

/// What one write's patterns may still read and build, and each pattern's
/// cache, all the batch's: the bytes of every search are charged against
/// one bound, and the growth of every cache, both directions, against one
/// total of cache.
pub(crate) struct Scan {
    pub(super) caches: Vec<Cache>,
    /// The bytes left of `bound`.
    pub(super) left: usize,
    pub(super) bound: usize,
    /// The bytes of cache the automata may still add, all patterns
    /// together.
    pub(super) cache_left: usize,
}
