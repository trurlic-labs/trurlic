//! The rules for a decision's choice and reason, which the store applies to
//! every write of them, whichever surface it comes from.
//!
//! A choice is one line of at most [`MAX_CHOICE_BYTES`], and no other
//! decision of its component has it; a reason holds at least
//! [`MIN_REASON_BYTES`] of reasoning and may span lines. Neither may carry a
//! character that renders as nothing or reorders the text around it, so what
//! a reader sees is what an agent is held to, nor the markup of a tool call,
//! which only lands in an argument when the call was malformed.

use std::fmt;

use crate::{Error, Result};

use super::limits::{MAX_CHOICE_BYTES, MAX_TEXT_FIELD_BYTES, MIN_REASON_BYTES};
use super::state::ProjectState;

/// The decision field a [`TextFault`] was found in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextField {
    Choice,
    Reason,
}

impl fmt::Display for TextField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Choice => "choice",
            Self::Reason => "reason",
        })
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

/// Refuse `text` as the value of `field` unless it meets that field's rules.
pub(crate) fn validate_text(field: TextField, text: &str) -> Result<()> {
    find_fault(field, text).map_or(Ok(()), |fault| Err(Error::InvalidText { field, fault }))
}

/// Refuse `choice` when another decision of `component` already has it, so
/// the graph never holds two nodes for one decision with their history and
/// edges split between them. `revised` names the decision being revised,
/// which may keep its own choice.
pub(crate) fn ensure_new_choice(
    state: &ProjectState,
    component: &str,
    choice: &str,
    revised: Option<&str>,
) -> Result<()> {
    let key = choice_key(choice);
    let existing = state.decisions.iter().find(|(name, dec)| {
        Some(name.as_str()) != revised
            && dec.decision.component == component
            && choice_key(&dec.decision.choice) == key
    });
    match existing {
        Some((name, _)) => Err(Error::DuplicateChoice {
            component: component.into(),
            // clone: the error outlives the borrow of `state`.
            existing: name.clone(),
        }),
        None => Ok(()),
    }
}

/// Two choices that differ only in case or in runs of whitespace have the
/// same key, so a trailing space or a doubled gap cannot pass as new.
fn choice_key(choice: &str) -> String {
    choice
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn find_fault(field: TextField, text: &str) -> Option<TextFault> {
    let len = text.len();
    let reasoning = text.trim().len();
    let max = match field {
        TextField::Choice => MAX_CHOICE_BYTES,
        TextField::Reason => MAX_TEXT_FIELD_BYTES,
    };
    if reasoning == 0 {
        return Some(TextFault::Blank);
    }
    if len > max {
        return Some(TextFault::TooLong { max, len });
    }
    if field == TextField::Reason && reasoning < MIN_REASON_BYTES {
        return Some(TextFault::TooShort {
            min: MIN_REASON_BYTES,
            len: reasoning,
        });
    }
    find_character(field, text).or_else(|| find_tool_markup(text))
}

fn find_character(field: TextField, text: &str) -> Option<TextFault> {
    text.char_indices()
        .find_map(|(at, found)| match (field, found) {
            (TextField::Choice, '\n') => Some(TextFault::LineBreak { at }),
            (TextField::Reason, '\n' | '\t') => None,
            (TextField::Choice | TextField::Reason, _) => {
                character_class(found).map(|_| TextFault::Character { found, at })
            }
        })
}

/// The class of a character no decision text may hold, or `None` for one
/// it may. Line and tab are classed as controls here; a reason allows them.
fn character_class(c: char) -> Option<&'static str> {
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

    fn fault(field: TextField, text: &str) -> Option<TextFault> {
        match validate_text(field, text) {
            Ok(()) => None,
            Err(Error::InvalidText {
                field: found,
                fault,
            }) => {
                assert_eq!(found, field);
                Some(fault)
            }
            Err(other) => panic!("expected InvalidText, got {other}"),
        }
    }

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
            ("<invoke name=\"advance\">".into(), "<invoke".into()),
            ("</function_calls>".into(), "</function_calls".into()),
            ("</choice>".into(), "</choice>".into()),
            ("</reason>".into(), "</reason>".into()),
            ("</tags>".into(), "</tags>".into()),
            ("</code_refs>".into(), "</code_refs>".into()),
            ("</anchors>".into(), "</anchors>".into()),
            ("</alternatives>".into(), "</alternatives>".into()),
            (format!("</{ns}parameter>"), format!("</{ns}parameter")),
            (
                format!("<{ns}invoke name=\"advance\">"),
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

    #[test]
    fn the_message_names_the_field_and_the_fault() {
        let err = validate_text(TextField::Reason, &format!("{REASON}\u{202E}")).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "`reason` contains bidirectional control U+202E at byte {}",
                REASON.len()
            )
        );
    }
}
