//! One `[quality]` pattern on regex-automata's lazy DFA, its only engine,
//! and what judging the patterns may spend. Compiling a pattern costs its
//! text, what its case-insensitive classes fold (bounded on its syntax
//! tree before translation, [`folding`](super::folding)) and its NFA,
//! bounded in size. A search costs time linear in the bytes it reads once
//! the states it walks are built, and it builds them in a cache of
//! `MAX_PATTERN_CACHE_BYTES` per direction that gives up when full instead
//! of clearing. One call's patterns, a write's or a report page's, share
//! one [`Scan`]: their caches grow by `MAX_PATTERN_CACHE_TOTAL_BYTES`
//! together, past it by one search at most, and what they read is the rest
//! of their cost, charged in bytes ([`searched`]).

use std::borrow::Cow;
use std::cmp::Reverse;
use std::collections::BTreeSet;

use regex_automata::hybrid::dfa;
use regex_automata::hybrid::regex::{Cache, Regex};
use regex_automata::nfa::thompson::{self, NFA, WhichCaptures};
use regex_automata::{Input, Match, MatchError, MatchKind};
use regex_syntax::ast;
use regex_syntax::hir::Hir;
use regex_syntax::hir::translate::Translator;

use super::folding::folded_code_points;
use super::{Breach, Broken, Hit, Place};
use crate::store::model::limits::{
    MAX_PATTERN_CACHE_BYTES, MAX_PATTERN_COMPILED_BYTES, MAX_PATTERN_FOLDED_CODE_POINTS,
};
use crate::store::model::{DecisionField, Level, Pattern};
use crate::store::quality::{PatternMatch, QualityFault};

/// The refusal of a pattern that holds a Unicode word boundary, which the
/// lazy DFA runs only over ASCII text, with the rewrites that run on it.
const UNICODE_WORD_BOUNDARY: &str = "holds a Unicode word boundary (`\\b`, `\\B`), which the \
     lazy DFA runs over ASCII text alone: write `(?-u:\\b)` over ASCII text, or \
     `(?:^|\\W)word(?:\\W|$)` in any script";

/// A pattern of `[quality]`, compiled.
#[derive(Debug)]
pub(super) struct Compiled {
    regex: Regex,
    fields: BTreeSet<DecisionField>,
    pub(super) level: Level,
    pub(super) message: String,
}

/// A pattern's match in a text: its byte offset and the text it matched.
pub(super) struct Found<'t> {
    at: usize,
    text: &'t str,
}

