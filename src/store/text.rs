//! The rules for every text a graph file holds and a write takes: what a
//! reader sees is what an agent is held to.
//!
//! No text may carry a character that renders as nothing or reorders the
//! text around it, nor the markup of a tool call, which only lands in an
//! argument when the call was malformed. Each field adds its shape:
//!
//! | field | blank | lines | bytes |
//! |---|---|---|---|
//! | a decision's `choice` | no | one | at most [`MAX_CHOICE_BYTES`] |
//! | a decision's `reason` | no | many | [`MIN_REASON_BYTES`] of reasoning to [`MAX_TEXT_FIELD_BYTES`] |
//! | an alternative | no | one | at most [`MAX_ALTERNATIVE_BYTES`] |
//! | a tag | no | one | at most [`MAX_TAG_BYTES`] |
//! | an `origin`, a `Session:` | no | one | at most [`MAX_TRAILER_BYTES`] |
//! | a component's `description` | yes | many | at most [`MAX_TEXT_FIELD_BYTES`] |
//! | a `[quality]` pattern's message | no | one | at most [`MAX_PATTERN_MESSAGE_BYTES`] |
//!
//! Each rule is a property of every file: validation reports a decision or
//! a component that breaks one, whoever wrote it, and `check` fails on it,
//! while the reads still answer. A write is refused for one in each field
//! it writes (`store::quality`). An anchor's `file` and `symbol` meet the
//! character rule as part of their type (`schema::Anchor`), whose faults a
//! load refuses, and a `[quality]` message as part of `Quality`.

use std::fmt;

use super::limits::{
    MAX_ALTERNATIVE_BYTES, MAX_CHOICE_BYTES, MAX_PATTERN_MESSAGE_BYTES, MAX_TAG_BYTES,
    MAX_TEXT_FIELD_BYTES, MAX_TRAILER_BYTES, MIN_REASON_BYTES,
};

/// The field a [`TextFault`] was found in: a decision's or a component's,
/// a trailer of a history commit, or a `[quality]` pattern's message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextField {
    Choice,
    Reason,
    /// One element of a decision's `alternatives`.
    Alternative,
    /// One element of a decision's `tags`.
    Tag,
    Origin,
    Session,
    /// A component's.
    Description,
    Message,
}

impl fmt::Display for TextField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Choice => "choice",
            Self::Reason => "reason",
            Self::Alternative => "alternative",
            Self::Tag => "tag",
            Self::Origin => "origin",
            Self::Session => "session",
            Self::Description => "description",
            Self::Message => "message",
        })
    }
}

/// What a field's text may be besides the character rule.
struct Shape {
    max: usize,
    /// Bytes of reasoning, not counting the whitespace around it.
    min: usize,
    blank: Blank,
    lines: Lines,
}

#[derive(PartialEq, Eq)]
enum Blank {
    Allowed,
    Refused,
}

/// Whether a field spans lines: one that may also keeps tabs.
#[derive(PartialEq, Eq)]
enum Lines {
    One,
    Many,
}

impl TextField {
    const fn shape(self) -> Shape {
        let (max, min, blank, lines) = match self {
            Self::Choice => (MAX_CHOICE_BYTES, 0, Blank::Refused, Lines::One),
            Self::Reason => (
                MAX_TEXT_FIELD_BYTES,
                MIN_REASON_BYTES,
                Blank::Refused,
                Lines::Many,
            ),
            Self::Alternative => (MAX_ALTERNATIVE_BYTES, 0, Blank::Refused, Lines::One),
            Self::Tag => (MAX_TAG_BYTES, 0, Blank::Refused, Lines::One),
            Self::Origin | Self::Session => (MAX_TRAILER_BYTES, 0, Blank::Refused, Lines::One),
            Self::Description => (MAX_TEXT_FIELD_BYTES, 0, Blank::Allowed, Lines::Many),
            Self::Message => (MAX_PATTERN_MESSAGE_BYTES, 0, Blank::Refused, Lines::One),
        };
        Shape {
            max,
            min,
            blank,
            lines,
        }
    }
}

