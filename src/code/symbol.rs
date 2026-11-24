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
//! module-level `open` is named as `open` beside a `Store::open`. Other
//! kinds split on `::` alone, so a Markdown heading may hold `<` and `>`.

use std::fmt;

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
    /// `::` outside angle brackets separates, and `<Trait>` is a segment.
    Rust,
    /// `::` separates, and every segment is a name.
    Plain,
}

/// Why a symbol is not in the grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SymbolFault {
    /// A `::` at an end or beside another, or a segment of whitespace.
    EmptySegment,
    /// An angle bracket that is never closed or closes none.
    UnbalancedBrackets,
}

impl fmt::Display for SymbolFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::EmptySegment => "has an empty segment",
            Self::UnbalancedBrackets => "has an angle bracket that does not pair",
        })
    }
}

/// A parsed symbol: its segments from the outermost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Symbol(Vec<Segment>);

impl Symbol {
    pub(crate) fn parse(text: &str, syntax: Syntax) -> Result<Self, SymbolFault> {
        let parts = match syntax {
            Syntax::Plain => text.split("::").collect(),
            Syntax::Rust => split_outside_brackets(text)?,
        };
        parts
            .into_iter()
            .map(|part| segment(part.trim(), syntax))
            .collect::<Result<_, _>>()
            .map(Self)
    }

    /// Whether this symbol names the definition whose path's segments,
    /// from its last, are `from_end`: the path ends with the symbol's
    /// segments, a trait segment of the path the symbol leaves out
    /// skipped, its last segment never.
    pub(crate) fn names<'a>(&self, from_end: impl Iterator<Item = &'a Segment>) -> bool {
        let mut wanted = self.0.iter().rev().peekable();
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
        self.0.len() == from_end.clone().count() && self.names(from_end)
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
        rust(text).0
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
    fn a_plain_symbol_takes_angle_brackets_as_text() {
        let heading = Symbol::parse("Install::<details> and  more", Syntax::Plain).unwrap();

        assert_eq!(
            heading.0,
            vec![
                Segment::name("Install"),
                Segment::name("<details> and more")
            ]
        );
        assert!(Symbol::parse("a < b", Syntax::Plain).is_ok());
    }
}
