//! The one reading of the paths a Rust file names ([`Reading`]), which the
//! index's imports (`imports.rs`) and the crate's boundary tests
//! (`crate::purity`) both take. It is every token of the file, macro
//! arguments and bodies included, each in the inline module it stands in,
//! and the paths those tokens name (`paths.rs`). Each consumer judges what it reads
//! by a rule of its own: which bytes are test code, which paths count.
//!
//! A token is a leaf or a literal of the tree (`tokens.rs`), a raw
//! identifier as its name. In an attribute other than `doc`, whose strings
//! are prose, a string is read for each path of two segments or more it
//! spells anywhere (`#[serde(with = "crate::x::f")]`, `bound = "T:
//! Into<crate::x::T>"`), a line it continues joined ([`joined`]). Those
//! segments are tokens before the string's own. A single word there names
//! an item in scope, which its `use` or its definition accounts for, and
//! no macro puts `$crate` into a string.

use std::borrow::Cow;
use std::ops::Range;
use std::sync::LazyLock;

use tree_sitter::Node;

use crate::code::parse::parser::text_of;
use crate::code::parse::tokens::Token as Event;

/// What is read of a Rust file: its tokens, in order, and the inline
/// modules they stand in. It holds too the bytes a path's first name is
/// read against: the `use`s that stand in a block, the blocks that may
/// bind a name anew, and the token trees of its macros. It holds one token for each the
/// walk takes, which the parse's budget bounds (`parser.rs`), and its
/// paths one link for each token (`paths.rs`). That is linear in the
/// file, and none of it is kept, so it is charged to no room.
#[derive(Default)]
pub(crate) struct Reading<'t> {
    pub(crate) tokens: Vec<Token<'t>>,
    /// The item of each inline module, `mod name { .. }`, in the order the
    /// tokens enter them, one inside a function's body included.
    pub(crate) modules: Vec<Node<'t>>,
    /// Each `use` that stands in a block, which binds its names in that
    /// block alone.
    pub(super) block_uses: Spans,
    /// The blocks that hold a `use`, an `extern crate` or a macro
    /// invocation as an item or a statement, which may bind any name in
    /// them anew: the outermost of each nest.
    pub(super) rebinding: Spans,
    /// Each token tree no other holds, a macro's arguments or body.
    pub(super) macro_trees: Spans,
    /// While the walk runs: the nodes it is in, innermost last. A node's
    /// parent is read here: tree-sitter's `parent()` walks down from the
    /// root on each ask, quadratic in a file of nested blocks.
    ancestors: Vec<Node<'t>>,
    /// While the walk runs: the inline modules it is in, innermost last.
    open: Vec<usize>,
    /// While the walk runs: the attributes other than `doc` it is in.
    attributes: usize,
    /// While the walk runs: the token trees it is in.
    trees: usize,
}

/// Bytes of a file, each run after the one before it and apart from it.
#[derive(Default)]
pub(super) struct Spans(Vec<Range<usize>>);

impl Spans {
    /// The spans of `bytes`, which nest or stand apart: the outermost hold
    /// every byte any of them holds.
    pub(super) fn of(mut bytes: Vec<Range<usize>>) -> Self {
        bytes.sort_unstable_by_key(|span| span.start);
        let mut end = 0;
        bytes.retain(|span| {
            let outer = span.start >= end;
            end = end.max(span.end);
            outer
        });
        Self(bytes)
    }

    /// Add `span`, which holds a byte past every span's start, as a walk in
    /// the file's order meets it: a span it holds, or that holds it, is
    /// one of the two.
    pub(super) fn add(&mut self, span: Range<usize>) {
        while let Some(last) = self.0.last() {
            if last.start <= span.start && span.end <= last.end {
                return;
            }
            if span.start <= last.start && last.end <= span.end {
                self.0.pop();
            } else {
                break;
            }
        }
        self.0.push(span);
    }

    /// Whether a span holds the byte `at`.
    pub(super) fn hold(&self, at: usize) -> bool {
        let after = self.0.partition_point(|span| span.start <= at);
        let holder = after.checked_sub(1).and_then(|holder| self.0.get(holder));
        holder.is_some_and(|span| span.contains(&at))
    }
}

/// The ids of the node kinds of tree-sitter-rust's grammar the reading
/// tells apart, and of the field of an item's generic parameters.
struct Kinds {
    attribute: u16,
    inner_attribute: u16,
    module: u16,
    use_declaration: u16,
    extern_crate: u16,
    macro_invocation: u16,
    statement: u16,
    block: u16,
    token_tree: u16,
    /// The field `type_parameters`; 0, which names no field, were the
    /// grammar to lose it.
    type_parameters: u16,
}

impl Kinds {
    /// The kinds' names, in the order of the fields they give.
    const NAMES: [&'static str; 9] = [
        "attribute_item",
        "inner_attribute_item",
        "mod_item",
        "use_declaration",
        "extern_crate_declaration",
        "macro_invocation",
        "expression_statement",
        "block",
        "token_tree",
    ];

