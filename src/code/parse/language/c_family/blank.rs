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