/// Why the store refused a field. `at` is a byte offset into the field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextFault {
    Blank,
    TooShort { min: usize, len: usize },
    TooLong { max: usize, len: usize },
    LineBreak { at: usize },
    Character { found: char, at: usize },
    ToolMarkup { found: String, at: usize },
}

impl fmt::Display for TextFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Blank => f.write_str("is blank"),
            Self::TooShort { min, len } => write!(
                f,
                "must hold at least {min} bytes of reasoning, not counting the \
                 whitespace around it ({len} given)"
            ),
            Self::TooLong { max, len } => {
                write!(f, "must be at most {max} bytes ({len} given)")
            }
            Self::LineBreak { at } => {
                write!(f, "must be a single line, but breaks at byte {at}")
            }
            Self::Character { found, at } => write!(
                f,
                "contains {} U+{:04X} at byte {at}",
                character_class(*found).unwrap_or("character"),
                u32::from(*found)
            ),
            Self::ToolMarkup { found, at } => write!(
                f,
                "contains tool-call markup `{found}` at byte {at}, left by a \
                 malformed tool call"
            ),
        }
    }
}

/// The first rule of `field` that `text` breaks, if any.
pub(crate) fn fault(field: TextField, text: &str) -> Option<TextFault> {
    let Shape {
        max,
        min,
        blank,
        lines,
    } = field.shape();
    let len = text.len();
    let reasoning = text.trim().len();
    if reasoning == 0 && blank == Blank::Refused {
        return Some(TextFault::Blank);
    }
    if len > max {
        return Some(TextFault::TooLong { max, len });
    }
    if reasoning < min {
        return Some(TextFault::TooShort {
            min,
            len: reasoning,
        });
    }
    find_character(&lines, text).or_else(|| find_tool_markup(text))
}

/// The first character of `text` that no text may hold, and its byte
/// offset: the character rule alone, a line break and a tab included.
pub(crate) fn hidden_character(text: &str) -> Option<(char, usize)> {
    text.char_indices()
        .find(|&(_, found)| character_class(found).is_some())
        .map(|(at, found)| (found, at))
}

fn find_character(lines: &Lines, text: &str) -> Option<TextFault> {
    text.char_indices()
        .find_map(|(at, found)| match (lines, found) {
            // A line break in a trailer would end it and start another.
            (Lines::One, '\n') => Some(TextFault::LineBreak { at }),
            (Lines::Many, '\n' | '\t') => None,
            (Lines::One | Lines::Many, _) => {
                character_class(found).map(|_| TextFault::Character { found, at })
            }
        })
}

/// The class of a character no text may hold, or `None` for one it may.
/// Line and tab are classed as controls here; a text of many lines allows
/// them.
pub(crate) fn character_class(c: char) -> Option<&'static str> {
    match c {
        '\u{0}'..='\u{1F}' | '\u{7F}'..='\u{9F}' => Some("control character"),
        '\u{61C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' => {
            Some("bidirectional control")
        }
        '\u{200B}'..='\u{200D}' | '\u{2060}' | '\u{FEFF}' => Some("zero-width character"),
        '\u{2028}' | '\u{2029}' => Some("line or paragraph separator"),
        _ => None,
    }
}

/// The elements of a tool call, opened or closed: `</?(name)\b`.
const CALL_ELEMENTS: [&str; 3] = ["parameter", "invoke", "function_calls"];

/// The argument names whose closing tag ends a leaked argument: `</(name)>`.
const ARGUMENT_ELEMENTS: [&str; 6] = [
    "choice",
    "reason",
    "tags",
    "code_refs",
    "anchors",
    "alternatives",
];

/// A tool call's elements carry this namespace prefix in the markup models
/// write; a leak may keep it or lose it.
const CALL_NAMESPACE: &str = "antml:";

fn find_tool_markup(text: &str) -> Option<TextFault> {
    text.match_indices('<').find_map(|(at, _)| {
        let markup = text.get(at..)?;
        let found = markup.get(..tag_len(markup)?)?;
        Some(TextFault::ToolMarkup {
            found: found.to_owned(),
            at,
        })
    })
}

