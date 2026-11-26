//! `[quality]` of `project.toml` as a type: the project's limits and
//! patterns, each optional, refused at load when no decision could meet
//! one or a pattern is out of bounds. Patterns are held as text here and
//! compiled only by a write, `check` and the `status` lint section
//! (`store::quality`): a load never runs them.

use std::collections::BTreeSet;
use std::fmt;

use serde::Deserialize;

use super::limits::{MAX_PATTERN_BYTES, MAX_QUALITY_PATTERNS, MAX_WARN_MESSAGES_BYTES};
use super::text::{TextFault, TextField, fault};

/// The project's quality bar beyond the built-in rules. The default, an
/// absent table, adds nothing to them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(try_from = "QualityFields")]
pub struct Quality {
    /// Words of a reason, at least and at most.
    pub reason_words: WordRange,
    pub alternatives_max: Option<usize>,
    /// Words of each alternative, at most.
    pub alternative_words_max: Option<usize>,
    pub anchors_max: Option<usize>,
    /// A batch that records a decision gives an `origin`.
    pub require_origin: bool,
    /// The project's own rules, in the order a refusal numbers them.
    pub patterns: Vec<Pattern>,
}

impl Quality {
    /// Whether the table adds nothing to the built-in rules, so the
    /// canonical form leaves it out.
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WordRange {
    pub min: Option<usize>,
    pub max: Option<usize>,
}

/// A rule of the project's: text in `fields` that `pattern` matches is
/// refused, or warned of, with `message`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pattern {
    /// A regular expression in the syntax of the `regex` crate, which
    /// runs on a lazy DFA: no Unicode word boundary (`Bar::compile`).
    pub pattern: String,
    pub fields: BTreeSet<DecisionField>,
    pub level: Level,
    pub message: String,
}

/// What a pattern's match does to the write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Refuse,
    /// The write goes through with a warning in its result.
    Warn,
}

impl Level {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Refuse => "refuse",
            Self::Warn => "warn",
        }
    }
}

/// The fields of a decision the bar reads, and the names a pattern's
/// `fields` gives them. `origin` is the batch's, read once per batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionField {
    Alternatives,
    Anchors,
    Choice,
    Origin,
    Reason,
    Tags,
}

impl DecisionField {
    /// The fields of a decision file, in the order of its keys: the order
    /// the bar reads them in.
    pub const IN_FILE_ORDER: [Self; 6] = [
        Self::Choice,
        Self::Reason,
        Self::Alternatives,
        Self::Tags,
        Self::Origin,
        Self::Anchors,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Alternatives => "alternatives",
            Self::Anchors => "anchors",
            Self::Choice => "choice",
            Self::Origin => "origin",
            Self::Reason => "reason",
            Self::Tags => "tags",
        }
    }
}

/// `[quality]` as TOML states it, before it is checked.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QualityFields {
    #[serde(default)]
    reason_words: WordRange,
    alternatives_max: Option<usize>,
    alternative_words_max: Option<usize>,
    anchors_max: Option<usize>,
    #[serde(default)]
    require_origin: bool,
    #[serde(default)]
    patterns: Vec<Pattern>,
}

/// A `[quality]` table no write could meet, or one out of bounds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigFault {
    /// A maximum of 0, which every decision breaks: each holds a reason,
    /// an alternative and an anchor.
    ZeroMax {
        key: &'static str,
    },
    MinOverMax {
        min: usize,
        max: usize,
    },
    TooManyPatterns {
        count: usize,
    },
    EmptyPattern {
        index: usize,
    },
    PatternTooLong {
        index: usize,
        bytes: usize,
    },
    NoFields {
        index: usize,
    },
    Message {
        index: usize,
        fault: TextFault,
    },
    /// The messages of the `warn` patterns take `bytes` together, as a
    /// JSON string writes them, over [`MAX_WARN_MESSAGES_BYTES`].
    WarnMessagesTooLong {
        bytes: usize,
    },
}

