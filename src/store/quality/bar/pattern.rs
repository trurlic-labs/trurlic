//! One `[quality]` pattern on regex-automata's lazy DFA, its only engine,
//! and what judging the patterns may spend. Compiling a pattern costs its
//! text, what its case-insensitive classes fold (bounded on its syntax
//! tree before translation, [`folding`](super::folding)) and its NFA,
//! bounded in size. A search costs time linear in the bytes it reads once
//! the states it walks are built, and it builds them in a cache of
//! `MAX_PATTERN_CACHE_BYTES` per direction that gives up when full instead
//! of clearing. A call's caches grow by `MAX_PATTERN_CACHE_TOTAL_BYTES`
//! together at most, a pattern's two caches past it at the check
//! ([`Scan`]), and what a call reads is the rest of its cost, charged in
//! bytes ([`searched`]).

use std::cmp::Reverse;
use std::collections::BTreeSet;

use regex_automata::hybrid::dfa;
use regex_automata::hybrid::regex::{Cache, Regex};
use regex_automata::nfa::thompson::{self, NFA, WhichCaptures};
use regex_automata::{Input, Match, MatchKind};
use regex_syntax::ast;
use regex_syntax::hir::Hir;
use regex_syntax::hir::translate::Translator;

use super::folding::folded_code_points;
use super::{Breach, Broken, Hit, Place};
use crate::store::limits::{
    MAX_PATTERN_CACHE_BYTES, MAX_PATTERN_COMPILED_BYTES, MAX_PATTERN_FOLDED_CODE_POINTS,
};
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
    /// The memory of each cache before the batch's first op, which its
    /// growth is measured from.
    pub(super) fresh: Vec<usize>,
    /// The memory of each cache when the op being judged began.
    pub(super) at_op: Vec<usize>,
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
        let fresh: Vec<usize> = caches.iter().map(Cache::memory_usage).collect();
        Self {
            // clone: what the caches hold before the first op is also what
            // they hold when it begins.
            at_op: fresh.clone(),
            fresh,
            caches,
            left: bytes,
            cache_left: cache_bytes,
        }
    }

    /// Count what the caches hold now as theirs before any op: the batch's
    /// origin is no op, and splitting the batch would not shed it.
    pub(super) fn settle(&mut self) {
        self.fresh = self.caches.iter().map(Cache::memory_usage).collect();
        // clone: no op has begun.
        self.at_op = self.fresh.clone();
    }

    /// Mark the start of an op: what the caches hold now, the ops' before
    /// it, tells whether its texts alone filled them.
    pub(crate) fn begin_op(&mut self) {
        self.at_op = self.caches.iter().map(Cache::memory_usage).collect();
    }

    /// Whether no op before the one being judged grew a cache.
    pub(super) fn is_alone(&self) -> bool {
        self.at_op == self.fresh
    }

    /// The first match of `pattern`, the one at `index`, over `read`,
    /// charged first: its bytes against what is left, refused at its first
    /// text past it; then a search over its cache, refused where the cache
    /// gave up; then the cache's growth against the total, `Err(place)` at
    /// the match or the last text past it.
    pub(super) fn search<'t>(
        &mut self,
        index: usize,
        pattern: &Compiled,
        read: Vec<(Place, &'t str)>,
    ) -> Result<Result<Option<(Place, Found<'t>)>, Place>, Breach> {
        let (Some(&(first, _)), Some(&(last, _))) = (read.first(), read.last()) else {
            return Ok(Ok(None));
        };
        let bytes = searched(read.iter().map(|&(_, text)| text));
        self.left = self.left.checked_sub(bytes).ok_or(Breach {
            place: first,
            broken: Broken::Work,
        })?;
        // A cache no earlier op grew was filled by this op's texts alone.
        let alone = self.fresh.get(index) == self.at_op.get(index);
        let Some(cache) = self.caches.get_mut(index) else {
            return Ok(Ok(None));
        };
        let before = cache.memory_usage();
        let found = pattern.first_match(cache, read).map_err(|place| Breach {
            place,
            broken: Broken::Quality(QualityFault::GaveUp { index, alone }),
        })?;
        let grown = cache.memory_usage().saturating_sub(before);
        let Some(rest) = self.cache_left.checked_sub(grown) else {
            return Ok(Err(found.as_ref().map_or(last, |&(place, _)| place)));
        };
        self.cache_left = rest;
        Ok(Ok(found))
    }

    /// The index of the pattern whose caches grew the most.
    pub(super) fn largest(&self) -> usize {
        let grown = self.caches.iter().zip(&self.fresh);
        grown_most(grown.map(|(cache, &fresh)| cache.memory_usage().saturating_sub(fresh)))
    }
}

/// The index of the pattern of `grown`, each pattern's growth in their
/// order, that grew the most: the first of them, `0` for none.
pub(super) fn grown_most(grown: impl IntoIterator<Item = usize>) -> usize {
    let ranked = grown.into_iter().enumerate();
    let ranked = ranked.map(|(index, grown)| (grown, Reverse(index)));
    ranked.max().map_or(0, |(_, Reverse(index))| index)
}
