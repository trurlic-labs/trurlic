//! A seeded synthetic project for benches and property tests: a store of
//! [`Corpus::decisions`] decisions shaped like a production graph, and a
//! Rust source tree that their code refs resolve in.
//!
//! The store is written in one commit through [`Store::commit_with_graph`],
//! so it passes the same validation as any write. Draws come from
//! SplitMix64 rather than `rand`, whose generators may change output between
//! releases: benches compare commits only while the corpus stays
//! byte-identical across them.

use std::fs;
use std::ops::RangeInclusive;
use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Duration, TimeZone, Utc};

use crate::{Error, Result};

use super::commit::PendingWrite;
use super::limits::MAX_CODE_REFS;
use super::schema::{
    Attribution, CodeRef, Component, ComponentFile, Decision, DecisionFile, EdgeEntry, EdgeKind,
    HistoryEntry, NodeEntry, NodeKind, Pattern, PatternFile,
};
use super::{ProjectState, Store, slugify};

/// The project-wide pseudo-component decisions may belong to.
const PROJECT: &str = "project";

const DECISIONS_PER_COMPONENT: usize = 40;

const MIN_COMPONENTS: usize = 3;

/// One decision in this many is project-wide.
const PROJECT_WIDE_EVERY: usize = 16;

/// One pattern per this many decisions.
const DECISIONS_PER_PATTERN: usize = 32;

const PATTERN_MEMBERS: RangeInclusive<usize> = 2..=5;

const FILES_PER_COMPONENT: usize = 64;

const SYMBOLS_PER_FILE: usize = 8;

/// Longer than any entry of [`WORDS`], so a sentence fits its room.
const SHORTEST_SENTENCE: usize = 16;

const SENTENCE_BYTES: RangeInclusive<usize> = 40..=120;

const CHOICE_BYTES: RangeInclusive<usize> = 150..=200;

/// Around 900 bytes on average.
const REASON_BYTES: RangeInclusive<usize> = 600..=1200;

const ALTERNATIVE_BYTES: RangeInclusive<usize> = 60..=150;

const MAX_ALTERNATIVES: usize = 7;

const MAX_TAGS: usize = 6;

/// History entries of a revised decision, at most.
const MAX_HISTORY: usize = 14;

/// Every timestamp is this instant plus an offset, so writing a corpus never
/// reads the clock. The newest is under 100 days later: every decision has
/// read as stale since late 2025, so a read answers the same on any day.
fn epoch() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0)
        .single()
        .unwrap_or(DateTime::UNIX_EPOCH)
}

/// Prose vocabulary. It carries keywords of every concern area, so concern
/// coverage is computed over text that matches.
const WORDS: &[&str] = &[
    "the",
    "store",
    "writes",
    "each",
    "node",
    "file",
    "under",
    "lock",
    "and",
    "reads",
    "it",
    "back",
    "before",
    "commit",
    "so",
    "a",
    "crash",
    "leaves",
    "graph",
    "valid",
    "token",
    "auth",
    "boundary",
    "error",
    "retry",
    "timeout",
    "cache",
    "latency",
    "budget",
    "hash",
    "validate",
    "integrity",
    "thread",
    "concurrent",
    "api",
    "protocol",
    "endpoint",
    "test",
    "fixture",
    "config",
    "default",
    "module",
    "layer",
    "dependency",
    "log",
    "metric",
    "trace",
    "schema",
    "format",
    "migration",
    "version",
    "name",
    "path",
    "request",
    "response",
    "client",
    "server",
    "queue",
    "batch",
    "index",
    "memory",
    "parse",
    "render",
    "refuse",
];

/// Component names; a corpus with more components than these suffixes them.
const COMPONENT_WORDS: &[&str] = &[
    "auth",
    "billing",
    "catalog",
    "gateway",
    "ingest",
    "ledger",
    "mailer",
    "notify",
    "orders",
    "pricing",
    "reports",
    "search",
    "session",
    "storage",
    "telemetry",
    "workers",
];

/// Source file stems inside a component directory.
const MODULE_WORDS: &[&str] = &[
    "api", "cache", "client", "config", "error", "handler", "index", "model", "parser", "queue",
    "render", "schema", "service", "store", "types", "worker",
];

/// Function name prefixes; one file holds one function per prefix.
const SYMBOL_WORDS: [&str; SYMBOLS_PER_FILE] = [
    "build", "check", "load", "parse", "read", "render", "store", "write",
];

const TAGS: &[&str] = &[
    "api",
    "concurrency",
    "error-handling",
    "integrity",
    "performance",
    "persistence",
    "security",
    "testing",
];

