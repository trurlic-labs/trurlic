//! What the index holds of a Rust doc: the first paragraph of a file's
//! or a definition's doc, as one line of text within
//! [`MAX_DOC_BYTES`] ([`first_paragraph`]).

use tree_sitter::Node;

use super::marks;
use crate::code::index::{Doc, MAX_DOC_BYTES};
use crate::code::parse::parser::text_of;

/// The first paragraph of the doc of an item whose attributes and outer
/// doc comments are `outer`, in order, and whose body is `body`; of a
/// file when `body` is its root and `outer` is empty. `None` when no doc
/// line that is not blank is read.
///
/// The doc is read as rustdoc reads it: the outer docs, then the inner
/// docs that start the body, before its first item or statement; a
/// file's shebang line comes before its inner docs and is passed over. A `///`
/// or `//!` comment is one line, a `/** */` or `/*! */` comment its lines,
/// each losing a leading `*` when every line after its first that is not
/// blank starts with one. A `#[doc = "..."]` or `#![doc = "..."]` gives
/// the lines of its string, raw or not, as written, escapes kept. A doc
/// attribute of any other value (`include_str!`) ends the read, its text
/// being in another file. Neither a doc under `cfg_attr`, which holds only
/// under its configuration, nor an attribute that is no doc, nor a plain
/// comment, is read.
///
/// The paragraph runs from the first line that is not blank to the line
/// before the next blank one, a line being blank when nothing but
/// whitespace is left of it. Its lines are trimmed, the space after
/// `///` with them, and joined by one space; a control character within a
/// line, a lone `\r` among them, is kept as written, for a reader to
/// escape as its output needs. Past [`MAX_DOC_BYTES`] it is
/// cut at the last char boundary within them, and the doc is marked cut.
/// No Markdown is read. No line past the paragraph's end is taken, so the
/// work is linear in the bytes of the docs.
pub(super) fn first_paragraph<'n>(
    outer: &[Node<'n>],
    body: Option<Node<'n>>,
    text: &str,
) -> Option<Doc> {
    let inner = body.map(inner_docs).unwrap_or_default();
    let mut paragraph = Paragraph::default();
    for node in outer.iter().chain(&inner) {
        let goes_on = match fragment(*node, text) {
            Fragment::Lines { text, starred } => paragraph.take_lines(text, starred),
            Fragment::NoDoc => true,
            Fragment::Elsewhere => false,
        };
        if !goes_on {
            break;
        }
    }
    Doc::new(paragraph.text, paragraph.cut)
}

/// The inner doc comments and inner attributes that start `body`, before
/// its first node that is neither those, nor a comment, nor a shebang.
fn inner_docs(body: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = body.walk();
    body.named_children(&mut cursor)
        .take_while(|node| {
            is_inner_attribute(*node) || is_comment(*node) || node.kind() == "shebang"
        })
        .filter(|node| is_inner_attribute(*node) || node.child_by_field_name("inner").is_some())
        .collect()
}

fn is_inner_attribute(node: Node<'_>) -> bool {
    node.kind() == "inner_attribute_item"
}

fn is_comment(node: Node<'_>) -> bool {
    matches!(node.kind(), "line_comment" | "block_comment")
}

/// What one node of an item's attributes and docs gives its doc.
enum Fragment<'t> {
    /// Lines of the doc, each losing a leading `*` when `starred`.
    Lines { text: &'t str, starred: bool },
    /// Nothing: an attribute that is no doc, a plain comment.
    NoDoc,
    /// A doc whose text is in another file.
    Elsewhere,
}

/// What `node`, a comment or an attribute, gives its item's doc.
fn fragment<'t>(node: Node<'_>, text: &'t str) -> Fragment<'t> {
    let block = match node.kind() {
        "line_comment" => false,
        "block_comment" => true,
        "attribute_item" | "inner_attribute_item" => return doc_attribute(node, text),
        // Every other node: no doc.
        _ => return Fragment::NoDoc,
    };
    let Some(doc) = node.child_by_field_name("doc") else {
        return Fragment::NoDoc;
    };
    let lines = text_of(doc, text);
    match block {
        true => Fragment::Lines {
            text: lines,
            starred: is_starred(lines),
        },
        // A line comment's text ends with the line's end, which ends no
        // paragraph.
        false => Fragment::Lines {
            text: lines.strip_suffix('\n').unwrap_or(lines),
            starred: false,
        },
    }
}

