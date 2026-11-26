//! The canonical form of `project.toml` and the node files: the one way
//! the crate writes them.
//!
//! Keys come in a fixed order per file type. A set (tags, edges, anchors)
//! is written sorted, one element per line with a trailing comma, so two
//! branches that each add an element at a different place merge without
//! a conflict; an empty set or list is left out. A string holding a line
//! break is a `"""` string, a `[quality]` pattern a `'...'` literal when it
//! can be one, every other one a basic `"..."` string.
//!
//! The form is defined here, not by the TOML library's printer, so a
//! dependency update never changes the bytes of a file nobody edited. Every
//! write parses its text back and refuses it unless it reads as the value
//! it came from (`commit.rs`).

use std::fmt::{self, Write};

use crate::store::model::{AcceptedVia, Anchor, Component, Decision, Project, Status};
use crate::store::model::{Pattern, Quality, WordRange};

/// A file type with a canonical form.
pub(crate) trait Canonical {
    /// Fails only when `out` does: never into a `String`, at the first byte
    /// that differs into a comparison.
    fn emit<W: Write>(&self, out: &mut Emitter<W>) -> fmt::Result;
}

pub(in crate::store) fn canonical<T: Canonical>(value: &T) -> String {
    let mut out = Emitter(String::with_capacity(1024));
    // Writing to a `String` cannot fail.
    let _ = value.emit(&mut out);
    out.0
}

/// Whether `bytes` are the canonical text of `value`. Compares as it
/// writes, so it allocates nothing and stops at the first difference.
pub(in crate::store) fn is_canonical<T: Canonical>(value: &T, bytes: &[u8]) -> bool {
    let mut out = Emitter(Compare { rest: bytes });
    value.emit(&mut out).is_ok() && out.0.rest.is_empty()
}

/// A sink that matches what is written against the bytes it holds and
/// fails at the first byte that differs.
struct Compare<'a> {
    rest: &'a [u8],
}

impl Write for Compare<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        match self.rest.strip_prefix(text.as_bytes()) {
            Some(rest) => {
                self.rest = rest;
                Ok(())
            }
            None => Err(fmt::Error),
        }
    }
}

/// Writes keys and values in the canonical layout.
pub(crate) struct Emitter<W>(W);

impl<W: Write> Emitter<W> {
    fn string(&mut self, key: &str, value: &str) -> fmt::Result {
        write!(self.0, "{key} = ")?;
        self.text(value)?;
        self.0.write_char('\n')
    }

    /// `value` as a basic string, written as it displays: for a value
    /// whose display holds no character a string escapes, such as a
    /// timestamp or a format version.
    fn display(&mut self, key: &str, value: &impl fmt::Display) -> fmt::Result {
        writeln!(self.0, "{key} = \"{value}\"")
    }

    fn optional(&mut self, key: &str, value: Option<&str>) -> fmt::Result {
        value.map_or(Ok(()), |value| self.string(key, value))
    }

    /// One element per line; nothing for an empty sequence.
    fn array<T>(
        &mut self,
        key: &str,
        items: impl IntoIterator<Item = T>,
        mut item: impl FnMut(&mut Self, T) -> fmt::Result,
    ) -> fmt::Result {
        let mut items = items.into_iter().peekable();
        if items.peek().is_none() {
            return Ok(());
        }
        writeln!(self.0, "{key} = [")?;
        for each in items {
            self.0.write_str("    ")?;
            item(self, each)?;
            self.0.write_str(",\n")?;
        }
        self.0.write_str("]\n")
    }