impl fmt::Display for ConfigFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroMax { key } => write!(
                f,
                "`[quality] {key}` is 0, which every decision breaks; leave it out for no limit"
            ),
            Self::MinOverMax { min, max } => write!(
                f,
                "`[quality] reason_words` has a `min` of {min}, over its `max` of {max}"
            ),
            Self::TooManyPatterns { count } => write!(
                f,
                "`[quality] patterns` holds {count} patterns, over the limit of \
                 {MAX_QUALITY_PATTERNS}"
            ),
            Self::EmptyPattern { index } => {
                write!(f, "`[quality] patterns[{index}].pattern` is empty")
            }
            Self::PatternTooLong { index, bytes } => write!(
                f,
                "`[quality] patterns[{index}].pattern` is {bytes} bytes, over the limit of \
                 {MAX_PATTERN_BYTES}"
            ),
            Self::NoFields { index } => write!(
                f,
                "`[quality] patterns[{index}].fields` is empty, so the pattern reads nothing"
            ),
            Self::Message { index, fault } => {
                write!(f, "`[quality] patterns[{index}].message` {fault}")
            }
            Self::WarnMessagesTooLong { bytes } => write!(
                f,
                "the messages of the `warn` patterns take {bytes} bytes together, counting a \
                 `\"` or a `\\` twice, over the limit of {MAX_WARN_MESSAGES_BYTES}: an `apply` \
                 result carries each one it names"
            ),
        }
    }
}

impl TryFrom<QualityFields> for Quality {
    type Error = ConfigFault;

    fn try_from(fields: QualityFields) -> Result<Self, ConfigFault> {
        let maxima = [
            ("reason_words.max", fields.reason_words.max),
            ("alternatives_max", fields.alternatives_max),
            ("alternative_words_max", fields.alternative_words_max),
            ("anchors_max", fields.anchors_max),
        ];
        if let Some((key, _)) = maxima.into_iter().find(|(_, max)| *max == Some(0)) {
            return Err(ConfigFault::ZeroMax { key });
        }
        if let WordRange {
            min: Some(min),
            max: Some(max),
        } = fields.reason_words
            && min > max
        {
            return Err(ConfigFault::MinOverMax { min, max });
        }
        let count = fields.patterns.len();
        if count > MAX_QUALITY_PATTERNS {
            return Err(ConfigFault::TooManyPatterns { count });
        }
        for (index, pattern) in fields.patterns.iter().enumerate() {
            pattern_fault(index, pattern).map_or(Ok(()), Err)?;
        }
        let warned = fields
            .patterns
            .iter()
            .filter(|pattern| pattern.level == Level::Warn);
        let bytes = warned.map(|pattern| escaped_len(&pattern.message)).sum();
        if bytes > MAX_WARN_MESSAGES_BYTES {
            return Err(ConfigFault::WarnMessagesTooLong { bytes });
        }
        Ok(Self {
            reason_words: fields.reason_words,
            alternatives_max: fields.alternatives_max,
            alternative_words_max: fields.alternative_words_max,
            anchors_max: fields.anchors_max,
            require_origin: fields.require_origin,
            patterns: fields.patterns,
        })
    }
}

/// Bytes of `message` in a JSON string: each `"` and `\\` escaped. The
/// text rule of a message refuses every control character, which JSON
/// writes longer.
fn escaped_len(message: &str) -> usize {
    let escaped = message
        .bytes()
        .filter(|&byte| byte == b'"' || byte == b'\\');
    message.len() + escaped.count()
}

