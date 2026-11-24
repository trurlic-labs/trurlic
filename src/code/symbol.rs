//! The symbol grammar: a symbol is a `::`-separated path of names from the
//! outermost container, and it names every definition whose own path ends
//! with it.
//!
//! In Rust a trait impl is a segment of its own, the trait in angle
//! brackets after the type: `Config::<Display>::fmt`, and
//! `Table::<From<Kind>>` for a trait with arguments. A symbol may leave a
//! trait segment out (`Config::fmt`) and may write a trait without its
//! arguments (`Table::<From>::from`), which then names the impl of every
//! argument; a trait is named by its last path segment, so `<fmt::Display>`
//! is `<Display>`. A definition's own last segment is never left out:
//! `Table` names the type, not its `Table::<Display>` impl. Among several
//! definitions a symbol names, the one whose whole path it is wins, so a
//! module-level `open` is named as `open` beside a `Store::open`. In a
//! language, a symbol whose last segment ends in `()` names a function or
//! method alone (`Store::code_root()` beside the field `Store::code_root`).
//! A language the general resolver reads, Python, splits on `::` alone;
//! so does a data or prose file, whose segments may hold `<`, `>` and
//! parentheses as text (a Markdown heading).

use std::fmt;

use super::kinds::DefinitionKind;

/// One name of a definition's path, or of a symbol.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Segment {
    /// A name, with its runs of whitespace made one space.
    Name(String),
    /// A Rust trait impl: the trait's last path segment, and its arguments
    /// with every space removed (`<Kind>`), when it has any.
    Impl {
        name: String,
        arguments: Option<String>,
    },
}

impl Segment {
    /// A name segment, its whitespace normalized as a symbol's is.
    pub(super) fn name(text: &str) -> Self {
        Self::Name(normalize_spaces(text))
    }

    /// The trait segment of `text`, a trait as source writes it
    /// (`fmt::Display`, `From < Kind >`), without the angle brackets of the
    /// segment.
    pub(super) fn of_trait(text: &str) -> Self {
        let (path, arguments) = match text.find('<') {
            Some(at) => text.split_at(at),
            None => (text, ""),
        };
        let name = path.rsplit("::").next().unwrap_or(path);
        let arguments: String = arguments.chars().filter(|c| !c.is_whitespace()).collect();
        Self::Impl {
            name: name.trim().to_owned(),
            arguments: (!arguments.is_empty()).then_some(arguments),
        }
    }

    /// Whether this segment of a symbol names `defined`, a segment of a
    /// definition's path.
    fn names(&self, defined: &Self) -> bool {
        match (self, defined) {
            (Self::Name(wanted), Self::Name(name)) => wanted == name,
            (
                Self::Impl { name, arguments },
                Self::Impl {
                    name: trait_name,
                    arguments: trait_arguments,
                },
            ) => name == trait_name && (arguments.is_none() || arguments == trait_arguments),
            (Self::Name(_), Self::Impl { .. }) | (Self::Impl { .. }, Self::Name(_)) => false,
        }
    }
}

impl fmt::Display for Segment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Name(name) => f.write_str(name),
            Self::Impl { name, arguments } => {
                write!(f, "<{name}{}>", arguments.as_deref().unwrap_or(""))
            }
        }
    }
}

/// How a kind of file spells its symbols.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Syntax {
    /// `::` outside angle brackets separates, `<Trait>` is a segment, and
    /// a last segment may end in `()`.
    Rust,
    /// `::` separates, every segment is a name, and a last segment may end
    /// in `()`: a language the general resolver reads.
    Code,
    /// `::` separates, and every segment is a name: a data or prose file.
    Plain,
}

/// Why a symbol is not in the grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SymbolFault {
    /// A `::` at an end or beside another, or a segment of whitespace.
    EmptySegment,
    /// An angle bracket that is never closed or closes none.
    UnbalancedBrackets,
    /// A call suffix that holds something, `name(x)`: it is `()` alone.
    CallArguments,
    /// A closing parenthesis that closes no `(`, `name)`.
    UnbalancedParentheses,
    /// A call suffix after a trait segment, which is no function.
    CallOnTrait,
}

