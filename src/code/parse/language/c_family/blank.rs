//! The blank of a C or C++ file: the names of its tree's attribute
//! macros, a blanked call's arguments and the comments inside a
//! `#define`, each written as spaces, so every offset holds. A call's
//! arguments are read up to [`MAX_BLANK_ARGUMENT_BYTES`], so the blank is
//! linear in its bytes.

use std::borrow::Cow;
use std::ops::Range;

use super::FileMacros;
use super::define::define_of;
use super::lex::{Item, lex};

/// The bytes a blanked function-like macro's arguments may span, from its
/// opening parenthesis; a call longer is left as written.
pub(in crate::code) const MAX_BLANK_ARGUMENT_BYTES: usize = 256;

/// What a blanked range of a file is written as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fill {
    /// Spaces, its line breaks kept.
    Spaces,
    /// Spaces, its line breaks too: a comment inside a `#define`.
    Joined,
    /// Spaces, and what follows it is private: a static macro.
    Private,
}

/// A C or C++ file as its parse reads it.
pub(in crate::code) struct Blanked<'t> {
    /// Its text, the names of its macros (and a blanked call's arguments)
    /// and the comments inside a `#define` blanked; borrowed when nothing
    /// is.
    pub(in crate::code) text: Cow<'t, str>,
    /// Where the code after each blanked static macro starts, in order:
    /// the definitions it marks private start there or hold the place
    /// before their name.
    pub(in crate::code) private: Vec<usize>,
}

/// `text`, a C or C++ file, as its parse reads it with `macros` blanked.
pub(in crate::code) fn blank<'t>(text: &'t str, macros: &FileMacros) -> Blanked<'t> {
    let mut blanks = Blanks {
        text,
        macros,
        found: Vec::new(),
        call: None,
    };
    lex(text, true, |item| blanks.take(item));
    blanks.abandon();
    if blanks.found.is_empty() {
        return Blanked {
            text: Cow::Borrowed(text),
            private: Vec::new(),
        };
    }
    let mut out = String::with_capacity(text.len());
    let (mut at, mut private_ends) = (0, Vec::new());
    for (range, fill) in blanks.found {
        out.push_str(text.get(at..range.start).unwrap_or_default());
        for c in text.get(range.start..range.end).unwrap_or_default().chars() {
            match (c, fill) {
                ('\n', Fill::Spaces | Fill::Private) => out.push('\n'),
                // Any other character, and a comment's line break.
                _ => out.extend(std::iter::repeat_n(' ', c.len_utf8())),
            }
        }
        if fill == Fill::Private {
            private_ends.push(range.end);
        }
        at = range.end;
    }
    out.push_str(text.get(at..).unwrap_or_default());
    let private = code_after(&out, &private_ends);
    Blanked {
        text: Cow::Owned(out),
        private,
    }
}

/// Where the first item of `text` but a comment or a directive after each
/// of `ends` starts, each place once, in one pass.
fn code_after(text: &str, ends: &[usize]) -> Vec<usize> {
    let mut found: Vec<usize> = Vec::new();
    if ends.is_empty() {
        return found;
    }
    let mut ends = ends.iter().peekable();
    lex(text, true, |item| {
        if matches!(item, Item::Comment { .. } | Item::Directive { .. }) {
            return;
        }
        let start = item.start();
        while ends.next_if(|end| **end <= start).is_some() {
            if found.last() != Some(&start) {
                found.push(start);
            }
        }
    });
    found
}

/// The ranges a blank finds, in order, as the lexer hands it the items of
/// its text.
struct Blanks<'t, 'm> {
    text: &'t str,
    macros: &'m FileMacros,
    found: Vec<(Range<usize>, Fill)>,
    call: Option<Call>,
}

/// A blankable macro's call whose arguments the blank is reading: where
/// its name starts and ends, where its `(` is once met, how deep its
/// parentheses are open, and the names it would blank in them were the
/// call never closed.
struct Call {
    start: usize,
    name_end: usize,
    open: Option<usize>,
    depth: usize,
    names: Vec<(Range<usize>, Fill)>,
}

