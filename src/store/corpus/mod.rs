//! A seeded synthetic project for benches and property tests: a store of
//! [`Corpus::decisions`] decisions shaped like a production graph, and a
//! Rust source tree that their anchors resolve in.
//!
//! The store is written in one journaled commit of canonical files, the
//! path every write takes, and its tests hold it valid. Every draw comes
//! from the seeded generator (`store::seeded`): benches compare commits only
//! while the corpus stays byte-identical across them.

mod words;

use std::collections::BTreeSet;
use std::fs;
use std::ops::RangeInclusive;
use std::path::Path;
use std::time::Duration;

use crate::{Error, Result};

use super::commit::PendingWrite;
use super::history::{CommitMessage, History, Tool};
use super::schema::{
    AcceptedText, AcceptedVia, Anchor, Component, Decision, PROJECT_COMPONENT, Placement, Status,
};
use super::seeded::SplitMix64;
use super::{Store, Timestamp};
use words::{COMPONENT_WORDS, MODULE_WORDS, SYMBOL_WORDS, TAGS, WORDS};

const DECISIONS_PER_COMPONENT: usize = 40;

const MIN_COMPONENTS: usize = 3;

/// One decision in this many is project-wide.
const PROJECT_WIDE_EVERY: usize = 16;

const FILES_PER_COMPONENT: usize = 64;

const SYMBOLS_PER_FILE: usize = 8;

/// Longer than any word drawn, so a sentence fits its room.
const SHORTEST_SENTENCE: usize = 16;

/// The vocabulary's ranks come in this many bands, the band `b` holding
/// `2^b` ranks: [`WORDS`] first, then words coined from [`SYLLABLES`],
/// 8,191 in all. A draw picks a band, then a rank in it, so rank `r` is
/// drawn about `1 / (13 (r + 1))` of the time. That gives a few words in
/// most texts and a long tail in few, as in prose; a uniform draw from a
/// short list makes every two texts alike.
const VOCABULARY_BANDS: usize = 13;

/// What a coined word is spelled with: none of them a stop word of
/// `lint::duplicates`, so a coined word is always a term.
const SYLLABLES: [&str; 16] = [
    "ba", "de", "fi", "go", "ku", "la", "me", "no", "pu", "ri", "sa", "te", "vo", "wu", "xe", "zo",
];

/// One decision in this many restates an earlier one: its reason opens
/// with the first half of the earlier reason, a likely duplicate for
/// `status` to report.
const RESTATED_EVERY: usize = 25;

const SENTENCE_BYTES: RangeInclusive<usize> = 40..=120;

const CHOICE_BYTES: RangeInclusive<usize> = 150..=200;

/// Around 900 bytes on average.
const REASON_BYTES: RangeInclusive<usize> = 600..=1200;

const ALTERNATIVE_BYTES: RangeInclusive<usize> = 60..=150;

const MAX_ALTERNATIVES: usize = 7;

const MAX_TAGS: usize = 6;

/// Anchors of most decisions, at most; one in ten has up to
/// [`MAX_ANCHORS`].
const USUAL_ANCHORS: usize = 10;

const MAX_ANCHORS: usize = 20;

/// Every timestamp is this instant plus an offset, so writing a corpus never
/// reads the clock.
fn epoch() -> Timestamp {
    // A literal in range, so the parse holds.
    "2025-01-01T00:00:00Z"
        .parse()
        .unwrap_or(Timestamp::UNIX_EPOCH)
}

/// `at` plus `offset`. A corpus spans months from [`epoch`], so the sum is
/// in range and holds.
fn later(at: Timestamp, offset: Duration) -> Timestamp {
    at.checked_add(offset).unwrap_or(at)
}

const HOUR: Duration = Duration::from_secs(60 * 60);