/// What `attribute`, an `attribute_item` or `inner_attribute_item`, gives
/// its item's doc: the string of a `doc = "..."`.
fn doc_attribute<'t>(attribute: Node<'_>, text: &'t str) -> Fragment<'t> {
    let mut cursor = attribute.walk();
    let Some(inner) = attribute
        .named_children(&mut cursor)
        .find(|child| child.kind() == "attribute")
    else {
        return Fragment::NoDoc;
    };
    let is_doc = inner
        .named_child(0)
        .is_some_and(|path| path.kind() == "identifier" && text_of(path, text) == "doc");
    let Some(value) = inner.child_by_field_name("value").filter(|_| is_doc) else {
        return Fragment::NoDoc;
    };
    match value.kind() {
        "string_literal" | "raw_string_literal" => Fragment::Lines {
            text: marks::string_value(value, text),
            starred: false,
        },
        // Every other value is an expression whose text is elsewhere.
        _ => Fragment::Elsewhere,
    }
}

/// Whether the lines of `block`, a block doc's text, lose a leading `*`:
/// every line after the first that is not blank starts with one, and
/// there is one such line.
fn is_starred(block: &str) -> bool {
    let mut rest = block
        .split('\n')
        .skip(1)
        .map(str::trim_start)
        .filter(|line| !line.trim_end().is_empty())
        .peekable();
    rest.peek().is_some() && rest.all(|line| line.starts_with('*'))
}

/// The first paragraph of a doc as its lines are taken.
#[derive(Default)]
struct Paragraph {
    text: String,
    cut: bool,
}

impl Paragraph {
    /// Take the lines of `text`, each losing a leading `*` when `starred`;
    /// whether the paragraph goes on past them.
    fn take_lines(&mut self, text: &str, starred: bool) -> bool {
        text.split('\n').all(|line| {
            let line = line.trim();
            let line = match starred {
                true => line.strip_prefix('*').map_or(line, str::trim_start),
                false => line,
            };
            self.take(line)
        })
    }

    /// Take `line`, trimmed: a blank one ends the paragraph once it has
    /// begun, and one past the room left is cut into it. Whether the
    /// paragraph goes on.
    fn take(&mut self, line: &str) -> bool {
        if line.is_empty() {
            return self.text.is_empty();
        }
        let separator = usize::from(!self.text.is_empty());
        let room = MAX_DOC_BYTES.saturating_sub(self.text.len() + separator);
        let fits = prefix_within(line, room);
        if !fits.is_empty() && separator == 1 {
            self.text.push(' ');
        }
        self.text.push_str(fits);
        self.cut = fits.len() < line.len();
        !self.cut
    }
}

