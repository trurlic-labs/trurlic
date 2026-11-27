//! The paths a file's tokens name ([`Reading::paths`]): every path or
//! `use` tree that starts at a token, each entry of a `use` list one. A
//! path is a chain of its segments' tokens, each linked to the one before
//! it ([`Paths::before`]), so the entries of a list share the tokens of
//! its prefix and a list nested as deep as the file allows is read in one
//! pass, from an explicit stack.

use super::reading::{Reading, is_word};

/// The paths of a file's tokens.
pub(crate) struct Paths {
    pub(crate) named: Vec<Named>,
    /// For each token, the token of the segment before it in its path;
    /// `None` for a path's first segment and for a token of no path.
    before: Vec<Option<usize>>,
}

/// A path a file's tokens name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Named {
    /// The token of its first segment, which gives its module and its
    /// byte.
    pub(crate) first: usize,
    /// The token of its last segment: following [`Paths::before`] from it
    /// reaches `first`.
    pub(crate) last: usize,
    pub(crate) kind: PathKind,
    pub(crate) end: End,
}

/// Where a path stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PathKind {
    /// In code, a macro's tokens or an attribute's string.
    Code,
    /// The tree of a `use` with no visibility.
    Use,
    /// The tree of a `use` a visibility opens (`pub use`, `pub(crate)
    /// use`, `pub(in ..) use`): a re-export.
    Reexport,
    /// The tree of a `use` in a block, a function's body or any other,
    /// which binds its names in that block alone.
    InBlock,
    /// The tree of a `use` among a macro's tokens, which the grammar reads
    /// as no declaration: it binds nothing a reading can place, and is a
    /// re-export where a visibility opens it.
    InMacro { exported: bool },
}

/// How a path ends after its last segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum End {
    Path,
    /// `::*`: a glob of the module its segments name.
    Glob,
    /// `as name`: the token of the alias, `_` included.
    Alias(usize),
}

/// Whether `word` is a strict keyword but `crate`, `self`, `super` and
/// `Self`, or `_`: a word no path's segment is.
fn is_strict(word: &str) -> bool {
    matches!(
        word,
        "_" | "as"
            | "async"
            | "await"
            | "break"
            | "const"
            | "continue"
            | "dyn"
            | "else"
            | "enum"
            | "extern"
            | "false"
            | "fn"
            | "for"
            | "if"
            | "impl"
            | "in"
            | "let"
            | "loop"
            | "match"
            | "mod"
            | "move"
            | "mut"
            | "pub"
            | "ref"
            | "return"
            | "static"
            | "struct"
            | "trait"
            | "true"
            | "type"
            | "unsafe"
            | "use"
            | "where"
            | "while"
    )
}

/// What a segment of a path names by its spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Spelled<'s> {
    /// `crate` or `$crate`: the crate root.
    Crate,
    /// `self`: the module it stands in.
    Current,
    /// `super`: the module above.
    Parent,
    /// Any other name.
    Name(&'s str),
}

impl<'s> Spelled<'s> {
    pub(super) fn of(segment: &'s str) -> Self {
        match segment {
            "crate" | "$crate" => Self::Crate,
            "self" => Self::Current,
            "super" => Self::Parent,
            name => Self::Name(name),
        }
    }
}

/// Where a path's segment leads, read in a module ([`lead`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Led<M> {
    /// To a module: the crate root, the module itself or the one above.
    Module(M),
    /// Above the crate root, where no module is.
    AboveRoot,
    /// Nowhere by its spelling: a name, which the module resolves.
    Name,
}

/// Where `segment`, read in `module`, leads by its spelling: `crate` and
/// `$crate` to `root`, `self` to `module`, `super` to the module above,
/// which `parent` gives. The one rule of a path's `crate`, `self` and
/// `super`, whatever a module is to the caller.
pub(crate) fn lead<M>(
    segment: &str,
    module: M,
    root: M,
    parent: impl FnOnce(M) -> Option<M>,
) -> Led<M> {
    match Spelled::of(segment) {
        Spelled::Crate => Led::Module(root),
        Spelled::Current => Led::Module(module),
        Spelled::Parent => parent(module).map_or(Led::AboveRoot, Led::Module),
        Spelled::Name(_) => Led::Name,
    }
}