/// The shape of a generated project. The same value writes the same bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Corpus {
    /// Decisions in the store, project-wide ones included.
    pub decisions: usize,
    /// Share of the source files that anchors name, in percent, rounded
    /// down per component. It holds exactly while each component's
    /// anchors outnumber its anchored files, as they do at [`Corpus::SIZES`].
    pub anchored_files_percent: u8,
    /// Seeds every draw; two seeds give two corpora of the same shape.
    pub seed: u64,
}

impl Corpus {
    /// The decision counts benches run at.
    pub const SIZES: [usize; 3] = [50, 600, 2000];

    /// About three quarters of a production project's files carry anchors.
    pub const PRODUCTION_ANCHORED_PERCENT: u8 = 75;

    /// A production-shaped corpus of `decisions` decisions.
    #[must_use]
    pub const fn new(decisions: usize) -> Self {
        Self {
            decisions,
            anchored_files_percent: Self::PRODUCTION_ANCHORED_PERCENT,
            seed: 0x7472_7572_6c69_6321,
        }
    }

    /// Create the store in `project_dir/.trurlic` and the source tree in
    /// `project_dir/src`. Fails like [`Store::create`] when a store is there.
    pub fn write(&self, project_dir: &Path) -> Result<Store> {
        self.write_as(project_dir, Placement::Tracked)
    }

    /// [`write`](Self::write), the store stating `placement`: a `shared`
    /// one's repository holds the corpus in its second commit.
    pub fn write_as(&self, project_dir: &Path, placement: Placement) -> Result<Store> {
        let plan = Plan::new(*self);
        let store = Store::create(project_dir, "corpus", placement)?;
        plan.write_sources(project_dir)?;
        let writes = plan.writes(&store)?;
        let message = CommitMessage::of(Tool::Init);
        let history = History::of(placement, &message);
        store.commit_batch(&store.lock()?, &writes, &[], history)?;
        Ok(store)
    }
}

/// Set `word` to the word of the vocabulary at `rank`: an entry of
/// [`WORDS`], then a word coined from [`SYLLABLES`]. A coined word spells
/// the digits of a number from 256 on in base 16, so it has three
/// syllables or more and no two are spelled alike.
fn spell(rank: usize, word: &mut String) {
    word.clear();
    if let Some(known) = WORDS.get(rank) {
        word.push_str(known);
        return;
    }
    let base = SYLLABLES.len();
    let mut coined = rank - WORDS.len() + base * base;
    while coined > 0 {
        let syllable = SYLLABLES.get(coined % base);
        word.push_str(syllable.copied().unwrap_or_default());
        coined /= base;
    }
}

/// The corpus's text, drawn from the vocabulary of [`VOCABULARY_BANDS`].
impl SplitMix64 {
    fn prose_within(&mut self, bytes: &RangeInclusive<usize>) -> String {
        let length = self.within(bytes);
        self.prose(length)
    }

    fn pick<'a>(&mut self, words: &[&'a str]) -> &'a str {
        words[self.below(words.len())]
    }

    /// Sentences of the vocabulary, at most `bytes` long, on one line.
    fn prose(&mut self, bytes: usize) -> String {
        let mut text = String::with_capacity(bytes);
        while bytes.saturating_sub(text.len()) > SHORTEST_SENTENCE {
            if !text.is_empty() {
                text.push(' ');
            }
            let room = bytes - text.len() - 1;
            let length = self.within(&SENTENCE_BYTES).min(room);
            let sentence = self.phrase(length);
            let mut letters = sentence.chars();
            text.extend(letters.next().map(|c| c.to_ascii_uppercase()));
            text.push_str(letters.as_str());
            text.push('.');
        }
        text
    }

    /// Lowercase words of the vocabulary, at most `bytes` long unless the
    /// first word alone is longer.
    fn phrase(&mut self, bytes: usize) -> String {
        let mut text = String::with_capacity(bytes);
        let mut word = String::new();
        loop {
            self.word(&mut word);
            if text.is_empty() {
                text.push_str(&word);
            } else if text.len() + 1 + word.len() <= bytes {
                text.push(' ');
                text.push_str(&word);
            } else {
                return text;
            }
        }
    }

    /// Set `word` to a word of the vocabulary, drawn by its rank's band.
    fn word(&mut self, word: &mut String) {
        let band = 1 << self.below(VOCABULARY_BANDS);
        spell(band - 1 + self.below(band), word);
    }

    /// A reason of about `bytes` that restates `earlier`: its first half,
    /// cut at a space, then sentences of its own.
    fn restate(&mut self, earlier: &str, bytes: usize) -> String {
        let half = earlier.get(..earlier.len() / 2).unwrap_or(earlier);
        let kept = half.rfind(' ').and_then(|space| half.get(..space));
        let mut text = kept.unwrap_or(half).to_owned();
        let rest = self.prose(bytes.saturating_sub(text.len() + 1));
        if !rest.is_empty() {
            text.push(' ');
            text.push_str(&rest);
        }
        text
    }
}