impl Blanks<'_, '_> {
    /// Take `item`, the next of the text: into the call being read, or else
    /// as one that may start a blank.
    fn take(&mut self, item: Item) {
        if let Some(call) = self.call.as_mut() {
            match call.read(item, self.text, self.macros) {
                Step::Reading => return,
                Step::Closed(end) => {
                    self.found.push((call.start..end, Fill::Spaces));
                    self.call = None;
                    return;
                }
                Step::Abandoned => self.abandon(),
            }
        }
        match item {
            Item::Directive {
                start,
                end,
                inner_comments: true,
            } => {
                for comment in inner_comments(self.text.get(start..end).unwrap_or_default()) {
                    let range = start + comment.start..start + comment.end;
                    self.found.push((range, Fill::Joined));
                }
            }
            Item::Identifier { start, end } => {
                let name = self.text.get(start..end).unwrap_or_default();
                if let Some(fill) = macros_fill(self.macros, name) {
                    self.found.push((start..end, fill));
                } else if self.macros.calls.contains(name) {
                    self.call = Some(Call {
                        start,
                        name_end: end,
                        open: None,
                        depth: 0,
                        names: Vec::new(),
                    });
                }
            }
            Item::Directive { .. } | Item::Token { .. } | Item::Comment { .. } => {}
        }
    }

    /// Give up the call being read: the names in its arguments are blanked
    /// alone.
    fn abandon(&mut self) {
        if let Some(call) = self.call.take() {
            self.found.extend(call.names);
        }
    }
}

/// How `name`, written alone, is blanked by `macros`; `None` when it is
/// not.
fn macros_fill(macros: &FileMacros, name: &str) -> Option<Fill> {
    match (macros.names.contains(name), macros.statics.contains(name)) {
        (true, true) => Some(Fill::Private),
        (true, false) => Some(Fill::Spaces),
        (false, _) => None,
    }
}

/// What reading one more item did to a call.
enum Step {
    Reading,
    /// Its `)` ends here.
    Closed(usize),
    /// It is no call to blank: no `(` right after its name, a `;`, brace or
    /// directive before its `)`, or arguments past their bound.
    Abandoned,
}

impl Call {
    fn read(&mut self, item: Item, text: &str, macros: &FileMacros) -> Step {
        let (start, end) = match item {
            Item::Token { start, end } | Item::Identifier { start, end } => (start, end),
            Item::Comment { .. } if self.open.is_some() => return Step::Reading,
            Item::Comment { .. } | Item::Directive { .. } => return Step::Abandoned,
        };
        let Some(open) = self.open else {
            let gap = text.get(self.name_end..start).unwrap_or_default();
            if text.get(start..end) != Some("(") || !gap.bytes().all(|b| matches!(b, b' ' | b'\t'))
            {
                return Step::Abandoned;
            }
            self.open = Some(start);
            self.depth = 1;
            return Step::Reading;
        };
        if end - open > MAX_BLANK_ARGUMENT_BYTES {
            return Step::Abandoned;
        }
        let token = text.get(start..end).unwrap_or_default();
        match token {
            "(" => self.depth += 1,
            ")" if self.depth == 1 => return Step::Closed(end),
            ")" => self.depth -= 1,
            ";" | "{" | "}" => return Step::Abandoned,
            name => self
                .names
                .extend(macros_fill(macros, name).map(|fill| (start..end, fill))),
        }
        Step::Reading
    }
}