/// The shape of a generated project. The same value writes the same bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Corpus {
    /// Decisions in the store, project-wide ones included.
    pub decisions: usize,
    /// Share of the source files that code refs name, in percent, rounded
    /// down per component. It holds exactly while each component's code
    /// refs outnumber its anchored files, as they do at [`Corpus::SIZES`].
    pub anchored_files_percent: u8,
    /// Seeds every draw; two seeds give two corpora of the same shape.
    pub seed: u64,
}

impl Corpus {
    /// The decision counts benches run at.
    pub const SIZES: [usize; 3] = [50, 600, 2000];

    /// About three quarters of a production project's files carry code refs.
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
        let plan = Plan::new(*self);
        let store = Store::create(project_dir, "corpus", epoch())?;
        plan.write_sources(project_dir)?;
        let lock = store.lock()?;
        let mut state = store.load_state()?;
        let writes = plan.into_state(&store, &mut state)?;
        store.commit_with_graph(&lock, writes, &[], &mut state)?;
        Ok(store)
    }
}

/// SplitMix64 (Steele, Lea and Flood, 2014): a 64-bit state, full period,
/// and an output fixed by its definition.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n` by multiply-shift, which has no modulo bias.
    fn below(&mut self, n: usize) -> usize {
        let bound = u128::try_from(n).unwrap_or(u128::MAX);
        let draw = (u128::from(self.next()) * bound) >> 64;
        // `draw < n`, so the conversion holds.
        usize::try_from(draw).unwrap_or(0)
    }

    fn within(&mut self, range: &RangeInclusive<usize>) -> usize {
        range.start() + self.below(range.end() - range.start() + 1)
    }

    fn one_in(&mut self, n: usize) -> bool {
        self.below(n) == 0
    }

    fn prose_within(&mut self, bytes: &RangeInclusive<usize>) -> String {
        let length = self.within(bytes);
        self.prose(length)
    }

    fn pick<'a>(&mut self, words: &[&'a str]) -> &'a str {
        words[self.below(words.len())]
    }

    /// Sentences of [`WORDS`], at most `bytes` long, on one line.
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

    /// Lowercase words of [`WORDS`], at most `bytes` long unless the first
    /// word alone is longer.
    fn phrase(&mut self, bytes: usize) -> String {
        let mut text = String::with_capacity(bytes);
        loop {
            let word = self.pick(WORDS);
            if text.is_empty() {
                text.push_str(word);
            } else if text.len() + 1 + word.len() <= bytes {
                text.push(' ');
                text.push_str(word);
            } else {
                return text;
            }
        }
    }
}

/// One generated decision before it is committed.
struct PlannedDecision {
    name: String,
    file: DecisionFile,
    depends_on: Option<usize>,
}

struct PlannedPattern {
    name: String,
    file: PatternFile,
    component: usize,
    members: Vec<usize>,
}

/// Every node and source file of a corpus, drawn in a fixed order.
struct Plan {
    components: Vec<String>,
    decisions: Vec<PlannedDecision>,
    patterns: Vec<PlannedPattern>,
}

impl Plan {
    fn new(corpus: Corpus) -> Self {
        let mut rng = SplitMix64(corpus.seed);
        let components = component_names(corpus.decisions);
        let percent = usize::from(corpus.anchored_files_percent.min(100));
        let anchored_per_component = FILES_PER_COMPONENT * percent / 100;
        let mut anchors = Anchors::new(components.len(), anchored_per_component);
        let mut decisions = Vec::with_capacity(corpus.decisions);
        let mut created = epoch();
        for index in 0..corpus.decisions {
            let owner =
                (!index.is_multiple_of(PROJECT_WIDE_EVERY)).then(|| rng.below(components.len()));
            let component = owner.map_or(PROJECT, |c| components[c].as_str());
            let code_refs = anchors.draw(&mut rng, owner, &components);
            decisions.push(plan_decision(
                &mut rng, index, component, created, code_refs,
            ));
            created += Duration::hours(1);
        }
        let mut plan = Self {
            components,
            decisions,
            patterns: Vec::new(),
        };
        plan.patterns = plan.plan_patterns(&mut rng, corpus.decisions / DECISIONS_PER_PATTERN);
        plan
    }

    /// Patterns over consecutive decisions of one component each.
    fn plan_patterns(&self, rng: &mut SplitMix64, count: usize) -> Vec<PlannedPattern> {
        (0..count)
            .filter_map(|index| {
                let owner = index % self.components.len();
                let component = &self.components[owner];
                let owned: Vec<usize> = (0..self.decisions.len())
                    .filter(|&d| self.decisions[d].file.decision.component == *component)
                    .collect();
                let wanted = rng.within(&PATTERN_MEMBERS);
                (owned.len() >= wanted).then(|| {
                    let start = rng.below(owned.len() - wanted + 1);
                    PlannedPattern {
                        name: format!("{component}-pattern-{index}"),
                        file: PatternFile {
                            pattern: Pattern {
                                name: format!("{component} pattern {index}"),
                                description: rng.prose_within(&ALTERNATIVE_BYTES),
                            },
                        },
                        component: owner,
                        members: owned[start..start + wanted].to_vec(),
                    }
                })
            })
            .collect()
    }