impl Paths {
    /// Push the path `prefix` holds, of `kind`, which ends as `end`; none
    /// when it ends at no segment. A word alone in code names an item in
    /// scope, which its `use` or its definition accounts for: no path.
    fn push(&mut self, prefix: Prefix, kind: PathKind, end: Option<End>) {
        let alone = kind == PathKind::Code && prefix.first == prefix.last;
        if let (Some(end), Some(first), Some(last)) = (end, prefix.first, prefix.last)
            && !alone
        {
            self.named.push(Named {
                first,
                last,
                kind,
                end,
            });
        }
    }

    /// The token of the segment before `token` in its path.
    pub(crate) fn before(&self, token: usize) -> Option<usize> {
        self.before.get(token).copied().flatten()
    }
}

impl Reading<'_> {
    /// Each path the tokens name, in order: a path starts at a word,
    /// `$crate` or `::` that no `::` or `.` comes before and no
    /// visibility holds, and the tokens it takes start none of their own.
    /// In code, a word alone is none.
    pub(crate) fn paths(&self) -> Paths {
        let mut paths = Paths {
            named: Vec::new(),
            before: vec![None; self.tokens.len()],
        };
        let mut at = 0;
        while at < self.tokens.len() {
            at = match self.starts_path(at) {
                true => self.read_path(at, self.kind_at(at), &mut paths),
                false => at + 1,
            };
        }
        paths
    }

    /// The text of the token at `at`; empty past the tokens.
    fn text(&self, at: usize) -> &str {
        self.tokens.get(at).map_or("", |token| &token.text)
    }

    /// Whether the token at `at` can be a segment of a path: a macro's
    /// metavariable (`$crate`, `$m`), or a word but a strict keyword
    /// ([`is_strict`]) not written raw, by its spelling whatever node the
    /// grammar lexes it as. So `use ::std::fs` names `std::fs`, while
    /// `r#type` and a contextual keyword (`gen`, `default`, `union`,
    /// nameless inside a macro's tokens) are segments. A metavariable but
    /// `$crate` is a segment no module or item is named (`crate::$m::f`).
    fn is_segment(&self, at: usize) -> bool {
        self.tokens.get(at).is_some_and(|token| {
            let text = token.text.as_ref();
            let metavariable = text.strip_prefix('$').is_some_and(is_word);
            metavariable || (is_word(text) && (token.raw || !is_strict(text)))
        })
    }

    /// The text of the token `by` before `at`; empty before the first.
    fn back(&self, at: usize, by: usize) -> &str {
        at.checked_sub(by).map_or("", |at| self.text(at))
    }

    /// Whether a path starts at `at`: a name or `::`, or the `{` a `use`
    /// tree opens with (`use {crate::a, crate::b};`). A tuple field's
    /// parenthesised type (`pub (crate::x::T)`) is a path; `pub(crate)`,
    /// `pub(super)`, `pub(self)` and `pub(in ..)` are not.
    fn starts_path(&self, at: usize) -> bool {
        let first = self.text(at);
        if first == "{" {
            return self.back(at, 1) == "use";
        }
        let word = first == "::" || self.is_segment(at);
        // A lifetime's or a label's name is no segment (`&'a ::std::fs::File`).
        let continues = matches!(self.back(at, 1), "::" | "." | "'");
        let restricted = matches!(first, "crate" | "super" | "self")
            && self.back(at, 1) == "("
            && self.back(at, 2) == "pub"
            && self.text(at + 1) == ")";
        let within = self.back(at, 1) == "in" && self.back(at, 2) == "(";
        let visibility = restricted || (within && self.back(at, 3) == "pub");
        word && !continues && !visibility
    }

    /// Where the path at `at` stands. A visibility's path before a `use`
    /// is read back over its words and `::` alone, so each token is read
    /// back at most once. A `use` in a block is one whatever its
    /// visibility.
    fn kind_at(&self, at: usize) -> PathKind {
        let exported = match (self.back(at, 1), self.back(at, 2)) {
            ("use", "pub") => true,
            ("use", ")") => {
                let mut by = 3;
                while self.back(at, by) == "::" || is_word(self.back(at, by)) {
                    by += 1;
                }
                self.back(at, by) == "(" && self.back(at, by + 1) == "pub"
            }
            ("use", _) => false,
            _ => return PathKind::Code,
        };
        let start = self.tokens.get(at).map_or(0, |token| token.start);
        if self.block_uses.hold(start) {
            PathKind::InBlock
        } else if self.macro_trees.hold(start) {
            PathKind::InMacro { exported }
        } else if exported {
            PathKind::Reexport
        } else {
            PathKind::Use
        }
    }

    /// Read the path or `use` tree from `at` into `paths`, each entry of a
    /// list a path of its own; the token past it. An entry that starts
    /// with no segment, `{` or `*` names nothing and is passed over.
    fn read_path(&self, at: usize, kind: PathKind, paths: &mut Paths) -> usize {
        let mut at = at + usize::from(self.text(at) == "::");
        // The prefix each open list's entries continue, innermost last.
        let mut lists: Vec<Prefix> = Vec::new();
        let mut prefix = Prefix {
            last: None,
            first: None,
        };
        loop {
            let end = match self.text(at) {
                "{" => {
                    lists.push(prefix);
                    at += 1;
                    continue;
                }
                "*" => {
                    at += 1;
                    Some(End::Glob)
                }
                _ if self.is_segment(at) => {
                    prefix = prefix.then(at, paths);
                    at += 1;
                    match self.after_segment(&mut at, kind) {
                        None => continue,
                        end => end,
                    }
                }
                _ => None,
            };
            paths.push(prefix, kind, end);
            let Some(next) = self.next_entry(&mut at, &mut lists) else {
                return at;
            };
            prefix = next;
        }
    }

    /// How a path of `kind` ends after a segment, the token at `at` being
    /// the one after it, which is left past what is taken; `None` when the
    /// path goes on (`::` and a segment, or in a `use` tree a list or a
    /// glob). A `::` before anything else (`a::<T>`) ends it. A list, a
    /// glob and an alias are a `use` tree's alone: in code, `as` ends the
    /// path and is left for what follows it, a cast's type or a trait (`x
    /// as crate::T`, `<B as std::io::Write>::f`).
    fn after_segment(&self, at: &mut usize, kind: PathKind) -> Option<End> {
        match self.text(*at) {
            "::" => {
                *at += 1;
                let next = self.text(*at);
                let tree = kind != PathKind::Code && matches!(next, "{" | "*");
                let goes_on = tree || self.is_segment(*at);
                (!goes_on).then_some(End::Path)
            }
            "as" if kind != PathKind::Code => {
                *at += 2;
                Some(End::Alias(*at - 1))
            }
            _ => Some(End::Path),
        }
    }

    /// Past the entry that ends at `at`: the prefix of the next entry of
    /// the innermost open list, its closing `}`s passed; `None` once no
    /// list is open or the tokens end. A token that is neither `,` nor
    /// `}` nor the start of an entry is passed over.
    fn next_entry(&self, at: &mut usize, lists: &mut Vec<Prefix>) -> Option<Prefix> {
        loop {
            let prefix = *lists.last()?;
            match self.text(*at) {
                "" => return None,
                "}" => {
                    lists.pop();
                    *at += 1;
                }
                "," => {
                    *at += 1;
                    return Some(prefix);
                }
                "{" | "*" => return Some(prefix),
                _ if self.is_segment(*at) => return Some(prefix),
                _ => *at += 1,
            }
        }
    }
}