/// Every node and source file of a corpus, drawn in a fixed order.
struct Plan {
    components: Vec<String>,
    decisions: Vec<(String, Decision)>,
}

impl Plan {
    fn new(corpus: Corpus) -> Self {
        let mut rng = SplitMix64::new(corpus.seed);
        let components = component_names(corpus.decisions);
        let percent = usize::from(corpus.anchored_files_percent.min(100));
        let anchored_per_component = FILES_PER_COMPONENT * percent / 100;
        let mut anchors = Anchors::new(components.len(), anchored_per_component);
        let mut decisions: Vec<(String, Decision)> = Vec::with_capacity(corpus.decisions);
        let mut created = epoch();
        for index in 0..corpus.decisions {
            let owner =
                (!index.is_multiple_of(PROJECT_WIDE_EVERY)).then(|| rng.below(components.len()));
            let component = owner.map_or(PROJECT_COMPONENT, |c| components[c].as_str());
            let drawn = anchors.draw(&mut rng, owner, &components);
            let mut decision = plan_decision(&mut rng, index, component, created, drawn);
            if index > 0 && rng.one_in(RESTATED_EVERY) {
                let earlier = decisions.get(rng.below(index));
                if let Some((_, earlier)) = earlier {
                    decision.reason = rng.restate(&earlier.reason, decision.reason.len());
                }
            }
            // An earlier decision only, so `depends_on` has no cycle.
            if index > 0 && rng.one_in(2) {
                let (target, _) = &decisions[rng.below(index)];
                // clone: the edge names a decision the plan keeps.
                decision.depends_on.insert(target.clone());
            }
            decisions.push((format!("{component}-d{index}"), decision));
            created = later(created, HOUR);
        }
        Self {
            components,
            decisions,
        }
    }

    /// The canonical file of every node. Each component connects to the
    /// next two.
    fn writes(self, store: &Store) -> Result<Vec<PendingWrite>> {
        let count = self.components.len();
        let mut writes = Vec::with_capacity(count + self.decisions.len());
        for (index, name) in self.components.iter().enumerate() {
            let component = Component {
                description: format!("The {name} service of the synthetic project"),
                connects_to: (1..=2)
                    // clone: each edge names a component the plan keeps.
                    .map(|step| self.components[(index + step) % count].clone())
                    .collect(),
            };
            writes.push(store.prepare_write(&store.component_path(name), &component)?);
        }
        for (name, decision) in &self.decisions {
            writes.push(store.prepare_write(&store.decision_path(name), decision)?);
        }
        Ok(writes)
    }

