//! Where anchors lie in one walked file, each held under a key its caller
//! gives (a decision): on the whole file, through an anchor on the file
//! or on a directory above it with no symbol; on the lines of the spans a
//! symbol on the file resolves to; or on no line, a symbol that resolves
//! to no span, missing, ambiguous, malformed or on a file with no index.
//! An ambiguous symbol's candidates cover nothing. A key on the whole file
//! covers every line, so no range holds it, whatever its symbols cover.
//! The file's lines are cut into maximal ranges, each covered by one set
//! of the other keys, so the ranges split exactly where a span of one of
//! them starts or ends.

use std::collections::{BTreeMap, BTreeSet};

use crate::store::Anchor;

use super::code_facts::{Resolution, Target};
use super::entry::Entry;
use super::tree::CodeTree;

/// Where the anchors of a walked file lie.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Coverage<'a, K> {
    /// The keys of an anchor on the file whole or on a directory above it.
    pub(crate) on_file: BTreeSet<K>,
    /// Lines 1 to the file's line count, in order, with no gap or overlap,
    /// each with the keys not in `on_file` whose spans cover it; none when
    /// the file's bytes were not read, or it has no line.
    pub(crate) ranges: Vec<LineRange<K>>,
    /// Each symbol anchor on the file with what it resolves to, in the
    /// order the anchors were given: the spans of one that resolves are
    /// in `ranges`.
    pub(crate) symbols: Vec<SymbolAnchor<'a, K>>,
}

/// Lines `first_line..=last_line` of a file, and the keys whose spans
/// cover each of them: the same keys for every line, and other keys on the
/// line before and on the line after.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LineRange<K> {
    pub(crate) first_line: usize,
    pub(crate) last_line: usize,
    /// In key order; empty where no span covers the lines.
    pub(crate) keys: Vec<K>,
}

/// A symbol anchor on a file, and what it resolves to there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SymbolAnchor<'a, K> {
    pub(crate) key: K,
    pub(crate) anchor: &'a Anchor,
    pub(crate) resolution: Resolution,
}

impl CodeTree<'_> {
    /// Where `anchors`, each under its key, lie in the walked file at
    /// `path`. An anchor on another file, or with a symbol on a directory,
    /// lies in none of it.
    pub(crate) fn coverage<'a, K: Ord + Copy>(
        &self,
        path: &str,
        anchors: impl IntoIterator<Item = (K, &'a Anchor)>,
    ) -> Coverage<'a, K> {
        let mut coverage = Coverage {
            on_file: BTreeSet::new(),
            ranges: Vec::new(),
            symbols: Vec::new(),
        };
        let mut spans = Vec::new();
        for (key, anchor) in anchors {
            let file = anchor.file();
            let on_file = file == path;
            let above = path
                .strip_prefix(file)
                .is_some_and(|rest| rest.starts_with('/'));
            match anchor.symbol() {
                None if on_file || above => {
                    coverage.on_file.insert(key);
                }
                Some(_) if on_file => {
                    let resolution = self.facts.resolve(anchor);
                    if let Resolution::Resolved(Target::Symbol(definitions)) = &resolution {
                        let lines = definitions
                            .spans()
                            .map(|span| (span.first_line, span.last_line));
                        spans.extend(lines.map(|(first, last)| (first, last, key)));
                    }
                    coverage.symbols.push(SymbolAnchor {
                        key,
                        anchor,
                        resolution,
                    });
                }
                // An anchor on another file, or a symbol on a directory.
                None | Some(_) => {}
            }
        }
        spans.retain(|(_, _, key)| !coverage.on_file.contains(key));
        coverage.ranges = self.ranges(path, &spans);
        coverage
    }

    /// The lines of the walked file at `path` cut by `spans`; none when its
    /// bytes were not read.
    fn ranges<K: Ord + Copy>(&self, path: &str, spans: &[(usize, usize, K)]) -> Vec<LineRange<K>> {
        match self.entry(path) {
            Some(Entry::File {
                lines: Some(lines), ..
            }) => ranges(*lines, spans),
            // A file never read, or a path no walked file is at.
            Some(Entry::File { lines: None, .. } | Entry::Directory | Entry::Unindexed) | None => {
                Vec::new()
            }
        }
    }
}

