//! A search of the code tree: the nodes (crates, what lies outside every
//! crate, modules, directories, files) and the definitions whose name or
//! path holds a text, each in the one rank that orders them.
//!
//! A hit's name is a node's as a level names it ([`CodeTree::name`]),
//! `outside_crates` for what lies outside every crate, or a definition's
//! last segment. Its path is a crate's, a directory's or a file's path, `.`
//! for the code root, and none for what lies outside every crate, which
//! its name alone matches; a root module's root
//! directory, any other module's segments joined by `::`; a definition's
//! segments joined by `::`, as an anchor's symbol spells them. The text is
//! matched with the ASCII letters of either case alike.
//!
//! A search is one pass over the tree's nodes and definitions and a sort
//! of its hits. A module's path is never spelled whole: each is matched
//! from its parent's last bytes and its own segment, in time linear in the
//! segments times the text's length, and two module paths of one class and
//! length are ordered from where they meet. So a chain of modules
//! thousands deep, a sibling at each level, is searched in time near
//! linear in its length.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::BTreeMap;

use super::address::spelled as spelled_dir;
use super::frame::spelled;
use super::places::ModulePath;
use super::tree::{CodeTree, Node};
use crate::code::index::{DefinitionKind, ScopeId, Scopes, Segment};

/// How a hit matched the text, the better first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Match {
    /// Its name is the text.
    ExactName,
    /// Its name starts with the text.
    NamePrefix,
    /// Its path holds the text.
    PathSubstring,
}

/// A node or a definition a search found.
pub(crate) struct Hit<'i> {
    pub(crate) matched: Match,
    /// The bytes of its path.
    length: usize,
    pub(crate) found: Found<'i>,
}

/// What a hit is.
pub(crate) enum Found<'i> {
    Node(Node<'i>),
    Definition(FoundDefinition<'i>),
}

/// A definition a search found, in the walked file of the node `file`.
pub(crate) struct FoundDefinition<'i> {
    pub(crate) file: Node<'i>,
    /// Its path's segments joined by `::`.
    path: String,
    /// The byte of `path` its last segment starts at.
    name_at: usize,
    pub(crate) kind: DefinitionKind,
    pub(crate) first_line: usize,
    pub(crate) last_line: usize,
    scopes: Scopes<'i>,
    scope: ScopeId,
}

impl FoundDefinition<'_> {
    /// The last segment of its path.
    pub(crate) fn name(&self) -> &str {
        self.path.get(self.name_at..).unwrap_or_default()
    }

    /// Its path's segments as the index spells them, outermost first.
    pub(crate) fn segments(&self) -> Vec<String> {
        spelled(self.scopes, self.scope)
    }
}

/// How a module path of a tree is matched: its bytes, its segments,
/// whether it holds the text, and its last bytes, one short of the text's
/// length or a few more to a char boundary, which a path below it
/// continues.
struct Spelling {
    length: usize,
    depth: usize,
    holds: bool,
    tail: String,
}

/// The spellings of a tree's module paths.
type Spellings = BTreeMap<ModulePath, Spelling>;

/// What a hit's path is compared by: its text, or a module path below a
/// root module, never spelled to be compared.
#[derive(Clone, Copy)]
enum PathKey<'h> {
    Text(&'h str),
    Module(ModulePath),
}