    /// One `pub fn` per [`SYMBOL_WORDS`] entry in every file, anchored or not.
    fn write_sources(&self, project_dir: &Path) -> Result<()> {
        for component in &self.components {
            let dir = project_dir.join("src").join(component);
            fs::create_dir_all(&dir).map_err(Error::io(&dir))?;
            for file in 0..FILES_PER_COMPONENT {
                let stem = module_stem(file);
                let mut source = format!("//! The {stem} module of {component}.\n");
                for (factor, prefix) in (3..).zip(SYMBOL_WORDS) {
                    source.push_str(&format!(
                        "\npub fn {prefix}_{stem}(input: usize) -> usize {{\n    \
                         input.wrapping_mul({factor}) ^ {file}\n}}\n"
                    ));
                }
                let path = dir.join(format!("{stem}.rs"));
                fs::write(&path, source).map_err(Error::io(&path))?;
            }
        }
        Ok(())
    }
}

/// Assigns anchors: a component's decisions name its anchored files in
/// turn and project-wide decisions name every component's, so each anchored
/// file is named once the anchors outnumber the files.
struct Anchors {
    next: Vec<usize>,
    next_project_wide: usize,
    per_component: usize,
}

impl Anchors {
    fn new(components: usize, per_component: usize) -> Self {
        Self {
            next: vec![0; components],
            next_project_wide: 0,
            per_component,
        }
    }

    /// Mostly 0 to [`USUAL_ANCHORS`] anchors, and one decision in ten up to
    /// [`MAX_ANCHORS`]. One anchor in twenty names a file without a symbol;
    /// two draws of one place are one anchor.
    fn draw(
        &mut self,
        rng: &mut SplitMix64,
        owner: Option<usize>,
        components: &[String],
    ) -> BTreeSet<Anchor> {
        if self.per_component == 0 {
            return BTreeSet::new();
        }
        let most = if rng.one_in(10) {
            MAX_ANCHORS
        } else {
            USUAL_ANCHORS
        };
        (0..rng.below(most + 1))
            .filter_map(|_| {
                let (component, file) = self.next_file(owner, components.len());
                let stem = module_stem(file);
                let path = format!("src/{}/{stem}.rs", components[component]);
                let symbol =
                    (!rng.one_in(20)).then(|| format!("{}_{stem}", rng.pick(&SYMBOL_WORDS)));
                // A path of names this module draws is always an anchor.
                Anchor::new(path, symbol).ok()
            })
            .collect()
    }

    /// The `(component, file)` the next anchor of `owner` names.
    fn next_file(&mut self, owner: Option<usize>, components: usize) -> (usize, usize) {
        match owner {
            Some(component) => {
                let file = self.next[component];
                self.next[component] = (file + 1) % self.per_component;
                (component, file)
            }
            None => {
                let slot = self.next_project_wide;
                self.next_project_wide = (slot + 1) % (self.per_component * components);
                (slot / self.per_component, slot % self.per_component)
            }
        }
    }
}

fn plan_decision(
    rng: &mut SplitMix64,
    index: usize,
    component: &str,
    created: Timestamp,
    anchors: BTreeSet<Anchor>,
) -> Decision {
    let alternatives = (0..rng.below(MAX_ALTERNATIVES + 1))
        .map(|_| rng.prose_within(&ALTERNATIVE_BYTES))
        .collect();
    let tags = tags(rng);
    let choice = choice(rng, index, component);
    let status = status(rng, index, component);
    Decision {
        component: component.into(),
        choice,
        reason: rng.prose_within(&REASON_BYTES),
        alternatives,
        tags,
        status,
        origin: rng.one_in(3).then(|| format!("task {}", index / 25)),
        created,
        anchors,
        depends_on: BTreeSet::new(),
    }
}

/// One decision in ten is proposed, one in four of those a revision of an
/// accepted text; an accepted one names how it was accepted, or nothing
/// when it predates the record.
fn status(rng: &mut SplitMix64, index: usize, component: &str) -> Status {
    if rng.one_in(10) {
        let accepted_text = rng.one_in(4).then(|| AcceptedText {
            choice: choice(rng, index, component),
            reason: rng.prose_within(&REASON_BYTES),
            alternatives: vec![rng.prose_within(&ALTERNATIVE_BYTES)],
            via: None,
        });
        return Status::Proposed { accepted_text };
    }
    let via = match rng.below(3) {
        0 => None,
        1 => Some(AcceptedVia::Cli),
        _ => Some(AcceptedVia::Elicitation),
    };
    Status::Accepted { via }
}

