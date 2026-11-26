//! Markdown definitions: the ATX headings (`# Title`) outside fenced code,
//! each named by its path through the headings above it
//! (`Install::From source`), and spanning its section: from its line to
//! the next heading of its level or a higher one, trailing blank lines
//! left out. A heading's text is trimmed, its closing `#`s dropped, and its
//! runs of whitespace made one space; a `::` in it splits it as it splits
//! a symbol.

use super::index::{DefinitionKind, FileIndex, IndexFault, ScopeId, ScopeTree};
use super::lexical::Lexical;

/// The index of `text`, a Markdown file: its heading sections.
pub(super) fn index(text: &str, scopes: ScopeTree) -> Result<FileIndex, IndexFault> {
    let mut sections = Sections {
        text,
        open: Vec::new(),
        lexical: Lexical::of(text, scopes),
    };
    let mut fence: Option<Fence> = None;
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        match &fence {
            Some(open) if open.closed_by(line) => fence = None,
            Some(_) => {}
            None => {
                if let Some(opened) = Fence::opened_by(line) {
                    fence = Some(opened);
                } else if let Some((level, title)) = heading(line) {
                    sections.open(level, title, start)?;
                }
            }
        }
        start += line.len();
    }
    sections.close_from(1, text.len())?;
    Ok(sections.lexical.finish())
}

/// The headings whose sections are still open, outermost first: each
/// one's level, scope and first byte.
struct Sections<'t> {
    text: &'t str,
    open: Vec<(usize, ScopeId, usize)>,
    lexical: Lexical<'t>,
}

impl Sections<'_> {
    /// Open the section of a heading of `level` at byte `start`.
    fn open(&mut self, level: usize, title: &str, start: usize) -> Result<(), IndexFault> {
        self.close_from(level, start)?;
        let parent = self.open.last().map(|(_, scope, _)| *scope);
        let scope = self.lexical.scope(parent, title)?;
        self.open.push((level, scope, start));
        Ok(())
    }

    /// Close every open section of `level` or a deeper one at byte `end`.
    fn close_from(&mut self, level: usize, end: usize) -> Result<(), IndexFault> {
        while let Some((_, scope, start)) = self.open.pop_if(|(open, _, _)| *open >= level) {
            let section = self.text.get(start..end).unwrap_or_default();
            let end = start + section.trim_end().len();
            let hash = self.lexical.hash(start..end)?;
            self.lexical
                .define(scope, DefinitionKind::Heading, start..end, hash)?;
        }
        Ok(())
    }
}

/// The level and text of an ATX heading line: at most three spaces, one
/// to six `#`, then a space, a tab or the line's end.
fn heading(line: &str) -> Option<(usize, &str)> {
    let rest = indented(line)?;
    let level = rest.bytes().take_while(|&b| b == b'#').count();
    let after = rest.get(level..)?;
    if !(1..=6).contains(&level)
        || !(after.is_empty() || after.starts_with([' ', '\t', '\n', '\r']))
    {
        return None;
    }
    let title = after.trim();
    let closed = title.trim_end_matches('#');
    let title = match closed.is_empty() || closed.ends_with([' ', '\t']) {
        true => closed.trim_end(),
        false => title,
    };
    Some((level, title))
}

/// `line` past an indent of at most three spaces; `None` for a deeper
/// one, which is code.
fn indented(line: &str) -> Option<&str> {
    let spaces = line.bytes().take_while(|&b| b == b' ').count();
    (spaces <= 3).then(|| line.get(spaces..)).flatten()
}

/// An open code fence: its character and its length.
struct Fence {
    mark: char,
    length: usize,
}

impl Fence {
    /// The fence `line` opens: three or more backticks or tildes, an info
    /// string after backticks holding no backtick.
    fn opened_by(line: &str) -> Option<Self> {
        let rest = indented(line)?;
        let mark = rest.chars().next().filter(|c| matches!(c, '`' | '~'))?;
        let length = rest.chars().take_while(|&c| c == mark).count();
        let info = rest.get(length..).unwrap_or_default();
        (length >= 3 && !(mark == '`' && info.contains('`'))).then_some(Self { mark, length })
    }

    fn closed_by(&self, line: &str) -> bool {
        indented(line).is_some_and(|rest| {
            let length = rest.chars().take_while(|&c| c == self.mark).count();
            length >= self.length
                && rest
                    .get(length..)
                    .is_some_and(|tail| tail.trim().is_empty())
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::code::index::{FileKind, IndexFault, MAX_NESTING};
    use crate::code::testing;

    const README: &str = "\
# Tool

Intro.

## Install

### From source ###

Build it.

```sh
# Not a heading
```

### From a package

## Use

   ## Indented heading
    ## Code, not a heading
#Not a heading either

## Store::open
";

    fn named<'t>(text: &'t str, symbol: &str) -> Vec<&'t str> {
        testing::named(FileKind::Markdown, text, symbol)
    }

    #[test]
    fn a_heading_is_named_by_its_path_through_the_headings_above() {
        assert_eq!(named(README, "Install::From source").len(), 1);
        assert_eq!(named(README, "Tool::Install::From source").len(), 1);
        assert_eq!(named(README, "From a package"), ["### From a package"]);
        assert_eq!(named(README, "Indented heading").len(), 1);
        assert_eq!(named(README, "Store::open"), ["## Store::open"]);
        assert!(named(README, "Use::From source").is_empty());
        assert!(
            named(README, "source").is_empty(),
            "a heading names a section, not a word"
        );
    }

    #[test]
    fn a_section_runs_to_the_next_heading_of_its_level_or_higher() {
        let source = named(README, "From source")[0];
        assert!(source.starts_with("### From source ###\n\nBuild it."));
        assert!(source.ends_with("```"), "{source:?}");
        assert!(named(README, "Install")[0].ends_with("### From a package"));
        assert!(named(README, "Tool")[0].ends_with("## Store::open"));
    }

    #[test]
    fn a_path_past_the_bound_of_segments_is_refused() {
        let heading = |segments: usize| format!("# {}\n## b\n", vec!["a"; segments].join("::"));

        assert!(
            crate::code::testing::index(FileKind::Markdown, heading(MAX_NESTING - 1).as_bytes())
                .is_ok()
        );
        assert_eq!(
            crate::code::testing::index(FileKind::Markdown, heading(MAX_NESTING).as_bytes()),
            Err(IndexFault::TooDeep)
        );
    }

    #[test]
    fn nothing_in_a_fence_is_a_heading() {
        assert!(named(README, "Not a heading").is_empty());
        assert!(named(README, "Code, not a heading").is_empty());
        assert!(named(README, "Not a heading either").is_empty());
        let unclosed = "~~~~\n# Hidden\n~~~\n# Still hidden\n";
        assert!(named(unclosed, "Hidden").is_empty());
        assert!(named(unclosed, "Still hidden").is_empty());
    }
}