/// The length of the tool-call tag `markup` starts with, or `None` when it
/// starts with none.
fn tag_len(markup: &str) -> Option<usize> {
    let tag = markup.strip_prefix('<')?;
    let (closing, name) = match tag.strip_prefix('/') {
        Some(name) => (true, name),
        None => (false, tag),
    };
    let unprefixed = name.strip_prefix(CALL_NAMESPACE).unwrap_or(name);
    let call = CALL_ELEMENTS.iter().find_map(|element| {
        unprefixed
            .strip_prefix(element)
            .filter(|rest| ends_word(rest))
    });
    let rest = match call {
        Some(rest) => rest,
        None if closing => ARGUMENT_ELEMENTS
            .iter()
            .find_map(|element| name.strip_prefix(element)?.strip_prefix('>'))?,
        None => return None,
    };
    Some(markup.len() - rest.len())
}

/// Whether a name that `rest` follows ends there, as `\b` after a word.
fn ends_word(rest: &str) -> bool {
    !rest
        .chars()
        .next()
        .is_some_and(|c| c.is_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    const REASON: &str = "Tokens are verified without a session store";

    #[test]
    fn a_plain_choice_and_a_multi_line_reason_pass() {
        assert_eq!(fault(TextField::Choice, "Sign tokens with Ed25519"), None);
        let reason = "Verification needs no session store.\n\n\t- rotation is a key swap";
        assert_eq!(fault(TextField::Reason, reason), None);
    }

    #[test]
    fn text_in_any_script_passes() {
        let polish = "U\u{17c}ywaj \u{17c}eton\u{f3}w";
        let japanese = "\u{7f72}\u{540d}\u{3059}\u{308b}";
        let arabic = "\u{631}\u{645}\u{632} \u{645}\u{648}\u{642}\u{639}";
        assert_eq!(fault(TextField::Choice, polish), None);
        assert_eq!(fault(TextField::Choice, japanese), None);
        assert_eq!(
            fault(TextField::Reason, &format!("{arabic} {REASON}")),
            None
        );
    }

    #[test]
    fn length_bounds_hold_at_the_limit() {
        let choice = "c".repeat(MAX_CHOICE_BYTES);
        assert_eq!(fault(TextField::Choice, &choice), None);
        assert_eq!(
            fault(TextField::Choice, &format!("{choice}c")),
            Some(TextFault::TooLong {
                max: MAX_CHOICE_BYTES,
                len: MAX_CHOICE_BYTES + 1
            })
        );

        let reason = "r".repeat(MIN_REASON_BYTES);
        assert_eq!(fault(TextField::Reason, &reason), None);
        assert_eq!(
            fault(TextField::Reason, &format!("  {}  ", &reason[1..])),
            Some(TextFault::TooShort {
                min: MIN_REASON_BYTES,
                len: MIN_REASON_BYTES - 1
            })
        );
        let long = "r".repeat(MAX_TEXT_FIELD_BYTES + 1);
        assert!(matches!(
            fault(TextField::Reason, &long),
            Some(TextFault::TooLong { .. })
        ));
    }

    #[test]
    fn blank_text_is_refused_in_either_field() {
        for field in [TextField::Choice, TextField::Reason] {
            assert_eq!(fault(field, " \n\t "), Some(TextFault::Blank));
        }
    }

    /// Every field but a component's description refuses a blank text;
    /// only a reason and a description span lines.
    #[test]
    fn each_field_has_its_shape() {
        let fields = [
            (TextField::Choice, Blank::Refused, Lines::One),
            (TextField::Reason, Blank::Refused, Lines::Many),
            (TextField::Alternative, Blank::Refused, Lines::One),
            (TextField::Tag, Blank::Refused, Lines::One),
            (TextField::Origin, Blank::Refused, Lines::One),
            (TextField::Session, Blank::Refused, Lines::One),
            (TextField::Description, Blank::Allowed, Lines::Many),
            (TextField::Message, Blank::Refused, Lines::One),
        ];
        for (field, blank, lines) in fields {
            let blank_fault = fault(field, " ");
            assert_eq!(blank_fault.is_none(), blank == Blank::Allowed, "{field}");
            let two_lines = fault(field, "Two lines of text,\nthe second as plain");
            assert_eq!(two_lines.is_none(), lines == Lines::Many, "{field}");
            let max = field.shape().max;
            assert_eq!(
                fault(field, &"x".repeat(max + 1)),
                Some(TextFault::TooLong { max, len: max + 1 }),
                "{field}"
            );
            assert_eq!(
                fault(field, &format!("{REASON} \u{200B}")),
                Some(TextFault::Character {
                    found: '\u{200B}',
                    at: REASON.len() + 1
                }),
                "{field}"
            );
            assert!(
                matches!(
                    fault(field, &format!("{REASON} </parameter>")),
                    Some(TextFault::ToolMarkup { .. })
                ),
                "{field}"
            );
        }
        assert_eq!(fault(TextField::Description, ""), None);
    }

    #[test]
    fn a_choice_is_one_line() {
        assert_eq!(
            fault(TextField::Choice, "Sign tokens\nwith Ed25519"),
            Some(TextFault::LineBreak { at: 11 })
        );
        assert_eq!(
            fault(TextField::Choice, "Sign\ttokens"),
            Some(TextFault::Character { found: '\t', at: 4 })
        );
    }

    #[test]
    fn each_invisible_or_reordering_character_is_refused_in_a_reason() {
        let refused = [
            '\0', '\r', '\u{1B}', '\u{7F}', '\u{80}', '\u{85}', '\u{9F}', '\u{61C}', '\u{200E}',
            '\u{200F}', '\u{202A}', '\u{202E}', '\u{2066}', '\u{2069}', '\u{200B}', '\u{200C}',
            '\u{200D}', '\u{2060}', '\u{FEFF}', '\u{2028}', '\u{2029}',
        ];
        for found in refused {
            let text = format!("{REASON}{found}.");
            assert_eq!(
                fault(TextField::Reason, &text),
                Some(TextFault::Character {
                    found,
                    at: REASON.len()
                }),
                "U+{:04X}",
                u32::from(found)
            );
        }
    }

    #[test]
    fn tool_call_markup_is_refused_with_the_tag_it_found() {
        let ns = CALL_NAMESPACE;
        let cases: [(String, String); 13] = [
            ("</parameter>".into(), "</parameter".into()),
            ("<parameter name=\"tags\">".into(), "<parameter".into()),
            ("<invoke name=\"get_context\">".into(), "<invoke".into()),
            ("</function_calls>".into(), "</function_calls".into()),
            ("</choice>".into(), "</choice>".into()),
            ("</reason>".into(), "</reason>".into()),
            ("</tags>".into(), "</tags>".into()),
            ("</code_refs>".into(), "</code_refs>".into()),
            ("</anchors>".into(), "</anchors>".into()),
            ("</alternatives>".into(), "</alternatives>".into()),
            (format!("</{ns}parameter>"), format!("</{ns}parameter")),
            (
                format!("<{ns}invoke name=\"get_context\">"),
                format!("<{ns}invoke"),
            ),
            (
                format!("<{ns}function_calls>"),
                format!("<{ns}function_calls"),
            ),
        ];
        for (markup, found) in cases {
            let text = format!("{REASON} {markup} more");
            assert_eq!(
                fault(TextField::Reason, &text),
                Some(TextFault::ToolMarkup {
                    found,
                    at: REASON.len() + 1
                }),
                "{markup}"
            );
        }
    }

    #[test]
    fn markup_like_text_that_is_no_tool_call_passes() {
        for text in [
            "Use Vec<Parameter> for the list",
            "Generic <parameters> stay as written",
            "An <invoke_hook> is no call element",
            "A <reason> opening tag is prose",
            "Compare a < b and b <invokes> c",
            "Ends with <",
            "Ends with </",
        ] {
            assert_eq!(fault(TextField::Reason, text), None, "{text}");
        }
    }
}