impl fmt::Display for SymbolFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::EmptySegment => "has an empty segment",
            Self::UnbalancedBrackets => "has an angle bracket that does not pair",
            Self::CallArguments => "holds something between the parentheses of its `()`",
            Self::UnbalancedParentheses => "has a parenthesis that does not pair",
            Self::CallOnTrait => "ends in `()` after a trait segment",
        })
    }
}

/// Whether a symbol names a function or method alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Call {
    /// No suffix: every kind of definition is named.
    Any,
    /// A last segment ending in `()`: a function or method alone.
    Function,
}

/// A parsed symbol: its segments from the outermost, and whether a `()`
/// ends it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Symbol {
    segments: Vec<Segment>,
    call: Call,
}

impl Symbol {
    /// A symbol of a file of `syntax`. In a language a last segment may
    /// end in `()`, whitespace before it allowed as within a name (`open
    /// ()` is `open()`); `(..)` holding anything is refused, as is `()`
    /// after a trait segment. A data or prose file takes parentheses as
    /// text.
    pub(crate) fn parse(text: &str, syntax: Syntax) -> Result<Self, SymbolFault> {
        let (text, call) = match syntax {
            Syntax::Plain => (text, Call::Any),
            Syntax::Rust | Syntax::Code => call_suffix(text)?,
        };
        let parts = match syntax {
            Syntax::Plain | Syntax::Code => text.split("::").collect(),
            Syntax::Rust => split_outside_brackets(text)?,
        };
        let segments: Vec<Segment> = parts
            .into_iter()
            .map(|part| segment(part.trim(), syntax))
            .collect::<Result<_, _>>()?;
        if call == Call::Function && matches!(segments.last(), Some(Segment::Impl { .. })) {
            return Err(SymbolFault::CallOnTrait);
        }
        Ok(Self { segments, call })
    }

    /// Its segments, from the outermost.
    pub(crate) fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// Whether this symbol may name a definition of `kind`: any kind, or
    /// a function or method alone after `()`.
    pub(crate) const fn admits(&self, kind: DefinitionKind) -> bool {
        match self.call {
            Call::Any => true,
            Call::Function => kind.is_function(),
        }
    }

    /// Whether this symbol names the definition whose path's segments,
    /// from its last, are `from_end`: the path ends with the symbol's
    /// segments, a trait segment of the path the symbol leaves out
    /// skipped, its last segment never.
    pub(crate) fn names<'a>(&self, from_end: impl Iterator<Item = &'a Segment>) -> bool {
        let mut wanted = self.segments.iter().rev().peekable();
        let mut defined = from_end.enumerate();
        while let Some(want) = wanted.peek() {
            let Some((from_end, segment)) = defined.next() else {
                return false;
            };
            if want.names(segment) {
                wanted.next();
            } else if from_end == 0 || matches!(segment, Segment::Name(_)) {
                return false;
            }
        }
        true
    }

    /// Whether this symbol is the whole path whose segments, from its
    /// last, are `from_end`, no segment left out.
    pub(crate) fn is_path_of<'a>(
        &self,
        from_end: impl Iterator<Item = &'a Segment> + Clone,
    ) -> bool {
        // clone: an iterator of references, walked once to count.
        self.segments.len() == from_end.clone().count() && self.names(from_end)
    }
}

/// The definition-path segments of `text`, a name as a file of `syntax`
/// holds it: a `::` in a heading or a key splits it as it splits a
/// symbol, so a symbol can name it.
pub(super) fn name_segments(text: &str) -> impl Iterator<Item = Segment> {
    text.split("::").map(Segment::name)
}