/// The segments of a path read so far: the token of its last and of its
/// first, `None` before its first.
#[derive(Clone, Copy)]
struct Prefix {
    last: Option<usize>,
    first: Option<usize>,
}

impl Prefix {
    /// This prefix and the segment at the token `at` after it, which
    /// `paths` links to the last of this one.
    fn then(self, at: usize, paths: &mut Paths) -> Self {
        if let Some(before) = paths.before.get_mut(at) {
            *before = self.last;
        }
        Self {
            last: Some(at),
            first: self.first.or(Some(at)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::parse::parser::parse;

    /// Each path `text` names, as its kind, its segments joined by `::`
    /// and how it ends.
    fn read(text: &str) -> Vec<String> {
        let tree = parse(text, &tree_sitter_rust::LANGUAGE.into()).unwrap();
        let reading = Reading::of(tree.root_node(), text);
        let paths = reading.paths();
        let spelled = |at: usize| reading.tokens[at].text.to_string();
        paths
            .named
            .iter()
            .map(|named| {
                let mut segments = Vec::new();
                let mut at = Some(named.last);
                while let Some(token) = at {
                    segments.push(spelled(token));
                    at = paths.before(token);
                }
                segments.reverse();
                let end = match named.end {
                    End::Path => String::new(),
                    End::Glob => "::*".to_owned(),
                    End::Alias(alias) => format!(" as {}", spelled(alias)),
                };
                format!("{:?} {}{end}", named.kind, segments.join("::"))
            })
            .collect()
    }

    #[test]
    fn each_form_of_a_path_is_read_as_the_chain_of_its_segments() {
        let text = "use crate::a::{b, c::{self, d as e}, *};\n\
             pub(crate) use super::f;\n\
             pub(in crate::x) use self::g as _;\n\
             pub use ::std::h;\n\
             fn f() { crate::i::j(); crate::r#type::k(); T::<crate::o::P>::q(); }\n\
             struct S(pub (crate::t::U));\n\
             #[serde(with = \"crate::v::w\", bound = \"T: crate:\\\n    :x::Y\")]\n\
             struct Z;\n\
             /// crate::doc::Comment\n\
             #[doc = \"crate::doc::Attribute\"]\n\
             macro_rules! m { () => { $crate::y::z() } }\n\
             pub(crate) use {crate::Error as E, crate::r::S};\n\
             fn g() { m!(crate::gen::x()); }\n";

        assert_eq!(
            read(text),
            [
                "Use crate::a::b",
                "Use crate::a::c::self",
                "Use crate::a::c::d as e",
                "Use crate::a::*",
                "Reexport super::f",
                "Reexport self::g as _",
                "Reexport std::h",
                "Code crate::i::j",
                "Code crate::type::k",
                "Code crate::o::P",
                "Code crate::t::U",
                "Code crate::v::w",
                "Code crate::x::Y",
                "Code $crate::y::z",
                "Reexport crate::Error as E",
                "Reexport crate::r::S",
                "Code crate::gen::x",
            ]
        );
    }

    #[test]
    fn as_in_code_ends_a_path_and_the_type_or_trait_after_it_is_a_path_of_its_own() {
        let text =
            "fn f(x: u8) -> T { x as crate::budget::Bytes; <B as std::io::Write>::flush(b); }";

        assert_eq!(
            read(text),
            ["Code crate::budget::Bytes", "Code std::io::Write"]
        );
    }

    #[test]
    fn a_lifetimes_or_a_labels_name_starts_no_path() {
        let text =
            "fn f<'a>(x: &'a ::std::fs::File) { 'outer: loop { break 'outer ::std::fs::read(); } }";

        assert_eq!(read(text), ["Code std::fs::File", "Code std::fs::read"]);
    }

    #[test]
    fn a_macros_metavariable_is_a_segment() {
        let text = "macro_rules! m { ($m:ident) => { crate::$m::g(); use $crate::$m; crate::$($p)::*; }; }";

        assert_eq!(
            read(text),
            [
                "Code crate::$m::g",
                "InMacro { exported: false } $crate::$m"
            ]
        );
    }

    #[test]
    fn a_raw_name_is_read_as_its_name_whatever_node_the_grammar_makes_of_it() {
        let text = "fn f() -> crate::m::r#Type { crate::r#type::f() }";

        assert_eq!(read(text), ["Code crate::m::Type", "Code crate::type::f"]);
    }

    #[test]
    fn an_inner_attributes_string_is_read_for_its_paths() {
        assert_eq!(
            read("#![cfg_attr(x, with = \"crate::q::r\")]\n"),
            ["Code crate::q::r"]
        );
    }

    #[test]
    fn a_use_among_a_macros_tokens_is_read_as_one() {
        let text =
            "fn f() { vec![{ use crate::y::x; x::g() }]; }\nkeep! { pub(crate) use crate::s::K; }";

        assert_eq!(
            read(text),
            [
                "InMacro { exported: false } crate::y::x",
                "Code x::g",
                "InMacro { exported: true } crate::s::K",
            ]
        );
    }

    #[test]
    fn a_use_in_a_block_is_read_as_one_whatever_its_visibility() {
        let text = "use crate::a;\nfn f() { use crate::b::{c, d as e}; pub use crate::f; }";

        assert_eq!(
            read(text),
            [
                "Use crate::a",
                "InBlock crate::b::c",
                "InBlock crate::b::d as e",
                "InBlock crate::f",
            ]
        );
    }

    #[test]
    fn a_list_nested_as_deep_as_a_file_allows_is_read_with_no_call_stack_per_level() {
        let depth = 50_000;
        let text = format!("use a::{}b{};", "a::{".repeat(depth), "}".repeat(depth));

        let read = read(&text);

        assert_eq!(read.len(), 1);
        assert_eq!(read[0].matches("::").count(), depth + 1);
    }

    #[test]
    fn a_token_a_list_cannot_hold_is_passed_over_and_an_unclosed_list_ends_with_the_file() {
        assert_eq!(
            read("use a::{+, b, };\nuse c::{d, "),
            ["Use a::b", "Use c::d"]
        );
    }
}