/// The longest start of `line` within `bytes` that ends on a char
/// boundary.
fn prefix_within(line: &str, bytes: usize) -> &str {
    let end = (0..=bytes.min(line.len()))
        .rev()
        .find(|&at| line.is_char_boundary(at))
        .unwrap_or(0);
    line.get(..end).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use crate::code::index::{Doc, FileKind, Scopes};
    use crate::code::testing;

    /// A doc as a test reads it: its text, and whether it is cut.
    type Read = Option<(String, bool)>;

    fn read(doc: Option<&Doc>) -> Read {
        doc.map(|doc| (doc.text().to_owned(), doc.is_cut()))
    }

    fn whole(text: &str) -> Read {
        Some((text.to_owned(), false))
    }

    /// The doc of `text`, a Rust file, then each of its definitions' by
    /// its path, in the index's order.
    fn docs(text: &str) -> (Read, Vec<(String, Read)>) {
        let index = testing::index(FileKind::Rust, text.as_bytes()).unwrap();
        let scopes = Scopes(&index.scopes);
        let definitions = index
            .definitions
            .iter()
            .map(|d| (testing::path(scopes, d.scope), read(d.doc.as_ref())))
            .collect();
        (read(index.doc.as_ref()), definitions)
    }

    #[test]
    fn the_doc_of_a_file_and_of_each_definition_is_its_first_paragraph() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/code/crates/ledger/src/docs.rs"
        );
        let (file, mut definitions) = docs(&std::fs::read_to_string(path).unwrap());
        let (name, verbose) = definitions.remove(definitions.len() - 2);

        assert_eq!(
            file,
            whole(
                "Docs as the index reads them: this first paragraph is the file's, its lines joined by one space."
            )
        );
        assert_eq!(
            definitions,
            [
                ("Journal".to_owned(), whole("A journal of entries.")),
                (
                    "Journal::entries".to_owned(),
                    whole("The entries, oldest first.")
                ),
                ("Journal::count".to_owned(), None),
                (
                    "Balance".to_owned(),
                    whole("A balance, written as a block doc whose lines each start with a star.")
                ),
                ("Balance::Even".to_owned(), whole("Nothing owed.")),
                ("Balance::Owed".to_owned(), None),
                (
                    "audit".to_owned(),
                    whole("An audit, documented by an attribute, and by a line after it.")
                ),
                ("undocumented".to_owned(), None),
                (
                    "terms".to_owned(),
                    whole("The terms of the journal. Its inner doc follows the outer one.")
                ),
            ]
        );
        assert_eq!(
            (name.as_str(), verbose.map(|(_, cut)| cut)),
            ("verbose", Some(true))
        );
    }

    /// The doc of the one definition of `text`.
    fn doc_of_one(text: &str) -> Read {
        let (_, mut definitions) = docs(text);
        assert_eq!(definitions.len(), 1, "{text}");
        definitions.remove(0).1
    }

    #[test]
    fn a_doc_is_read_from_each_form_rustdoc_reads_and_from_no_other() {
        for (text, doc) in [
            (
                "/** Block\n    without stars. */\nfn f() {}",
                whole("Block without stars."),
            ),
            (
                "#[doc = r\"Raw \\n as written.\"]\nfn f() {}",
                whole("Raw \\n as written."),
            ),
            (
                "#[doc = \"A blank line\n\nin a string ends it.\"]\nfn f() {}",
                whole("A blank line"),
            ),
            (
                "#[cfg_attr(docsrs, doc = \"Configured.\")]\n#[doc(hidden)]\n/// Read.\nfn f() {}",
                whole("Read."),
            ),
            ("//// Four slashes.\n/***/\n/**/\nfn f() {}", None),
            (
                "/// Before.\n#[doc = include_str!(\"x.md\")]\n/// After.\nfn f() {}",
                whole("Before."),
            ),
            (
                "#[doc = include_str!(\"x.md\")]\n/// After.\nfn f() {}",
                None,
            ),
            (
                "///\n///   Blank lines and indentation\n///\tgo.   \n///\n/// Next.\nfn f() {}",
                whole("Blank lines and indentation go."),
            ),
            (
                "/// One.\r\n/// Two.\r\n///\r\nfn f() {}",
                whole("One. Two."),
            ),
            (
                "fn f() {\n    //! Inner.\n    let x = 1;\n}",
                whole("Inner."),
            ),
            ("/// a\rb\nfn f() {}", whole("a\rb")),
        ] {
            assert_eq!(doc_of_one(text), doc, "{text}");
        }
        let shebang = "#!/usr/bin/env rust-script\n//! File doc.\nfn f() {}\n";
        assert_eq!(docs(shebang).0, whole("File doc."));
    }

    #[test]
    fn every_definition_but_a_reexport_holds_its_doc() {
        let text = "/// M.\nmacro_rules! m { () => {} }\n/// T.\npub trait T {\n    /// A.\n    type A;\n    /// F.\n    fn f();\n}\n/// I.\nimpl T for u8 {\n    /// IA.\n    type A = u8;\n    /// IF.\n    fn f() {}\n}\n/// C.\nconst C: u8 = 0;\n/// R.\npub use a::b;\n";
        let (_, definitions) = docs(text);
        let found: Vec<(&str, Option<&str>)> = definitions
            .iter()
            .map(|(path, doc)| (path.as_str(), doc.as_ref().map(|(text, _)| text.as_str())))
            .collect();

        assert_eq!(
            found,
            [
                ("m", Some("M.")),
                ("T", Some("T.")),
                ("T::A", Some("A.")),
                ("T::f", Some("F.")),
                ("u8::<T>", Some("I.")),
                ("u8::<T>::A", Some("IA.")),
                ("u8::<T>::f", Some("IF.")),
                ("C", Some("C.")),
                ("b", None),
            ]
        );
    }

    #[test]
    fn a_paragraph_past_the_bound_is_cut_on_a_char_boundary_and_marked_cut() {
        let at_bound = "a".repeat(512);
        let wide = format!("{}\u{e9}", "a".repeat(511));

        assert_eq!(
            doc_of_one(&format!("/// {at_bound}\nfn f() {{}}")),
            whole(&at_bound)
        );
        assert_eq!(
            doc_of_one(&format!("/// {wide}\nfn f() {{}}")),
            Some(("a".repeat(511), true))
        );
        assert_eq!(
            doc_of_one(&format!("/// {at_bound}\n/// b\nfn f() {{}}")),
            Some((at_bound, true))
        );
    }
}