/// Up to [`MAX_TAGS`] draws from [`TAGS`]; a repeat is one tag.
fn tags(rng: &mut SplitMix64) -> BTreeSet<String> {
    (0..rng.below(MAX_TAGS + 1))
        .map(|_| rng.pick(TAGS).to_owned())
        .collect()
}

/// A one-line choice that starts with its component and index.
fn choice(rng: &mut SplitMix64, index: usize, component: &str) -> String {
    let mut text = format!("{component} {index} ");
    let length = rng.within(&CHOICE_BYTES);
    text.push_str(&rng.phrase(length - text.len()));
    text
}

fn component_names(decisions: usize) -> Vec<String> {
    let count = (decisions / DECISIONS_PER_COMPONENT).max(MIN_COMPONENTS);
    (0..count)
        .map(|index| {
            let word = COMPONENT_WORDS[index % COMPONENT_WORDS.len()];
            match index / COMPONENT_WORDS.len() {
                0 => word.to_owned(),
                round => format!("{word}-{round}"),
            }
        })
        .collect()
}

/// File stems: [`MODULE_WORDS`] with a round suffix.
fn module_stem(file: usize) -> String {
    let word = MODULE_WORDS[file % MODULE_WORDS.len()];
    format!("{word}_{}", file / MODULE_WORDS.len())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::store::canonical::canonical;
    use crate::store::limits::MAX_CHOICE_BYTES;
    use crate::store::{ProjectState, STATE_DIR};
    use tempfile::TempDir;

    /// Every file under `dir` but `.state/`, by path relative to `dir`.
    fn files(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut found = Vec::new();
        let mut pending = vec![dir.to_path_buf()];
        while let Some(next) = pending.pop() {
            for entry in fs::read_dir(next).unwrap() {
                let path = entry.unwrap().path();
                if path.ends_with(STATE_DIR) {
                    continue;
                }
                if path.is_dir() {
                    pending.push(path);
                } else {
                    let bytes = fs::read(&path).unwrap();
                    found.push((path.strip_prefix(dir).unwrap().to_path_buf(), bytes));
                }
            }
        }
        found.sort();
        found
    }

    fn written(corpus: Corpus) -> (TempDir, ProjectState) {
        let tmp = TempDir::new().unwrap();
        let state = corpus.write(tmp.path()).unwrap().load_state().unwrap();
        (tmp, state)
    }

    /// Source files under `src/`, and those an anchor names.
    fn source_files(dir: &Path, state: &ProjectState) -> (usize, BTreeSet<String>) {
        let total = files(&dir.join("src")).len();
        let named = state
            .decisions
            .values()
            .flat_map(|d| &d.anchors)
            .map(|anchor| anchor.file().to_owned())
            .collect();
        (total, named)
    }

    #[test]
    fn a_seed_fixes_every_byte_and_another_seed_changes_them() {
        let corpus = Corpus::new(50);
        let first = TempDir::new().unwrap();
        let second = TempDir::new().unwrap();
        let reseeded = TempDir::new().unwrap();
        corpus.write(first.path()).unwrap();
        corpus.write(second.path()).unwrap();
        Corpus { seed: 7, ..corpus }.write(reseeded.path()).unwrap();

        assert_eq!(files(first.path()), files(second.path()));
        assert_ne!(files(first.path()), files(reseeded.path()));
    }

    #[test]
    fn every_size_is_a_valid_canonical_graph_of_the_requested_shape() {
        for size in Corpus::SIZES {
            let (tmp, state) = written(Corpus::new(size));

            assert_eq!(state.decisions.len(), size);
            assert_eq!(state.validate(), [], "{size}");
            assert_eq!(state.non_canonical, [], "{size}");
            let decisions: Vec<&Decision> = state.decisions.values().collect();
            let reason_bytes: usize = decisions.iter().map(|d| d.reason.len()).sum();
            assert!((800..=1000).contains(&(reason_bytes / size)), "{size}");
            assert!(decisions.iter().all(|d| d.choice.len() <= MAX_CHOICE_BYTES));
            assert!(decisions.iter().all(|d| d.anchors.len() <= MAX_ANCHORS));
            let depends_on = decisions
                .iter()
                .filter(|d| !d.depends_on.is_empty())
                .count();
            assert!((40..=60).contains(&(depends_on * 100 / size)), "{size}");
            let proposed = decisions
                .iter()
                .filter(|d| matches!(d.status, Status::Proposed { .. }))
                .count();
            assert!(proposed > 0 && proposed * 5 < size, "{size}: {proposed}");
            let (total, named) = source_files(tmp.path(), &state);
            assert_eq!(named.len() * 100, total * 75, "{size}");
        }
    }

    /// The property behind `check` and `fmt`: canonical bytes read back as
    /// the value they were written from, and the bytes of what a file reads
    /// as are a fixpoint.
    #[test]
    fn every_node_reads_back_from_its_canonical_bytes_and_they_are_a_fixpoint() {
        let tmp = TempDir::new().unwrap();
        let store = Corpus::new(600).write(tmp.path()).unwrap();
        let state = store.load_state().unwrap();

        for (name, decision) in &state.decisions {
            let bytes = fs::read_to_string(store.decision_path(name)).unwrap();
            let read: Decision = toml::from_str(&bytes).unwrap();
            assert_eq!(&read, decision, "{name}");
            assert_eq!(canonical(&read), bytes, "{name}");
        }
        for (name, component) in &state.components {
            let bytes = fs::read_to_string(store.component_path(name)).unwrap();
            assert_eq!(&toml::from_str::<Component>(&bytes).unwrap(), component);
            assert_eq!(canonical(component), bytes, "{name}");
        }
    }

    #[test]
    fn every_anchor_names_a_file_and_function_in_the_source_tree() {
        let (tmp, state) = written(Corpus::new(600));

        for anchor in state.decisions.values().flat_map(|d| &d.anchors) {
            let source = fs::read_to_string(tmp.path().join(anchor.file())).unwrap();
            if let Some(symbol) = anchor.symbol() {
                assert!(source.contains(&format!("pub fn {symbol}(")), "{anchor}");
            }
        }
    }

    /// Every rank the bands reach is a word of its own: 8,191 words, the
    /// coined ones of three syllables or more, longer than no sentence's
    /// least room.
    #[test]
    fn the_vocabulary_holds_one_word_per_rank() {
        let ranks = (1 << VOCABULARY_BANDS) - 1;
        let mut word = String::new();
        let words: BTreeSet<String> = (0..ranks)
            .map(|rank| {
                spell(rank, &mut word);
                assert!(word.len() < SHORTEST_SENTENCE, "{word}");
                word.clone()
            })
            .collect();
        assert_eq!(words.len(), 8191);
        spell(WORDS.len(), &mut word);
        assert_eq!(word.len(), 6);
    }

    #[test]
    fn the_anchored_share_follows_its_parameter() {
        let half = Corpus {
            anchored_files_percent: 50,
            ..Corpus::new(600)
        };
        let (tmp, state) = written(half);
        let (total, named) = source_files(tmp.path(), &state);
        assert_eq!(named.len() * 2, total);

        let none = Corpus {
            anchored_files_percent: 0,
            ..Corpus::new(50)
        };
        let (_tmp, state) = written(none);
        assert!(state.decisions.values().all(|d| d.anchors.is_empty()));
    }
}