    /// The field of an item's generic parameters.
    const TYPE_PARAMETERS: &'static str = "type_parameters";

    fn of(grammar: &tree_sitter::Language) -> Self {
        let [
            attribute,
            inner_attribute,
            module,
            use_declaration,
            extern_crate,
            macro_invocation,
            statement,
            block,
            token_tree,
        ] = Self::NAMES.map(|name| grammar.id_for_node_kind(name, true));
        Self {
            attribute,
            inner_attribute,
            module,
            use_declaration,
            extern_crate,
            macro_invocation,
            statement,
            block,
            token_tree,
            type_parameters: grammar
                .field_id_for_name(Self::TYPE_PARAMETERS)
                .map_or(0, u16::from),
        }
    }

    fn is_attribute(&self, kind: u16) -> bool {
        kind == self.attribute || kind == self.inner_attribute
    }
}

/// [`Kinds`], read once a process from the grammar.
static KINDS: LazyLock<Kinds> = LazyLock::new(|| Kinds::of(&tree_sitter_rust::LANGUAGE.into()));

/// The generic parameters of `item`, by the field's id read once: a
/// field asked by its name is found by a scan of the grammar's names on
/// each ask.
pub(super) fn type_parameters<'t>(item: Node<'t>) -> Option<Node<'t>> {
    item.child_by_field_id(KINDS.type_parameters)
}

/// One token of a file.
pub(crate) struct Token<'t> {
    /// Its text, a raw identifier as its name; a segment an attribute's
    /// string spells is owned only when the string continues a line.
    pub(crate) text: Cow<'t, str>,
    /// The byte it starts at; for a segment a string spells, the
    /// string's.
    pub(crate) start: usize,
    /// The inline module it stands in, an index into
    /// [`Reading::modules`]; `None` for the file's own module.
    pub(crate) module: Option<usize>,
    /// Whether it is a raw identifier (`r#type`, `r#Type`), whose name may
    /// be spelled as a keyword.
    pub(crate) raw: bool,
}

impl<'t> Reading<'t> {
    /// The reading of `root`, the tree of `text`, in one walk of its
    /// tokens, as the boundary tests read a file alone. The index takes
    /// each event of its own walk of the tokens instead ([`Reading::take`]),
    /// so one walk serves its hashes and its reading.
    #[cfg(test)]
    pub(crate) fn of(root: Node<'t>, text: &'t str) -> Self {
        let mut reading = Self::default();
        let lexicon = &super::lexicon::LEXICON;
        crate::code::parse::tokens::tokens(root, text, lexicon, |event| {
            reading.take(event, text);
            std::ops::ControlFlow::Continue(())
        });
        reading
    }

    /// Take `event`, the next of a walk of the tokens of `text`'s tree
    /// with Rust's lexicon (`tokens.rs`). A node is told apart by its
    /// kind's id ([`KINDS`]), not its name, which tree-sitter spells anew
    /// on each ask: the reading rides every index's walk.
    pub(super) fn take(&mut self, event: Event<'t>, text: &'t str) {
        match event {
            Event::Enter(node) => {
                self.enter(node, text);
                self.ancestors.push(node);
            }
            Event::Leave(node) => {
                self.ancestors.pop();
                self.leave(node, text);
            }
            Event::Node(node) => self.push_token(node, text),
            // A gap, a run and a hole hold no token of a path.
            Event::Gap { .. } | Event::Run { .. } | Event::Hole { .. } => {}
        }
    }

    /// Enter `node`: an attribute whose strings are read, an inline
    /// module, what may bind a name anew in a block, or a token tree.
    fn enter(&mut self, node: Node<'t>, text: &'t str) {
        let kinds = &*KINDS;
        let kind = node.kind_id();
        if kinds.is_attribute(kind) && is_attribute(node, text) {
            self.attributes += 1;
        } else if kind == kinds.module && is_inline_module(node) {
            self.open.push(self.modules.len());
            self.modules.push(node);
        } else if kind == kinds.use_declaration || kind == kinds.extern_crate {
            if let Some(block) = self.block_above(0) {
                if kind == kinds.use_declaration {
                    self.block_uses.0.push(node.byte_range());
                }
                self.rebinding.add(block);
            }
        } else if kind == kinds.macro_invocation {
            // An item's or a statement's: in a block, or in an expression
            // statement in one.
            let up = self.ancestors.last().map(Node::kind_id);
            let block = match up == Some(kinds.statement) {
                true => self.block_above(1),
                false => self.block_above(0),
            };
            if let Some(block) = block {
                self.rebinding.add(block);
            }
        } else if kind == kinds.token_tree {
            if self.trees == 0 {
                self.macro_trees.0.push(node.byte_range());
            }
            self.trees += 1;
        }
    }

    /// The bytes of the block `up` nodes above the node being entered,
    /// when that node is one.
    fn block_above(&self, up: usize) -> Option<Range<usize>> {
        let at = self.ancestors.len().checked_sub(up + 1)?;
        let node = self.ancestors.get(at)?;
        (node.kind_id() == KINDS.block).then(|| node.byte_range())
    }