    fn strings<'a>(
        &mut self,
        key: &str,
        items: impl IntoIterator<Item = &'a String>,
    ) -> fmt::Result {
        self.array(key, items, |out, item| out.text(item))
    }

    fn anchor(&mut self, anchor: &Anchor) -> fmt::Result {
        self.0.write_str("{ file = ")?;
        self.text(anchor.file())?;
        if let Some(symbol) = anchor.symbol() {
            self.0.write_str(", symbol = ")?;
            self.text(symbol)?;
        }
        self.0.write_str(" }")
    }

    fn table(&mut self, name: &str) -> fmt::Result {
        writeln!(self.0, "\n[{name}]")
    }

    fn array_table(&mut self, name: &str) -> fmt::Result {
        writeln!(self.0, "\n[[{name}]]")
    }

    fn count(&mut self, key: &str, value: Option<usize>) -> fmt::Result {
        value.map_or(Ok(()), |value| writeln!(self.0, "{key} = {value}"))
    }

    /// `true` as a key; `false`, the default, is left out.
    fn flag(&mut self, key: &str, value: bool) -> fmt::Result {
        if value {
            writeln!(self.0, "{key} = true")?;
        }
        Ok(())
    }

    /// A range as an inline table of the bounds it has; nothing for none.
    fn range(&mut self, key: &str, range: WordRange) -> fmt::Result {
        let bounds = [("min", range.min), ("max", range.max)];
        let mut bounds = bounds
            .into_iter()
            .filter_map(|(name, bound)| bound.map(|bound| (name, bound)));
        let Some((name, bound)) = bounds.next() else {
            return Ok(());
        };
        write!(self.0, "{key} = {{ {name} = {bound}")?;
        for (name, bound) in bounds {
            write!(self.0, ", {name} = {bound}")?;
        }
        self.0.write_str(" }\n")
    }

    /// A regular expression as a `'...'` literal string, which holds its
    /// backslashes as written, unless it holds what a literal cannot: a
    /// `'` or a control character other than tab.
    fn pattern(&mut self, key: &str, pattern: &str) -> fmt::Result {
        let literal = !pattern
            .chars()
            .any(|c| c == '\'' || (c.is_control() && c != '\t'));
        if literal {
            writeln!(self.0, "{key} = '{pattern}'")
        } else {
            self.string(key, pattern)
        }
    }

    /// `text` as a TOML string: `"""` when it holds a line break, so the
    /// file shows its lines, else `"..."`.
    fn text(&mut self, text: &str) -> fmt::Result {
        let multiline = text.contains('\n');
        // A line break right after the opening `"""` is not part of the string.
        let quote = if multiline { "\"\"\"" } else { "\"" };
        self.0.write_str(quote)?;
        if multiline {
            self.0.write_char('\n')?;
        }
        let mut rest = text;
        while let Some(at) = rest.find(|c| needs_escape(c, multiline)) {
            let Some((plain, tail)) = rest.split_at_checked(at) else {
                break;
            };
            self.0.write_str(plain)?;
            let mut chars = tail.chars();
            match chars.next() {
                Some('"') => self.0.write_str("\\\"")?,
                Some('\\') => self.0.write_str("\\\\")?,
                Some('\n') => self.0.write_str("\\n")?,
                Some('\r') => self.0.write_str("\\r")?,
                Some(c) => write!(self.0, "\\u{:04X}", u32::from(c))?,
                None => {}
            }
            rest = chars.as_str();
        }
        self.0.write_str(rest)?;
        self.0.write_str(quote)
    }
}

/// Whether `c` is written as an escape: a quote, a backslash, every control
/// character but tab and line break, and a line break outside a `"""`
/// string.
fn needs_escape(c: char, multiline: bool) -> bool {
    match c {
        '"' | '\\' => true,
        '\t' => false,
        '\n' => !multiline,
        c => c.is_control(),
    }
}

impl Canonical for Project {
    fn emit<W: Write>(&self, out: &mut Emitter<W>) -> fmt::Result {
        out.display("format_version", &self.format_version)?;
        out.string("name", &self.name)?;
        out.string("description", &self.description)?;
        out.string("placement", self.placement.as_str())?;
        self.quality.emit(out)
    }
}

/// `[quality]`, left out when it holds nothing: each limit it has, then
/// each pattern as a `[[quality.patterns]]` table, in its order.
impl Canonical for Quality {
    fn emit<W: Write>(&self, out: &mut Emitter<W>) -> fmt::Result {
        if self.is_default() {
            return Ok(());
        }
        out.table("quality")?;
        out.range("reason_words", self.reason_words)?;
        out.count("alternatives_max", self.alternatives_max)?;
        out.count("alternative_words_max", self.alternative_words_max)?;
        out.count("anchors_max", self.anchors_max)?;
        out.flag("require_origin", self.require_origin)?;
        for pattern in &self.patterns {
            out.array_table("quality.patterns")?;
            pattern.emit(out)?;
        }
        Ok(())
    }
}

impl Canonical for Pattern {
    fn emit<W: Write>(&self, out: &mut Emitter<W>) -> fmt::Result {
        out.pattern("pattern", &self.pattern)?;
        out.array("fields", &self.fields, |out, field| {
            out.text(field.as_str())
        })?;
        out.string("level", self.level.as_str())?;
        out.string("message", &self.message)
    }
}