impl Compiled {
    /// `pattern` on the lazy DFA, or why it does not run there: the
    /// parser's message for a syntax error, the engine's for an NFA past
    /// `MAX_PATTERN_COMPILED_BYTES`, and the rewrite for case-insensitive
    /// classes past `MAX_PATTERN_FOLDED_CODE_POINTS`, a Unicode word
    /// boundary or an automaton that cannot start within its cache. The
    /// text is parsed and translated once for both directions, and what
    /// translation would fold is bounded before it runs.
    pub(super) fn new(pattern: &Pattern) -> Result<Self, String> {
        let hir = translated(&pattern.pattern)?;
        let forward = nfa(&hir, false)?;
        if forward.look_set_any().contains_word_unicode() {
            return Err(UNICODE_WORD_BOUNDARY.to_owned());
        }
        let reverse = nfa(&hir, true)?;
        let config = dfa::Config::new()
            .cache_capacity(MAX_PATTERN_CACHE_BYTES)
            .minimum_cache_clear_count(Some(0));
        // The reverse automaton as regex-automata's own builder makes it:
        // every match, no prefilter, start states unmarked.
        let backward = config
            // clone: the forward automaton is built from the config as it is.
            .clone()
            .prefilter(None)
            .specialize_start_states(false)
            .match_kind(MatchKind::All);
        let regex =
            Regex::builder().build_from_dfas(lazy(config, forward)?, lazy(backward, reverse)?);
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
    fn cache(&self) -> Cache {
        self.regex.create_cache()
    }

    /// Those of `texts` in the fields this pattern reads, in their order.
    pub(super) fn read<'t>(&self, texts: &'t [(Place, Cow<'_, str>)]) -> Vec<(Place, &'t str)> {
        texts
            .iter()
            .filter(|(place, _)| self.fields.contains(&place.field))
            .map(|(place, text)| (*place, text.as_ref()))
            .collect()
    }

    /// The first match of this pattern in `text`, searched over `cache`;
    /// the error the search gave when the cache gave up, full. No byte
    /// quits the search, and it is unanchored over the whole text, so a
    /// give-up is the only error the configuration admits.
    fn find<'t>(&self, cache: &mut Cache, text: &'t str) -> Result<Option<Found<'t>>, MatchError> {
        let found = self.regex.try_search(cache, &Input::new(text))?;
        Ok(found.map(|found| Found::of(text, found)))
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

/// `text` parsed and translated, once for both directions; or why not: the
/// parser's message, or the remedy for case-insensitive classes whose
/// folding, counted over the parsed pattern before translation runs, would
/// pass `MAX_PATTERN_FOLDED_CODE_POINTS`.
fn translated(text: &str) -> Result<Hir, String> {
    let ast = ast::parse::Parser::new()
        .parse(text)
        .map_err(|error| error.to_string())?;
    let folded = folded_code_points(text, &ast);
    if folded > MAX_PATTERN_FOLDED_CODE_POINTS {
        return Err(format!(
            "folds the case of {folded} code points in its case-insensitive classes, past the \
             {MAX_PATTERN_FOLDED_CODE_POINTS} a pattern may: narrow those classes, or scope the \
             flag to the words that need it with `(?i:...)`"
        ));
    }
    Translator::new()
        .translate(text, &ast)
        .map_err(|error| error.to_string())
}

/// The lazy DFA of `nfa` under `config`; or, the lazy DFA's one refusal of
/// an NFA that compiled, the remedy for a cache that cannot hold the
/// states a search starts from.
fn lazy(config: dfa::Config, nfa: NFA) -> Result<dfa::DFA, String> {
    let built = dfa::Builder::new().configure(config).build_from_nfa(nfa);
    built.map_err(|error| {
        format!(
            "its automaton does not fit the cache of a pattern ({error}): repeat a Unicode \
             class fewer times, or write an ASCII one such as `(?-u:\\w)` or `[0-9]`"
        )
    })
}

/// The Thompson NFA of `hir`, forward or in `reverse`, as the lazy DFA
/// reads it (no capture), within `MAX_PATTERN_COMPILED_BYTES`; or the
/// engine's message.
fn nfa(hir: &Hir, reverse: bool) -> Result<NFA, String> {
    let config = thompson::Config::new()
        .nfa_size_limit(Some(MAX_PATTERN_COMPILED_BYTES))
        .which_captures(WhichCaptures::None)
        .reverse(reverse);
    thompson::Compiler::new()
        .configure(config)
        .build_from_hir(hir)
        .map_err(|error| error.to_string())
}

/// The bytes a search over each of `texts` charges: its length, and one
/// more for the search itself, so that empty texts are not free. A match's
/// bytes are read twice, forward to its end and back to its start, so a
/// search reads at most twice what it charges.
pub(super) fn searched<'t>(texts: impl IntoIterator<Item = &'t str>) -> usize {
    texts
        .into_iter()
        .map(|text| text.len().saturating_add(1))
        .fold(0, usize::saturating_add)
}

/// What one call's patterns may still read and build, a write's or a
/// report page's, and each pattern's cache, the call's. The bytes of every
/// search are charged against one bound before it runs, and the growth of
/// every cache, both directions, against one total after each text a
/// pattern searches: the caches pass the total by one search's growth at
/// most, one pattern's two caches. What a search spent stays charged,
/// whatever becomes of its verdict.
pub(crate) struct Scan {
    caches: Vec<Cache>,
    /// The memory of each cache when the call began, which its growth is
    /// measured from.
    fresh: Vec<usize>,
    /// The bytes the patterns may still read.
    pub(super) left: usize,
    /// The bytes of cache the automata may still add, all patterns
    /// together.
    pub(super) cache_left: usize,
}

impl Scan {
    /// The caches of `patterns`, empty, and the bytes they may read and
    /// grow by.
    pub(super) fn of(patterns: &[Compiled], bytes: usize, cache_bytes: usize) -> Self {
        let caches: Vec<Cache> = patterns.iter().map(Compiled::cache).collect();
        Self {
            fresh: caches.iter().map(Cache::memory_usage).collect(),
            caches,
            left: bytes,
            cache_left: cache_bytes,
        }
    }

    /// Charge the bytes of the searches over `read` against what is left,
    /// before any of them runs: past it, refused at the first of them.
    pub(super) fn charge(&mut self, read: &[(Place, &str)]) -> Result<(), Breach> {
        let Some(&(first, _)) = read.first() else {
            return Ok(());
        };
        let bytes = searched(read.iter().map(|&(_, text)| text));
        self.left = self.left.checked_sub(bytes).ok_or(Breach {
            place: first,
            broken: Broken::Work,
        })?;
        Ok(())
    }

    /// The first match of `pattern`, the one at `index`, over `read`, text
    /// by text over its cache, each search's growth charged against the
    /// total: the place of the text where the cache gave up, or where the
    /// growth passed what was left, and the fault. Growth past the total
    /// spends it whole, and a cache that gave up is emptied, so a call that
    /// goes on past the fault, a write leaving a repair, builds again only
    /// what it charges.
    pub(super) fn search<'t>(
        &mut self,
        index: usize,
        pattern: &Compiled,
        read: &[(Place, &'t str)],
    ) -> Result<Option<(Place, Found<'t>)>, (Place, QualityFault)> {
        let Some(cache) = self.caches.get_mut(index) else {
            return Ok(None);
        };
        for &(place, text) in read {
            let before = cache.memory_usage();
            let found = pattern.find(cache, text);
            let grown = cache.memory_usage().saturating_sub(before);
            let within = self.cache_left.checked_sub(grown);
            self.cache_left = within.unwrap_or(0);
            let found = match found {
                Ok(found) => found,
                Err(_gave_up) => {
                    cache.reset(&pattern.regex);
                    return Err((place, QualityFault::GaveUp { index }));
                }
            };
            if within.is_none() {
                let largest = self.largest();
                return Err((place, QualityFault::CacheTotal { largest }));
            }
            if let Some(found) = found {
                return Ok(Some((place, found)));
            }
        }
        Ok(None)
    }

    /// The index of the pattern whose caches hold the most they built over
    /// the call, the first of equals, `0` for none.
    fn largest(&self) -> usize {
        let grown = self.caches.iter().zip(&self.fresh).enumerate();
        let ranked = grown.map(|(index, (cache, &fresh))| {
            (cache.memory_usage().saturating_sub(fresh), Reverse(index))
        });
        ranked.max().map_or(0, |(_, Reverse(index))| index)
    }
}