    /// Leave `node`, as [`Reading::enter`] entered it.
    fn leave(&mut self, node: Node<'t>, text: &'t str) {
        let kinds = &*KINDS;
        let kind = node.kind_id();
        if kinds.is_attribute(kind) && is_attribute(node, text) {
            self.attributes = self.attributes.saturating_sub(1);
        } else if kind == kinds.module && is_inline_module(node) {
            self.open.pop();
        } else if kind == kinds.token_tree {
            self.trees = self.trees.saturating_sub(1);
        }
    }

    /// Push the token `node` of `text`; in an attribute, a string after
    /// the segments of each path it spells. A raw identifier is told by
    /// its spelling, whatever node the grammar lexes it as.
    fn push_token(&mut self, node: Node<'t>, text: &'t str) {
        let module = self.open.last().copied();
        if self.attributes > 0 && node.kind().ends_with("string_literal") {
            self.push_spelled(node, text, module);
        }
        let spelled = text_of(node, text);
        let raw = spelled.strip_prefix("r#").filter(|name| is_word(name));
        self.tokens.push(Token {
            text: Cow::Borrowed(raw.unwrap_or(spelled)),
            start: node.start_byte(),
            module,
            raw: raw.is_some(),
        });
    }

    /// Push the segments of each path that `node`, a string in an
    /// attribute of `text` standing in `module`, spells.
    fn push_spelled(&mut self, node: Node<'t>, text: &'t str, module: Option<usize>) {
        let token = |text: Cow<'t, str>| Token {
            text,
            start: node.start_byte(),
            module,
            raw: false,
        };
        match joined(text_of(node, text)) {
            Cow::Borrowed(whole) => {
                let segments = spelled_paths(whole).into_iter();
                self.tokens
                    .extend(segments.map(|s| token(Cow::Borrowed(s))));
            }
            Cow::Owned(whole) => {
                let segments = spelled_paths(&whole).into_iter();
                let owned = segments.map(|s| token(Cow::Owned(s.to_owned())));
                self.tokens.extend(owned);
            }
        }
    }
}

/// `literal`, a string literal as written, each line it continues
/// joined: a line's closing `\` drops the newline and the next line's
/// leading whitespace.
pub(crate) fn joined(literal: &str) -> Cow<'_, str> {
    match literal.contains("\\\n") {
        true => Cow::Owned(literal.split("\\\n").map(str::trim_start).collect()),
        false => Cow::Borrowed(literal),
    }
}

/// A run of letters, digits and underscores: an identifier or a keyword.
pub(super) fn is_word(token: &str) -> bool {
    !token.is_empty() && token.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// The tokens of each path of two segments or more that `literal` spells
/// anywhere in it: segments joined by `::`, with or without whitespace
/// around it (`"crate::x::f"`, `"Vec<super::T>"`, `"T: crate :: Bound"`).
/// A leading `::` is left out, as a path in code is read without it.
fn spelled_paths(literal: &str) -> Vec<&str> {
    let lexemes = lexemes(literal);
    let is_segment = |at: usize| lexemes.get(at).is_some_and(|lexeme| is_word(lexeme));
    let mut found = Vec::new();
    let mut at = 0;
    while at < lexemes.len() {
        let mut end = at + 1;
        while is_segment(at) && lexemes.get(end) == Some(&"::") && is_segment(end + 1) {
            end += 2;
        }
        found.extend(
            lexemes
                .get(at..end)
                .filter(|run| run.len() > 1)
                .unwrap_or_default(),
        );
        at = end;
    }
    found
}

/// `text` cut into words, `::` and single characters, whitespace dropped.
fn lexemes(text: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(first) = rest.chars().next() {
        let word = rest
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(rest.len());
        let length = match word {
            0 if rest.starts_with("::") => 2,
            0 => first.len_utf8(),
            word => word,
        };
        let (lexeme, after) = rest.split_at(length);
        if !lexeme.trim().is_empty() {
            found.push(lexeme);
        }
        rest = after;
    }
    found
}

/// Whether `node` is an attribute, outer or inner, other than `doc`,
/// whose strings are prose: one whose strings are read for paths.
pub(crate) fn is_attribute(node: Node<'_>, text: &str) -> bool {
    let kind = matches!(node.kind(), "attribute_item" | "inner_attribute_item");
    kind && !node
        .named_child(0)
        .and_then(|attribute| attribute.named_child(0))
        .is_some_and(|path| text_of(path, text) == "doc")
}

/// Whether `node` is a `mod name { .. }` item: an inline module.
pub(crate) fn is_inline_module(node: Node<'_>) -> bool {
    node.kind() == "mod_item" && node.child_by_field_name("body").is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_the_reading_tells_apart_is_a_named_kind_of_the_grammar() {
        let grammar: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        for name in Kinds::NAMES {
            let id = grammar.id_for_node_kind(name, true);

            assert_eq!(grammar.node_kind_for_id(id), Some(name), "{name}");
            assert!(grammar.node_kind_is_named(id), "{name}");
        }
        assert_ne!(KINDS.type_parameters, 0);
    }
}