/// The comments of `directive` to blank, when it is a `#define`: each a
/// token of it follows. A trailing one is left, since blanked it would
/// leave the blanks tree-sitter-c reads a body's end at.
fn inner_comments(directive: &str) -> Vec<Range<usize>> {
    let (mut found, mut trailing) = (Vec::new(), Vec::new());
    if define_of(directive).is_none() {
        return found;
    }
    lex(directive, false, |item| match item {
        Item::Comment { start, end } => trailing.push(start..end),
        Item::Token { .. } | Item::Identifier { .. } => found.append(&mut trailing),
        Item::Directive { .. } => {}
    });
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::parse::language::c_family::testing::{blanked, file, tree};

    #[test]
    fn a_tree_defined_attribute_is_blanked_where_code_writes_it_alone() {
        let header = "#define API __attribute__((used))\n#define PRINTF(f, a) __attribute__((format(printf, f, a)))\n";
        let text = "#ifdef API\n#define OTHER API\n#endif\n/* API */ API int f(const char *s, ...) PRINTF(1, 2);\nconst char *t = \"API\", *r = R\"x(API)x\";\nint PRINTF;\n";
        let blanked = blanked(&[header], text);

        assert_eq!(
            blanked,
            "#ifdef API\n#define OTHER API\n#endif\n/* API */     int f(const char *s, ...)             ;\nconst char *t = \"API\", *r = R\"x(API)x\";\nint PRINTF;\n"
        );
        assert!(matches!(
            blank("int f();\n", &FileMacros::default()).text,
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn a_static_macro_of_any_length_marks_the_code_after_it_private() {
        let header = "#ifdef TESTING\n#define STATIC\n#define SI\n#else\n#define STATIC static\n#define SI static inline\n#endif\n#define local static\n#define API\n";
        let text = "STATIC int a;\nSI /* c */ int b(void);\nlocal API int c;\nint d; local\n#if X\n#endif\nint e;\n";
        let blanked = blank(text, &file(&tree(&[header]), text));
        let after = |code: &str| text.find(code).unwrap();

        assert_eq!(
            blanked.text,
            "       int a;\n   /* c */ int b(void);\n          int c;\nint d;      \n#if X\n#endif\nint e;\n"
        );
        assert_eq!(
            blanked.private,
            [
                after("int a"),
                after("int b"),
                after("int c"),
                after("int e")
            ]
        );
    }

    #[test]
    fn a_call_is_blanked_within_its_bound_of_argument_bytes() {
        let header = "#define ATTR(x) __attribute__((x))\n";
        let call = |bytes: usize| format!("int f() ATTR({});\n", "a".repeat(bytes - 2));

        let within = call(MAX_BLANK_ARGUMENT_BYTES);
        assert!(!blanked(&[header], &within).contains("ATTR"));
        let past = call(MAX_BLANK_ARGUMENT_BYTES + 1);
        assert_eq!(blanked(&[header], &past), past);
    }

    #[test]
    fn a_calls_arguments_end_at_their_parenthesis_past_literals_comments_and_lines() {
        let header = "#define ATTR(x) __attribute__((x))\n";
        let text = "int f() ATTR(\")\" /* ) */ ')',\n  (x));\n";

        assert_eq!(
            blanked(&[header], text),
            format!("int f() {}\n{};\n", " ".repeat(21), " ".repeat(6))
        );
    }

    #[test]
    fn a_call_is_left_as_written_when_no_parenthesis_follows_its_name() {
        let header = "#define ATTR(x) __attribute__((x))\n";
        for text in [
            "int ATTR;\n",
            "int f() ATTR\n(x);\n",
            "int f() ATTR /* c */ (x);\n",
        ] {
            assert_eq!(blanked(&[header], text), text);
        }
    }

    #[test]
    fn a_call_a_statement_ends_before_its_parenthesis_blanks_only_the_names_in_it() {
        let header = "#define ATTR(x) __attribute__((x))\n#define API\n";
        for stop in [";", "{", "}", "\n#if X\n"] {
            let text = format!("int f() ATTR(API x{stop} int g(void));\n");
            let blanked = blanked(&[header], &text);

            assert!(
                blanked.contains("ATTR(") && blanked.contains("g(void)"),
                "{text}"
            );
            assert!(!blanked.contains("API"), "{text}");
        }
    }

    #[test]
    fn the_comments_inside_a_define_are_blanked_line_breaks_and_all() {
        let text = "#define SWAP(a, b) do { /* swap the\n two */ \\\n  t = a; \\\n } while (0) // last\n#define ONE 1 /* one */\n/* kept */\n";
        let inside =
            "#define M(a) ((a) /* c */ + 1)\n#define LIMIT(a) ((a) + \\\n    /* slack */ 1)\n";

        assert_eq!(
            blank(text, &FileMacros::default()).text,
            format!(
                "#define SWAP(a, b) do {{ {} \\\n  t = a; \\\n }} while (0) // last\n#define ONE 1 /* one */\n/* kept */\n",
                " ".repeat(19)
            )
        );
        assert_eq!(
            blank(inside, &FileMacros::default()).text,
            "#define M(a) ((a)         + 1)\n#define LIMIT(a) ((a) + \\\n                1)\n"
        );
    }
}