fn pattern_fault(index: usize, pattern: &Pattern) -> Option<ConfigFault> {
    let bytes = pattern.pattern.len();
    if bytes == 0 {
        Some(ConfigFault::EmptyPattern { index })
    } else if bytes > MAX_PATTERN_BYTES {
        Some(ConfigFault::PatternTooLong { index, bytes })
    } else if pattern.fields.is_empty() {
        Some(ConfigFault::NoFields { index })
    } else {
        fault(TextField::Message, &pattern.message)
            .map(|fault| ConfigFault::Message { index, fault })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quality(text: &str) -> Result<Quality, toml::de::Error> {
        toml::from_str(text)
    }

    const PATTERN: &str =
        "pattern = 'x'\nfields = [\"choice\"]\nlevel = \"warn\"\nmessage = \"M\"\n";

    #[test]
    fn every_key_is_optional_and_an_empty_table_is_the_default() {
        let empty = quality("").unwrap();
        assert!(empty.is_default());
        let full = quality(&format!(
            "reason_words = {{ min = 2, max = 9 }}\nalternatives_max = 3\n\
             alternative_words_max = 30\nanchors_max = 8\nrequire_origin = true\n\
             [[patterns]]\n{PATTERN}"
        ))
        .unwrap();
        assert_eq!(
            full.reason_words,
            WordRange {
                min: Some(2),
                max: Some(9)
            }
        );
        assert_eq!(
            (
                full.alternatives_max,
                full.alternative_words_max,
                full.anchors_max
            ),
            (Some(3), Some(30), Some(8))
        );
        assert!(full.require_origin);
        assert_eq!(full.patterns[0].fields, [DecisionField::Choice].into());
        assert_eq!(full.patterns[0].level, Level::Warn);
        assert!(!full.is_default());
    }

    #[test]
    fn a_key_or_value_the_table_has_no_place_for_is_refused() {
        for text in [
            "reason_word = { max = 9 }",
            "reason_words = { most = 9 }",
            "alternatives_max = -1",
            "require_origin = \"yes\"",
            "[[patterns]]\npattern = 'x'\nfields = [\"name\"]\nlevel = \"warn\"\nmessage = \"M\"",
            "[[patterns]]\npattern = 'x'\nfields = [\"choice\"]\nlevel = \"block\"\nmessage = \"M\"",
            "[[patterns]]\npattern = 'x'\nfields = [\"choice\"]\nlevel = \"warn\"",
            &format!("[[patterns]]\n{PATTERN}weight = 1"),
        ] {
            assert!(quality(text).is_err(), "{text}");
        }
    }

    #[test]
    fn a_table_no_write_could_meet_or_out_of_bounds_is_refused_naming_its_key() {
        let long = "p".repeat(MAX_PATTERN_BYTES + 1);
        let many = format!("[[patterns]]\n{PATTERN}").repeat(MAX_QUALITY_PATTERNS + 1);
        let cases = [
            ("anchors_max = 0".to_owned(), "`[quality] anchors_max` is 0"),
            (
                "reason_words = { max = 0 }".to_owned(),
                "`[quality] reason_words.max` is 0",
            ),
            (
                "reason_words = { min = 10, max = 9 }".to_owned(),
                "a `min` of 10, over its `max` of 9",
            ),
            (many, "holds 33 patterns, over the limit of 32"),
            (
                format!("[[patterns]]\n{}", PATTERN.replace("'x'", "''")),
                "`[quality] patterns[0].pattern` is empty",
            ),
            (
                format!(
                    "[[patterns]]\n{}",
                    PATTERN.replace("'x'", &format!("'{long}'"))
                ),
                "is 501 bytes, over the limit of 500",
            ),
            (
                format!("[[patterns]]\n{}", PATTERN.replace("[\"choice\"]", "[]")),
                "`[quality] patterns[0].fields` is empty",
            ),
            (
                format!(
                    "[[patterns]]\n{}",
                    PATTERN.replace("\"M\"", "\"two\\nlines\"")
                ),
                "`[quality] patterns[0].message` must be a single line",
            ),
        ];
        for (text, named) in cases {
            let err = quality(&text).unwrap_err();
            assert!(err.message().contains(named), "{named:?} not in {err}");
        }
        let at_limits = format!("[[patterns]]\n{PATTERN}").repeat(MAX_QUALITY_PATTERNS);
        assert!(quality(&at_limits).is_ok());
    }

    /// The `warn` messages together, a quote counted twice, fit the bound
    /// an `apply` result's budget sets; a `refuse` message does not count.
    #[test]
    fn the_warn_messages_together_are_bounded() {
        let warn = |message: &str| PATTERN.replace("\"M\"", &format!("'{message}'"));
        // Messages of 192 quotes, 384 bytes escaped each.
        let quotes = "\"".repeat(192);
        let fits = format!("[[patterns]]\n{}", warn(&quotes)).repeat(MAX_WARN_MESSAGES_BYTES / 384);
        assert!(quality(&fits).is_ok());
        let over = format!("{fits}[[patterns]]\n{}", warn("x"));
        let err = quality(&over).unwrap_err();
        let said = format!("take {} bytes together", MAX_WARN_MESSAGES_BYTES + 1);
        assert!(err.message().contains(&said), "{err}");
        let refusing = format!(
            "{fits}[[patterns]]\n{}",
            warn("x").replace("warn", "refuse")
        );
        assert!(quality(&refusing).is_ok());
    }
}