/// Lines 1 to `lines` cut into maximal ranges, each with the keys of
/// `spans` (first line, last line, key) that cover it. A span past the
/// last line, which no index of the same bytes gives, covers to it.
fn ranges<K: Ord + Copy>(lines: usize, spans: &[(usize, usize, K)]) -> Vec<LineRange<K>> {
    // Each line where keys start covering, and the line after the last
    // they cover, with the change in each key's count there.
    let mut changes: BTreeMap<usize, Vec<(K, bool)>> = BTreeMap::new();
    changes.entry(1).or_default();
    for &(first, last, key) in spans {
        let first = first.max(1);
        if first > lines || last < first {
            continue;
        }
        changes.entry(first).or_default().push((key, true));
        changes
            .entry(last.min(lines) + 1)
            .or_default()
            .push((key, false));
    }
    let mut covering: BTreeMap<K, usize> = BTreeMap::new();
    let mut ranges: Vec<LineRange<K>> = Vec::new();
    for (line, changed) in changes.range(..=lines) {
        for &(key, starts) in changed {
            // A span's end follows its start, so no count drops below
            // zero.
            let count = covering.entry(key).or_default();
            match starts {
                true => *count += 1,
                false => *count -= 1,
            }
        }
        covering.retain(|_, count| *count > 0);
        let keys: Vec<K> = covering.keys().copied().collect();
        if ranges.last().is_some_and(|range| range.keys == keys) {
            continue;
        }
        if let Some(last) = ranges.last_mut() {
            last.last_line = line - 1;
        }
        ranges.push(LineRange {
            first_line: *line,
            last_line: lines,
            keys,
        });
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::testing::facts;

    /// Each range as `first-last keys`.
    fn spelled(ranges: &[LineRange<char>]) -> Vec<String> {
        let spelled = ranges.iter().map(|range| {
            let keys: String = range.keys.iter().collect();
            format!("{}-{} {keys}", range.first_line, range.last_line)
        });
        spelled.collect()
    }

    #[test]
    fn a_range_ends_where_its_set_of_keys_changes_and_no_sooner() {
        let spans = [
            (2, 9, 'a'),
            (4, 5, 'b'),
            (6, 7, 'b'),
            (6, 6, 'c'),
            (11, 12, 'd'),
            (13, 30, 'd'),
            (0, 0, 'e'),
        ];

        let ranges = ranges(14, &spans);

        assert_eq!(
            spelled(&ranges),
            [
                "1-1 ", "2-3 a", "4-5 ab", "6-6 abc", "7-7 ab", "8-9 a", "10-10 ", "11-14 d"
            ],
            "two spans of one key that meet are one range, and a span past the last line ends \
             there"
        );
    }

    #[test]
    fn a_file_with_no_line_has_no_range() {
        assert_eq!(ranges(0, &[(1, 3, 'a')]), []);
    }

    #[test]
    fn only_an_anchor_on_the_file_or_a_directory_above_it_with_no_symbol_is_on_the_file() {
        let files = [("src/shapes.rs", "fn area() {}\n"), ("src/sha/x.rs", "")];
        let facts = facts(&files, &[]);
        let tree = CodeTree::of(&facts);
        let anchor = |file: &str, symbol: Option<&str>| {
            Anchor::new(file.to_owned(), symbol.map(str::to_owned)).unwrap()
        };
        let anchors = [
            ('a', anchor("src/shapes.rs", None)),
            ('b', anchor("src", None)),
            ('c', anchor("src/sha", None)),
            ('d', anchor("src", Some("area"))),
            ('e', anchor("src/sha/x.rs", None)),
            ('f', anchor("src/shapes.rs", Some("area"))),
        ];

        let coverage = tree.coverage("src/shapes.rs", anchors.iter().map(|(k, a)| (*k, a)));

        assert_eq!(coverage.on_file, BTreeSet::from(['a', 'b']));
        let symbols: Vec<char> = coverage.symbols.iter().map(|symbol| symbol.key).collect();
        assert_eq!(
            symbols,
            ['f'],
            "a symbol on a directory names no part of the file"
        );
        assert_eq!(spelled(&coverage.ranges), ["1-1 f"]);
    }

    #[test]
    fn a_key_on_the_file_whole_is_in_no_range_whatever_its_symbols_cover() {
        let files = [("src/shapes.rs", "fn area() {}\nfn side() {}\n")];
        let facts = facts(&files, &[]);
        let tree = CodeTree::of(&facts);
        let anchor = |file: &str, symbol: Option<&str>| {
            Anchor::new(file.to_owned(), symbol.map(str::to_owned)).unwrap()
        };
        let anchors = [
            ('a', anchor("src", None)),
            ('a', anchor("src/shapes.rs", Some("area"))),
            ('b', anchor("src/shapes.rs", Some("side"))),
        ];

        let coverage = tree.coverage("src/shapes.rs", anchors.iter().map(|(k, a)| (*k, a)));

        assert_eq!(spelled(&coverage.ranges), ["1-1 ", "2-2 b"]);
    }
}