fn segment(part: &str, syntax: Syntax) -> Result<Segment, SymbolFault> {
    if part.is_empty() {
        return Err(SymbolFault::EmptySegment);
    }
    let bracketed = part
        .strip_prefix('<')
        .and_then(|inner| inner.strip_suffix('>'))
        .filter(|_| syntax == Syntax::Rust);
    Ok(match bracketed {
        Some(inner) if !inner.trim().is_empty() => Segment::of_trait(inner),
        Some(_) => return Err(SymbolFault::EmptySegment),
        None => Segment::name(part),
    })
}

/// `text` without its `()`, and whether it ended in one.
fn call_suffix(text: &str) -> Result<(&str, Call), SymbolFault> {
    let Some(open) = text.trim_end().strip_suffix(')') else {
        return Ok((text, Call::Any));
    };
    match open.rsplit_once('(') {
        Some((name, "")) => Ok((name, Call::Function)),
        Some((_, inside)) if !inside.contains(')') => Err(SymbolFault::CallArguments),
        Some(_) | None => Err(SymbolFault::UnbalancedParentheses),
    }
}

/// `text` split at each `::` outside angle brackets; the `>` of an `->`
/// closes nothing.
fn split_outside_brackets(text: &str) -> Result<Vec<&str>, SymbolFault> {
    let bytes = text.as_bytes();
    let (mut depth, mut start, mut parts) = (0_usize, 0, Vec::new());
    let mut at = 0;
    while let Some(&byte) = bytes.get(at) {
        match byte {
            b'<' => depth += 1,
            b'>' if at == 0 || bytes.get(at - 1) != Some(&b'-') => {
                depth = depth
                    .checked_sub(1)
                    .ok_or(SymbolFault::UnbalancedBrackets)?;
            }
            b':' if depth == 0 && bytes.get(at + 1) == Some(&b':') => {
                parts.push(text.get(start..at).unwrap_or_default());
                start = at + 2;
                at += 1;
            }
            _ => {}
        }
        at += 1;
    }
    if depth > 0 {
        return Err(SymbolFault::UnbalancedBrackets);
    }
    parts.push(text.get(start..).unwrap_or_default());
    Ok(parts)
}