    /// Move every node and edge into `state` and return the node file writes.
    fn into_state(self, store: &Store, state: &mut ProjectState) -> Result<Vec<PendingWrite>> {
        let edges = self.edges();
        let mut writes = Vec::new();
        for name in self.components {
            let file = ComponentFile {
                component: Component {
                    description: format!("The {name} service of the synthetic project"),
                    // clone: the file and the node entry each own the name.
                    name: name.clone(),
                },
            };
            let write = store.prepare_write(&store.component_path(&name), &file)?;
            state
                .graph_index
                .nodes
                .push(node_entry(&name, NodeKind::Component, &[], &write));
            state.components.insert(name, Arc::new(file));
            writes.push(write);
        }
        for planned in self.decisions {
            let write = store.prepare_write(&store.decision_path(&planned.name), &planned.file)?;
            let tags = &planned.file.decision.tags;
            state.graph_index.nodes.push(node_entry(
                &planned.name,
                NodeKind::Decision,
                tags,
                &write,
            ));
            state.decisions.insert(planned.name, Arc::new(planned.file));
            writes.push(write);
        }
        for planned in self.patterns {
            let write = store.prepare_write(&store.pattern_path(&planned.name), &planned.file)?;
            state
                .graph_index
                .nodes
                .push(node_entry(&planned.name, NodeKind::Pattern, &[], &write));
            state.patterns.insert(planned.name, Arc::new(planned.file));
            writes.push(write);
        }
        state.graph_index.edges.extend(edges);
        Ok(writes)
    }

    /// Each component connects to the next two; each decision belongs to its
    /// component and depends on at most one earlier decision, so `depends_on`
    /// has no cycle.
    fn edges(&self) -> Vec<EdgeEntry> {
        let edge = |from: &str, to: &str, kind| EdgeEntry {
            from: from.into(),
            to: to.into(),
            kind,
        };
        let count = self.components.len();
        let mut edges = Vec::new();
        for (index, from) in self.components.iter().enumerate() {
            for step in 1..=2 {
                let to = &self.components[(index + step) % count];
                edges.push(edge(from, to, EdgeKind::ConnectsTo));
            }
        }
        for planned in &self.decisions {
            let decision = &planned.file.decision;
            edges.push(edge(
                &planned.name,
                &decision.component,
                EdgeKind::BelongsTo,
            ));
            if let Some(target) = planned.depends_on {
                let to = &self.decisions[target].name;
                edges.push(edge(&planned.name, to, EdgeKind::DependsOn));
            }
        }
        for planned in &self.patterns {
            let component = &self.components[planned.component];
            edges.push(edge(&planned.name, component, EdgeKind::AppliesTo));
            for &member in &planned.members {
                let to = &self.decisions[member].name;
                edges.push(edge(&planned.name, to, EdgeKind::MemberOf));
            }
        }
        edges
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

fn node_entry(name: &str, kind: NodeKind, tags: &[String], write: &PendingWrite) -> NodeEntry {
    NodeEntry {
        name: name.into(),
        kind,
        tags: tags.to_vec(),
        hash: write.content_hash(),
    }
}

/// Assigns code refs: a component's decisions name its anchored files in
/// turn and project-wide decisions name every component's, so each anchored
/// file is named once the refs outnumber the files.
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

    /// Mostly 0 to 10 refs, and one decision in ten up to [`MAX_CODE_REFS`].
    /// One ref in twenty names a file without a symbol.
    fn draw(
        &mut self,
        rng: &mut SplitMix64,
        owner: Option<usize>,
        components: &[String],
    ) -> Vec<CodeRef> {
        if self.per_component == 0 {
            return Vec::new();
        }
        let most = if rng.one_in(10) { MAX_CODE_REFS } else { 10 };
        (0..rng.below(most + 1))
            .map(|_| {
                let (component, file) = self.next_file(owner, components.len());
                let stem = module_stem(file);
                CodeRef {
                    file: format!("src/{}/{stem}.rs", components[component]),
                    symbol: (!rng.one_in(20))
                        .then(|| format!("{}_{stem}", rng.pick(&SYMBOL_WORDS))),
                }
            })
            .collect()
    }

    /// The `(component, file)` the next ref of `owner` names.
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
    created: DateTime<Utc>,
    code_refs: Vec<CodeRef>,
) -> PlannedDecision {
    let history = history(rng, index, component, created);
    let alternatives = (0..rng.below(MAX_ALTERNATIVES + 1))
        .map(|_| rng.prose_within(&ALTERNATIVE_BYTES))
        .collect();
    let tags = tags(rng);
    let choice = choice(rng, index, component);
    let attribution = if rng.one_in(10) {
        Attribution::Agent
    } else {
        Attribution::User
    };
    PlannedDecision {
        name: slugify(&choice),
        file: DecisionFile {
            decision: Decision {
                component: component.into(),
                choice,
                reason: rng.prose_within(&REASON_BYTES),
                alternatives,
                tags,
                attribution,
                created,
                code_refs,
                history,
            },
        },
        depends_on: (index > 0 && rng.one_in(2)).then(|| rng.below(index)),
    }
}

/// Prior versions of a decision, a day apart: one decision in twenty has
/// any, as most decisions are never revised.
fn history(
    rng: &mut SplitMix64,
    index: usize,
    component: &str,
    created: DateTime<Utc>,
) -> Vec<HistoryEntry> {
    let revisions = if rng.one_in(20) {
        1 + rng.below(MAX_HISTORY)
    } else {
        0
    };
    let mut changed_at = created;
    (0..revisions)
        .map(|_| {
            changed_at += Duration::days(1);
            HistoryEntry {
                choice: choice(rng, index, component),
                reason: rng.prose_within(&REASON_BYTES),
                changed_at,
            }
        })
        .collect()
}

/// Up to [`MAX_TAGS`] draws from [`TAGS`], sorted and without repeats.
fn tags(rng: &mut SplitMix64) -> Vec<String> {
    let mut tags: Vec<String> = (0..rng.below(MAX_TAGS + 1))
        .map(|_| rng.pick(TAGS).to_owned())
        .collect();
    tags.sort_unstable();
    tags.dedup();
    tags
}

/// A one-line choice that starts with its component and index, so its slug
/// is unique and differs from every component name.
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
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    use super::*;
    use crate::store::limits::MAX_CHOICE_BYTES;
    use crate::store::{GRAPH_FILE, STATE_DIR, STORE_DIR};
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