impl Canonical for Component {
    fn emit<W: Write>(&self, out: &mut Emitter<W>) -> fmt::Result {
        out.string("description", &self.description)?;
        out.strings("connects_to", &self.connects_to)
    }
}

impl Canonical for Decision {
    fn emit<W: Write>(&self, out: &mut Emitter<W>) -> fmt::Result {
        out.string("component", &self.component)?;
        out.string("choice", &self.choice)?;
        out.string("reason", &self.reason)?;
        out.strings("alternatives", &self.alternatives)?;
        out.strings("tags", &self.tags)?;
        out.string("status", self.status.name())?;
        let via = match &self.status {
            Status::Accepted { via } => via.map(AcceptedVia::as_str),
            Status::Proposed { .. } => None,
        };
        out.optional("accepted_via", via)?;
        out.optional("origin", self.origin.as_deref())?;
        out.display("created", &self.created)?;
        out.array("anchors", &self.anchors, Emitter::anchor)?;
        out.strings("depends_on", &self.depends_on)?;
        if let Status::Proposed {
            accepted_text: Some(text),
        } = &self.status
        {
            out.table("accepted_text")?;
            out.string("choice", &text.choice)?;
            out.string("reason", &text.reason)?;
            out.strings("alternatives", &text.alternatives)?;
            out.optional("accepted_via", text.via.map(AcceptedVia::as_str))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::store::model::{AcceptedText, Placement};
    use crate::store::testing::{sample_decision, ts};
    use crate::store::{FORMAT_VERSION, Timestamp};

    fn anchor(file: &str, symbol: Option<&str>) -> Anchor {
        Anchor::new(file.into(), symbol.map(str::to_owned)).unwrap()
    }

    fn full_decision() -> Decision {
        Decision {
            component: "store".into(),
            choice: "Write node files in one canonical form".into(),
            reason: "Two branches merge line by line.\n\tA \"set\" is sorted \\ once.\n".into(),
            alternatives: vec!["Pretty-print with the library".into(), "Two\nlines".into()],
            tags: ["format", "git"].map(String::from).into(),
            status: Status::Proposed {
                accepted_text: Some(AcceptedText {
                    choice: "Old choice".into(),
                    reason: "The reason the owner accepted".into(),
                    alternatives: vec![],
                    via: Some(AcceptedVia::Cli),
                }),
            },
            origin: Some("format 0.5".into()),
            created: ts(),
            anchors: [anchor("src/b.rs", None), anchor("src/a.rs", Some("f"))].into(),
            depends_on: ["b-rule", "a-rule"].map(String::from).into(),
        }
    }

    #[test]
    fn a_decision_is_written_in_its_fixed_layout() {
        assert_eq!(
            canonical(&full_decision()),
            r#"component = "store"
choice = "Write node files in one canonical form"
reason = """
Two branches merge line by line.
	A \"set\" is sorted \\ once.
"""
alternatives = [
    "Pretty-print with the library",
    """
Two
lines""",
]
tags = [
    "format",
    "git",
]
status = "proposed"
origin = "format 0.5"
created = "2025-06-01T10:00:00Z"
anchors = [
    { file = "src/a.rs", symbol = "f" },
    { file = "src/b.rs" },
]
depends_on = [
    "a-rule",
    "b-rule",
]

[accepted_text]
choice = "Old choice"
reason = "The reason the owner accepted"
accepted_via = "cli"
"#
        );
    }

    #[test]
    fn empty_sets_and_absent_values_are_left_out() {
        let mut decision = sample_decision("project");
        decision.status = Status::Accepted {
            via: Some(AcceptedVia::Elicitation),
        };
        assert_eq!(
            canonical(&decision),
            "component = \"project\"\nchoice = \"Choice of the sample\"\n\
             reason = \"Reason of the sample decision\"\nstatus = \"accepted\"\n\
             accepted_via = \"elicitation\"\ncreated = \"2025-06-01T10:00:00Z\"\n"
        );
        let component = Component {
            description: String::new(),
            connects_to: BTreeSet::new(),
        };
        assert_eq!(canonical(&component), "description = \"\"\n");
    }

    #[test]
    fn every_control_character_is_escaped_and_reads_back() {
        let text: String = (0..=0x7F_u8)
            .map(char::from)
            .chain(['\u{80}', '\u{9F}', '\u{2028}', '\u{FEFF}', '\u{17c}'])
            .collect();
        for value in [text.replace('\n', ""), text] {
            let component = Component {
                description: value,
                connects_to: BTreeSet::new(),
            };
            let written = canonical(&component);
            assert!(!written.contains('\u{0}'), "{written:?}");
            assert_eq!(toml::from_str::<Component>(&written).unwrap(), component);
        }
    }

    #[test]
    fn each_file_type_reads_back_as_the_value_it_was_written_from() {
        let decision = full_decision();
        assert_eq!(
            toml::from_str::<Decision>(&canonical(&decision)).unwrap(),
            decision
        );
        let project = Project {
            format_version: FORMAT_VERSION,
            name: "demo".into(),
            description: "A \"demo\"\nproject".into(),
            placement: Placement::Shared,
            quality: Quality::default(),
        };
        assert_eq!(
            toml::from_str::<Project>(&canonical(&project)).unwrap(),
            project
        );
        let component = Component {
            description: "Auth".into(),
            connects_to: ["store", "billing"].map(String::from).into(),
        };
        assert_eq!(
            toml::from_str::<Component>(&canonical(&component)).unwrap(),
            component
        );
    }

    /// `[quality]` follows the project's keys: its limits, then each
    /// pattern as a table, a regular expression as a literal string unless
    /// it holds a `'`; the default table is left out.
    #[test]
    fn a_quality_table_is_written_in_its_fixed_layout_and_reads_back() {
        use crate::store::model::{DecisionField, Level, Pattern, WordRange};

        let pattern = |text: &str, level| Pattern {
            pattern: text.into(),
            fields: [DecisionField::Reason, DecisionField::Choice].into(),
            level,
            message: "Say \"use\"".into(),
        };
        let mut project = Project {
            format_version: FORMAT_VERSION,
            name: "demo".into(),
            description: String::new(),
            placement: Placement::Tracked,
            quality: Quality::default(),
        };
        let header = "format_version = \"0.5.0\"\nname = \"demo\"\ndescription = \"\"\n\
                      placement = \"tracked\"\n";
        assert_eq!(canonical(&project), header);
        project.quality = Quality {
            reason_words: WordRange {
                min: None,
                max: Some(180),
            },
            anchors_max: Some(8),
            require_origin: true,
            patterns: vec![
                pattern(r"\butilize\b", Level::Refuse),
                pattern(r"it's\s", Level::Warn),
            ],
            ..Quality::default()
        };
        let written = canonical(&project);
        assert_eq!(
            written,
            format!(
                "{header}\n[quality]\nreason_words = {{ max = 180 }}\nanchors_max = 8\n\
                 require_origin = true\n\n[[quality.patterns]]\npattern = '\\butilize\\b'\n\
                 fields = [\n    \"choice\",\n    \"reason\",\n]\nlevel = \"refuse\"\n\
                 message = \"Say \\\"use\\\"\"\n\n[[quality.patterns]]\n\
                 pattern = \"it's\\\\s\"\nfields = [\n    \"choice\",\n    \"reason\",\n]\n\
                 level = \"warn\"\nmessage = \"Say \\\"use\\\"\"\n"
            )
        );
        assert_eq!(toml::from_str::<Project>(&written).unwrap(), project);
        assert!(is_canonical(&project, written.as_bytes()));
    }

    #[test]
    fn is_canonical_holds_for_exactly_the_canonical_bytes() {
        let decision = full_decision();
        let bytes = canonical(&decision).into_bytes();
        assert!(is_canonical(&decision, &bytes));
        assert!(!is_canonical(&decision, &bytes[..bytes.len() - 1]));
        let mut longer = bytes.clone();
        longer.push(b'\n');
        assert!(!is_canonical(&decision, &longer));
        let mut changed = bytes;
        changed[0] = b'C';
        assert!(!is_canonical(&decision, &changed));
    }

    #[test]
    fn a_timestamp_keeps_the_digits_it_was_read_with() {
        let mut decision = sample_decision("project");
        decision.created = "2026-10-03T14:48:16.270508355Z"
            .parse::<Timestamp>()
            .unwrap();
        let written = canonical(&decision);
        assert!(written.contains("created = \"2026-10-03T14:48:16.270508355Z\"\n"));
    }
}