/// `text` trimmed, with each run of whitespace in it made one space.
pub(super) fn normalize_spaces(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rust(text: &str) -> Symbol {
        Symbol::parse(text, Syntax::Rust).unwrap()
    }

    fn path(text: &str) -> Vec<Segment> {
        rust(text).segments
    }

    /// Whether `symbol` names `path`.
    fn names(symbol: &str, path: &[Segment]) -> bool {
        rust(symbol).names(path.iter().rev())
    }

    #[test]
    fn a_symbol_names_each_definition_its_path_ends_with() {
        let method = path("store::Store::open");

        assert!(names("Store::open", &method));
        assert!(names("open", &method));
        assert!(names("store::Store::open", &method));
        assert!(!names("Store", &method));
        assert!(!names("Other::open", &method));
        assert!(!names("x::store::Store::open", &method));
    }

    #[test]
    fn a_trait_segment_may_be_left_out_but_never_the_last() {
        let method = path("Config::<Display>::fmt");
        let block = path("Config::<Display>");

        assert!(names("Config::fmt", &method));
        assert!(names("Config::<Display>::fmt", &method));
        assert!(names("Config::<fmt::Display>::fmt", &method));
        assert!(!names("Config::<Debug>::fmt", &method));
        assert!(names("Config::<Display>", &block));
        assert!(!names("Config", &block));
        assert!(!names("Config::<Display>::fmt", &path("Config::fmt")));
    }

    #[test]
    fn a_trait_without_arguments_names_the_impl_of_every_argument() {
        let from_kind = path("Table::<From<Kind>>::from");

        assert!(names("Table::<From>::from", &from_kind));
        assert!(names("Table::<From < Kind >>::from", &from_kind));
        assert!(!names("Table::<From<Site>>::from", &from_kind));
        assert!(!names("From<Kind>", &path("Table::<From<Kind>>")));
    }

    #[test]
    fn a_symbol_is_the_whole_path_only_of_a_definition_it_leaves_nothing_of() {
        assert!(rust("open").is_path_of(path("open").iter().rev()));
        assert!(!rust("open").is_path_of(path("Store::open").iter().rev()));
        assert!(rust("Table::<From>").is_path_of(path("Table::<From<Kind>>").iter().rev()));
        assert!(!rust("Table::fmt").is_path_of(path("Table::<Display>::fmt").iter().rev()));
    }

    #[test]
    fn a_rust_symbol_splits_only_outside_angle_brackets() {
        assert_eq!(
            path("A::<From<b::C>>::f"),
            vec![
                Segment::name("A"),
                Segment::Impl {
                    name: "From".into(),
                    arguments: Some("<b::C>".into())
                },
                Segment::name("f"),
            ]
        );
        assert_eq!(path("A::<Fn(u8) -> u8>").len(), 2);
    }

    #[test]
    fn a_symbol_out_of_the_grammar_is_refused_with_its_fault() {
        for (text, fault) in [
            ("", SymbolFault::EmptySegment),
            ("a::", SymbolFault::EmptySegment),
            ("::a", SymbolFault::EmptySegment),
            ("a:: ::b", SymbolFault::EmptySegment),
            ("a::<>", SymbolFault::EmptySegment),
            ("a::<B", SymbolFault::UnbalancedBrackets),
            ("a>::b", SymbolFault::UnbalancedBrackets),
        ] {
            assert_eq!(Symbol::parse(text, Syntax::Rust), Err(fault), "{text:?}");
        }
    }

    #[test]
    fn a_call_suffix_is_empty_parentheses_after_a_name_in_a_kind_with_functions() {
        let open = rust("Store::open()");
        let spaced = rust("Store::open ()");

        assert_eq!(open, spaced);
        assert_eq!(open.segments, rust("Store::open").segments);
        assert!(open.admits(DefinitionKind::Method));
        assert!(open.admits(DefinitionKind::Function));
        assert!(!open.admits(DefinitionKind::Field));
        assert!(rust("Store::open").admits(DefinitionKind::Field));
        for (text, syntax, fault) in [
            ("open(x)", Syntax::Rust, SymbolFault::CallArguments),
            ("open)", Syntax::Rust, SymbolFault::UnbalancedParentheses),
            (
                "A::<Fn(u8)>::f)",
                Syntax::Rust,
                SymbolFault::UnbalancedParentheses,
            ),
            ("Table::<Display>()", Syntax::Rust, SymbolFault::CallOnTrait),
            ("::()", Syntax::Rust, SymbolFault::EmptySegment),
        ] {
            assert_eq!(Symbol::parse(text, syntax), Err(fault), "{text:?}");
        }
        let python = Symbol::parse("Store::open()", Syntax::Code).unwrap();
        assert_eq!(python.segments, rust("Store::open").segments);
        assert!(!python.admits(DefinitionKind::Variable));
        assert_eq!(
            Symbol::parse("open(x)", Syntax::Code),
            Err(SymbolFault::CallArguments)
        );
        assert_eq!(
            Symbol::parse("A::<B>", Syntax::Code).unwrap().segments,
            [Segment::name("A"), Segment::name("<B>")]
        );
        for heading in ["Calling run()", "Install (Linux)", "g(x)"] {
            let plain = Symbol::parse(heading, Syntax::Plain).unwrap();
            assert_eq!(plain.segments, [Segment::name(heading)], "{heading:?}");
            assert!(plain.admits(DefinitionKind::Heading));
        }
    }

    #[test]
    fn a_plain_symbol_takes_angle_brackets_as_text() {
        let heading = Symbol::parse("Install::<details> and  more", Syntax::Plain).unwrap();

        assert_eq!(
            heading.segments,
            vec![
                Segment::name("Install"),
                Segment::name("<details> and more")
            ]
        );
        assert!(Symbol::parse("a < b", Syntax::Plain).is_ok());
    }
}