    fn edges_of(state: &ProjectState, kind: EdgeKind) -> usize {
        state
            .graph_index
            .edges
            .iter()
            .filter(|e| e.kind == kind)
            .count()
    }

    /// Source files under `src/`, and those a code ref names.
    fn source_files(dir: &Path, state: &ProjectState) -> (usize, BTreeSet<String>) {
        let total = files(&dir.join("src")).len();
        let named = state
            .decisions
            .values()
            .flat_map(|d| &d.decision.code_refs)
            .map(|r| r.file.clone())
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
        let graph = |dir: &TempDir| fs::read(dir.path().join(STORE_DIR).join(GRAPH_FILE)).unwrap();
        assert_ne!(graph(&first), graph(&reseeded));
    }

    #[test]
    fn every_size_is_a_valid_graph_of_the_requested_shape() {
        for size in Corpus::SIZES {
            let (tmp, state) = written(Corpus::new(size));
            let store = Store::at(tmp.path().join(STORE_DIR));

            assert_eq!(state.decisions.len(), size);
            assert_eq!(state.validate(), [], "{size}");
            assert_eq!(store.verify_hashes().unwrap(), [], "{size}");
            let decisions: Vec<&Decision> = state.decisions.values().map(|d| &d.decision).collect();
            let reason_bytes: usize = decisions.iter().map(|d| d.reason.len()).sum();
            assert!((800..=1000).contains(&(reason_bytes / size)), "{size}");
            assert!(decisions.iter().all(|d| d.choice.len() <= MAX_CHOICE_BYTES));
            assert!(decisions.iter().all(|d| d.code_refs.len() <= MAX_CODE_REFS));
            assert!(decisions.iter().all(|d| d.history.len() <= MAX_HISTORY));
            assert!(decisions.iter().any(|d| d.history.len() > 1), "{size}");
            let depends_on = edges_of(&state, EdgeKind::DependsOn) * 100 / size;
            assert!((40..=60).contains(&depends_on), "{size}: {depends_on}");
            assert!(edges_of(&state, EdgeKind::MemberOf) >= 2, "{size}");
            let (total, named) = source_files(tmp.path(), &state);
            assert_eq!(named.len() * 100, total * 75, "{size}");
        }
    }

    #[test]
    fn every_code_ref_names_a_file_and_function_in_the_source_tree() {
        let (tmp, state) = written(Corpus::new(600));

        for code_ref in state.decisions.values().flat_map(|d| &d.decision.code_refs) {
            let source = fs::read_to_string(tmp.path().join(&code_ref.file)).unwrap();
            if let Some(symbol) = &code_ref.symbol {
                assert!(
                    source.contains(&format!("pub fn {symbol}(")),
                    "{code_ref:?}"
                );
            }
        }
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
        assert!(
            state
                .decisions
                .values()
                .all(|d| d.decision.code_refs.is_empty())
        );
    }
}