impl<'i> CodeTree<'i> {
    /// Every node and every definition of the walked files whose name or
    /// path holds `text`, in the rank [`CodeTree::rank`] states.
    pub(crate) fn search(&self, text: &str) -> Vec<Hit<'i>> {
        let needle = text.to_ascii_lowercase();
        let modules = self.module_spellings(&needle);
        let nodes = self.levels.values().flatten();
        let mut hits: Vec<Hit<'i>> = nodes
            .filter_map(|&node| self.node_hit(node, &needle, &modules))
            .collect();
        let mut lowered = String::new();
        for (&path, &place) in &self.places {
            let Some((_, index)) = self.walked_index(path) else {
                continue;
            };
            let file = Node::File { path, place };
            let scopes = Scopes(&index.scopes);
            for definition in &index.definitions {
                let (path, name_at) = joined(scopes, definition.scope);
                let name = path.get(name_at..).unwrap_or_default();
                let Some(matched) = matched(name, &path, &needle, &mut lowered) else {
                    continue;
                };
                hits.push(Hit {
                    matched,
                    length: path.len(),
                    found: Found::Definition(FoundDefinition {
                        file,
                        path,
                        name_at,
                        kind: definition.kind,
                        first_line: definition.span.first_line,
                        last_line: definition.span.last_line,
                        scopes,
                        scope: definition.scope,
                    }),
                });
            }
        }
        hits.sort_by(|a, b| self.rank(&modules, a, b));
        hits
    }

    /// The one rank of hits: how they matched (an exact name, a name's
    /// prefix, then a path's substring), then the shorter path, then the
    /// path by its bytes; then a node before a definition, nodes by
    /// address, definitions by file, lines and kind.
    fn rank(&self, modules: &Spellings, a: &Hit<'i>, b: &Hit<'i>) -> Ordering {
        let first = (a.matched, a.length).cmp(&(b.matched, b.length));
        first
            .then_with(|| self.path_order(modules, &a.found, &b.found))
            .then_with(|| match (&a.found, &b.found) {
                (Found::Node(a), Found::Node(b)) => self.node_order(*a, *b),
                (Found::Node(_), Found::Definition(_)) => Ordering::Less,
                (Found::Definition(_), Found::Node(_)) => Ordering::Greater,
                (Found::Definition(a), Found::Definition(b)) => (
                    a.file,
                    a.first_line,
                    a.last_line,
                    a.kind,
                )
                    .cmp(&(b.file, b.first_line, b.last_line, b.kind)),
            })
    }

    /// The order of the paths `a` and `b` are matched by, by their bytes.
    fn path_order(&self, modules: &Spellings, a: &Found<'i>, b: &Found<'i>) -> Ordering {
        match (self.path_key(a), self.path_key(b)) {
            (PathKey::Text(a), PathKey::Text(b)) => a.cmp(b),
            (PathKey::Module(a), PathKey::Module(b)) => self.module_order(modules, a, b),
            (a, b) => self.spelled_key(a).cmp(&self.spelled_key(b)),
        }
    }

    /// What the path `found` is matched by is compared by.
    fn path_key<'h>(&self, found: &'h Found<'i>) -> PathKey<'h> {
        match found {
            Found::Node(node) => match self.node_path(*node) {
                NodePath::Text(text) => PathKey::Text(text),
                NodePath::Module(path) => PathKey::Module(path),
            },
            Found::Definition(definition) => PathKey::Text(&definition.path),
        }
    }

    /// `key` spelled: a module path's segments joined by `::`.
    fn spelled_key<'h>(&self, key: PathKey<'h>) -> Cow<'h, str> {
        match key {
            PathKey::Text(text) => Cow::Borrowed(text),
            PathKey::Module(path) => Cow::Owned(self.module_path(path).join("::")),
        }
    }

    /// The order of the module paths `a` and `b` by the bytes of their
    /// segments joined by `::`, read from where the two meet: each is
    /// walked up to their common ancestor, and the first segments that
    /// differ below it decide, a segment holding no `:`.
    fn module_order(&self, modules: &Spellings, a: ModulePath, b: ModulePath) -> Ordering {
        let depth = |path: ModulePath| modules.get(&path).map_or(0, |spelling| spelling.depth);
        let (mut x, mut y) = (a, b);
        let (mut below_x, mut below_y) = (None, None);
        while depth(x) > depth(y) {
            below_x = Some(x);
            x = self.paths.parent(x).unwrap_or(ModulePath::ROOT);
        }
        while depth(y) > depth(x) {
            below_y = Some(y);
            y = self.paths.parent(y).unwrap_or(ModulePath::ROOT);
        }
        while x != y {
            (below_x, below_y) = (Some(x), Some(y));
            x = self.paths.parent(x).unwrap_or(ModulePath::ROOT);
            y = self.paths.parent(y).unwrap_or(ModulePath::ROOT);
        }
        let first = |below: Option<ModulePath>, end: ModulePath| {
            let segment = below
                .and_then(|path| self.paths.last(path))
                .unwrap_or_default();
            let on = below.is_some_and(|path| path != end);
            let separator: &[u8] = if on { b"::" } else { b"" };
            segment.bytes().chain(separator.iter().copied())
        };
        first(below_x, a).cmp(first(below_y, b))
    }

    /// The order of the nodes `a` and `b`, whose paths are equal, by their
    /// addresses: two modules by their roots, as their segments are one.
    fn node_order(&self, a: Node<'i>, b: Node<'i>) -> Ordering {
        match (a, b) {
            (Node::Module { root: a, .. }, Node::Module { root: b, .. }) => a.cmp(b),
            (a, b) => self.address(a).cmp(&self.address(b)),
        }
    }

    /// `node` as a hit of `needle`, `modules` the spellings of the module
    /// paths; `None` when it does not match.
    fn node_hit(&self, node: Node<'i>, needle: &str, modules: &Spellings) -> Option<Hit<'i>> {
        let (length, holds) = match self.node_path(node) {
            NodePath::Text(text) => (text.len(), text.to_ascii_lowercase().contains(needle)),
            NodePath::Module(path) => {
                let spelling = modules.get(&path)?;
                (spelling.length, spelling.holds)
            }
        };
        let by_name = name_match(self.name(node), needle);
        let matched = by_name.or_else(|| holds.then_some(Match::PathSubstring))?;
        Some(Hit {
            matched,
            length,
            found: Found::Node(node),
        })
    }

    /// The path a search matches `node` by: a text of the tree, or a
    /// module path below a root module.
    fn node_path(&self, node: Node<'i>) -> NodePath<'i> {
        match node {
            Node::Crate(dir) => NodePath::Text(dir.map_or("", spelled_dir)),
            Node::Module { root, path, .. } => match self.paths.last(path) {
                Some(_) => NodePath::Module(path),
                None => NodePath::Text(spelled_dir(root)),
            },
            Node::Directory { dir, .. } => NodePath::Text(dir),
            Node::File { path, .. } => NodePath::Text(path),
        }
    }

    /// How each module path of the tree is matched by `needle`, each from
    /// its parent's.
    fn module_spellings(&self, needle: &str) -> Spellings {
        let keep = needle.len().saturating_sub(1);
        let mut spellings: BTreeMap<ModulePath, Spelling> = BTreeMap::new();
        for (path, parent, segment) in self.paths.each() {
            let above = spellings.get(&parent);
            let mut continued = String::new();
            if let Some(above) = above {
                continued.push_str(&above.tail);
                continued.push_str("::");
            }
            continued.push_str(segment);
            continued.make_ascii_lowercase();
            let holds = above.is_some_and(|above| above.holds) || continued.contains(needle);
            let length = above.map_or(0, |above| above.length + 2) + segment.len();
            let depth = above.map_or(1, |above| above.depth + 1);
            let from = floor_char_boundary(&continued, continued.len().saturating_sub(keep));
            let tail = continued.split_off(from);
            let spelling = Spelling {
                length,
                depth,
                holds,
                tail,
            };
            spellings.insert(path, spelling);
        }
        spellings
    }
}

/// A node's path as a search matches it.
enum NodePath<'i> {
    Text(&'i str),
    Module(ModulePath),
}

/// How `name` and `path` match `needle`, an ASCII-lowercased text, by
/// their best match; `lowered` is a buffer the path is lowercased into.
fn matched(name: &str, path: &str, needle: &str, lowered: &mut String) -> Option<Match> {
    if let Some(matched) = name_match(name, needle) {
        return Some(matched);
    }
    lowered.clear();
    lowered.push_str(path);
    lowered.make_ascii_lowercase();
    lowered.contains(needle).then_some(Match::PathSubstring)
}

/// How `name` matches `needle`, an ASCII-lowercased text: whole, by its
/// start, or not at all.
fn name_match(name: &str, needle: &str) -> Option<Match> {
    let head = name.as_bytes().get(..needle.len())?;
    if !head.eq_ignore_ascii_case(needle.as_bytes()) {
        return None;
    }
    match name.len() == needle.len() {
        true => Some(Match::ExactName),
        false => Some(Match::NamePrefix),
    }
}

/// The path of `scope` with its segments joined by `::`, and the byte its
/// last segment starts at.
fn joined(scopes: Scopes<'_>, scope: ScopeId) -> (String, usize) {
    let mut segments: Vec<&Segment> = scopes.path_from_end(scope).collect();
    segments.reverse();
    let mut joined = String::new();
    let mut name_at = 0;
    for (at, segment) in segments.into_iter().enumerate() {
        if at > 0 {
            joined.push_str("::");
        }
        name_at = joined.len();
        joined.push_str(&segment.to_string());
    }
    (joined, name_at)
}

/// The greatest char boundary of `text` at or below `at`.
fn floor_char_boundary(text: &str, at: usize) -> usize {
    (0..=at.min(text.len()))
        .rev()
        .find(|&at| text.is_char_boundary(at))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::testing::facts;

    /// The modules a search of `text` in `tree` finds, each by its path
    /// spelled, in the rank of the search.
    fn modules(tree: &CodeTree<'_>, text: &str) -> Vec<String> {
        let found = tree.search(text).into_iter();
        let modules = found.filter_map(|hit| match hit.found {
            found @ Found::Node(Node::Module { .. }) => {
                Some(tree.spelled_key(tree.path_key(&found)).into_owned())
            }
            Found::Node(_) | Found::Definition(_) => None,
        });
        modules.collect()
    }

    #[test]
    fn a_module_path_holds_a_text_across_its_segments_as_when_spelled_whole() {
        let files = [
            ("Cargo.toml", "[package]\n"),
            ("src/lib.rs", "mod alpha;\n"),
            ("src/alpha.rs", "mod beta;\n"),
            ("src/alpha/beta.rs", "mod gamma;\n"),
            ("src/alpha/beta/gamma.rs", ""),
        ];
        let facts = facts(&files, &[]);
        let tree = CodeTree::of(&facts);
        let spelled = ["src", "alpha", "alpha::beta", "alpha::beta::gamma"];

        for text in [
            "SRC",
            "a::b",
            "HA::BE",
            "a::beta::g",
            "ph",
            ":",
            "lpha::",
            "x",
        ] {
            let mut oracle: Vec<&str> = spelled
                .into_iter()
                .filter(|path| {
                    path.to_ascii_lowercase()
                        .contains(&text.to_ascii_lowercase())
                })
                .collect();
            oracle.sort_by_key(|path| (path.len(), *path));
            let mut found = modules(&tree, text);
            found.sort_by_key(|path| (path.len(), path.clone()));

            assert_eq!(found, oracle, "{text}");
        }
    }

    /// Modules whose paths tie in length, so their bytes order them: a
    /// segment that another starts with, one ending in a digit, which
    /// sorts below `:`, and one in a letter, which sorts above it.
    #[test]
    fn module_paths_of_one_length_are_in_the_order_of_their_spelled_bytes() {
        let files = [
            ("Cargo.toml", "[package]\n"),
            ("src/lib.rs", "mod ab;\nmod abcde;\nmod ab0xy;\nmod b;\n"),
            ("src/ab.rs", "mod x;\n"),
            ("src/ab/x.rs", "mod q;\n"),
            ("src/ab/x/q.rs", ""),
            ("src/abcde.rs", ""),
            ("src/ab0xy.rs", ""),
            ("src/b.rs", "mod ab;\n"),
            ("src/b/ab.rs", ""),
        ];
        let facts = facts(&files, &[]);
        let tree = CodeTree::of(&facts);

        let found = modules(&tree, "b");

        let mut spelled = found.clone();
        spelled.sort_by(|a, b| (a.len(), a.as_bytes()).cmp(&(b.len(), b.as_bytes())));
        let five: Vec<&String> = found.iter().filter(|path| path.len() == 5).collect();
        assert_eq!(five, ["ab0xy", "ab::x", "abcde", "b::ab"]);
        assert_eq!(found.len(), spelled.len());
    }

    /// A chain of modules, each file declaring the next and a sibling of
    /// it: every level holds two paths of one length, so comparing spelled
    /// paths would be quadratic in the chain's length; ordered from where
    /// they meet, the pair at each level takes one step.
    #[test]
    fn a_chain_of_modules_with_a_sibling_at_each_level_is_searched_and_ranked_to_its_deepest() {
        let count = 8_000;
        let texts: Vec<String> = (0..count)
            .map(|n| {
                format!(
                    "#[path = \"m{}.rs\"]\nmod m;\n#[path = \"x{n}.rs\"]\nmod x;\n",
                    n + 1
                )
            })
            .collect();
        let chain: Vec<String> = (0..count).map(|n| format!("src/m{n}.rs")).collect();
        let siblings: Vec<String> = (0..count).map(|n| format!("src/x{n}.rs")).collect();
        let mut files = vec![
            ("Cargo.toml", "[package]\n"),
            ("src/lib.rs", "#[path = \"m0.rs\"]\nmod m;\n"),
        ];
        files.extend(
            chain
                .iter()
                .map(String::as_str)
                .zip(texts.iter().map(String::as_str)),
        );
        files.extend(siblings.iter().map(|path| (path.as_str(), "")));
        let facts = facts(&files, &[]);
        let tree = CodeTree::of(&facts);

        let found = tree.search("m::");

        let deepest = found
            .last()
            .map(|hit| tree.spelled_key(tree.path_key(&hit.found)).len());
        assert_eq!(found.len(), 2 * count - 1);
        assert_eq!(deepest, Some(3 * count + 1));
    }
}
