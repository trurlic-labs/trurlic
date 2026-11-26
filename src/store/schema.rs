//! The files of a store in format 0.5, as types.
//!
//! A node's name is its file stem; no file holds it. Edges live in the file
//! of their source: a component lists what it `connects_to`, a decision
//! what it `depends_on`. Every type refuses a key it does not know, and a
//! value the type cannot hold (a status its fields contradict, an anchor
//! path that leaves the code root) fails the read of its file. Node files
//! are written only in the canonical form of `canonical.rs`.

use std::collections::BTreeSet;
use std::fmt;

use serde::Deserialize;

use super::limits::{MAX_ANCHOR_PATH_BYTES, MAX_ANCHOR_SYMBOL_BYTES};
use super::quality::Quality;
use super::text::{character_class, hidden_character};
use super::time::Timestamp;
use super::version::FormatVersion;

// -- project.toml ------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub format_version: FormatVersion,
    pub name: String,
    pub description: String,
    pub placement: Placement,
    /// The write rules beyond the built-in ones; absent means none.
    #[serde(default)]
    pub quality: Quality,
}

/// Where the graph is versioned: in the project's git, or out of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    /// The project's git versions `.trurlic/`; every worktree has its own.
    Tracked,
    /// `.trurlic/` stays out of the project's git.
    Shared,
}

impl Placement {
    pub const ALL: [Self; 2] = [Self::Tracked, Self::Shared];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tracked => "tracked",
            Self::Shared => "shared",
        }
    }
}

// -- components/<name>.toml --------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Component {
    pub description: String,
    #[serde(default)]
    pub connects_to: BTreeSet<String>,
}

// -- decisions/<name>.toml ---------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "DecisionFields")]
pub struct Decision {
    /// A component's name, or [`PROJECT_COMPONENT`] for a project-wide rule.
    pub component: String,
    pub choice: String,
    pub reason: String,
    pub alternatives: Vec<String>,
    pub tags: BTreeSet<String>,
    pub status: Status,
    /// The task or plan row that recorded the decision, as its writer named it.
    pub origin: Option<String>,
    pub created: Timestamp,
    pub anchors: BTreeSet<Anchor>,
    pub depends_on: BTreeSet<String>,
}

/// Whether the owner has accepted a decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// `accepted_text` is the text the owner accepted, kept while a
    /// revision of an accepted decision waits for the owner.
    Proposed { accepted_text: Option<AcceptedText> },
    /// `via` is how the owner accepted it; `None` for a decision accepted
    /// before acceptance was recorded.
    Accepted { via: Option<AcceptedVia> },
}

impl Status {
    #[must_use]
    pub const fn kind(&self) -> StatusName {
        match self {
            Self::Proposed { .. } => StatusName::Proposed,
            Self::Accepted { .. } => StatusName::Accepted,
        }
    }

    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.kind().as_str()
    }

    /// Whether the owner accepted the decision once: an accepted one, or a
    /// proposed revision of one. The accepted text keeps no anchors, so all
    /// of a revision's anchors, one the revising op added included, are
    /// judged as the accepted decision's.
    #[must_use]
    pub const fn ever_accepted(&self) -> bool {
        match self {
            Self::Accepted { .. }
            | Self::Proposed {
                accepted_text: Some(_),
            } => true,
            Self::Proposed {
                accepted_text: None,
            } => false,
        }
    }
}

/// A [`Status`] without its fields: what the `status` key of a decision
/// file names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusName {
    Proposed,
    Accepted,
}

impl StatusName {
    pub const ALL: [Self; 2] = [Self::Proposed, Self::Accepted];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Accepted => "accepted",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcceptedVia {
    /// The owner answered a form the MCP client showed.
    Elicitation,
    /// The owner accepted it on a terminal.
    Cli,
}

impl AcceptedVia {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Elicitation => "elicitation",
            Self::Cli => "cli",
        }
    }
}

/// The text of an accepted decision whose revision is proposed, and how
/// the owner accepted it: a revision back to this text accepts the decision
/// again as it was accepted.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptedText {
    pub choice: String,
    pub reason: String,
    #[serde(default)]
    pub alternatives: Vec<String>,
    /// The accepted decision's `accepted_via`.
    #[serde(rename = "accepted_via")]
    pub via: Option<AcceptedVia>,
}

/// A decision file as TOML states it, before its status is read.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionFields {
    component: String,
    choice: String,
    reason: String,
    #[serde(default)]
    alternatives: Vec<String>,
    #[serde(default)]
    tags: BTreeSet<String>,
    status: StatusName,
    accepted_via: Option<AcceptedVia>,
    accepted_text: Option<AcceptedText>,
    origin: Option<String>,
    created: Timestamp,
    #[serde(default)]
    anchors: BTreeSet<Anchor>,
    #[serde(default)]
    depends_on: BTreeSet<String>,
}

/// A status its fields contradict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusFault {
    /// `accepted_via` on a proposed decision.
    ProposedWithVia,
    /// `accepted_text` on an accepted decision.
    AcceptedWithText,
}

impl fmt::Display for StatusFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ProposedWithVia => {
                "`accepted_via` is set, but `status` is \"proposed\": only an accepted \
                 decision states how it was accepted"
            }
            Self::AcceptedWithText => {
                "`accepted_text` is set, but `status` is \"accepted\": it holds the \
                 accepted text only while a revision is proposed"
            }
        })
    }
}

impl TryFrom<DecisionFields> for Decision {
    type Error = StatusFault;

    fn try_from(fields: DecisionFields) -> Result<Self, StatusFault> {
        let status = match (fields.status, fields.accepted_via, fields.accepted_text) {
            (StatusName::Proposed, Some(_), _) => return Err(StatusFault::ProposedWithVia),
            (StatusName::Accepted, _, Some(_)) => return Err(StatusFault::AcceptedWithText),
            (StatusName::Proposed, None, accepted_text) => Status::Proposed { accepted_text },
            (StatusName::Accepted, via, None) => Status::Accepted { via },
        };
        Ok(Self {
            component: fields.component,
            choice: fields.choice,
            reason: fields.reason,
            alternatives: fields.alternatives,
            tags: fields.tags,
            status,
            origin: fields.origin,
            created: fields.created,
            anchors: fields.anchors,
            depends_on: fields.depends_on,
        })
    }
}

// -- anchors -----------------------------------------------------------------

/// Where a decision applies in the code: a file or a directory, relative to
/// the code root, and optionally a symbol in it. An anchor always holds a
/// path in its one spelling, so two anchors are equal exactly when they name
/// the same place.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(try_from = "AnchorFields")]
pub struct Anchor {
    file: String,
    symbol: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AnchorFields {
    file: String,
    symbol: Option<String>,
}

impl TryFrom<AnchorFields> for Anchor {
    type Error = AnchorFault;

    fn try_from(fields: AnchorFields) -> Result<Self, AnchorFault> {
        Self::new(fields.file, fields.symbol)
    }
}

impl Anchor {
    /// `file`, `/`-separated and relative to the code root, refused when it
    /// could name a place outside the root or name one place two ways.
    pub fn new(file: String, symbol: Option<String>) -> Result<Self, AnchorFault> {
        path_fault(&file).map_or(Ok(()), Err)?;
        if let Some(symbol) = &symbol {
            symbol_fault(symbol).map_or(Ok(()), Err)?;
        }
        Ok(Self { file, symbol })
    }

    #[must_use]
    pub fn file(&self) -> &str {
        &self.file
    }

    #[must_use]
    pub fn symbol(&self) -> Option<&str> {
        self.symbol.as_deref()
    }

    /// Whether this anchor and `path` name one place, or one is under the
    /// other as a directory: a directory holds the anchors in it, and a
    /// directory anchor applies to every file under it.
    #[must_use]
    pub fn overlaps(&self, path: &str) -> bool {
        let under = |inner: &str, outer: &str| {
            inner
                .strip_prefix(outer)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
        };
        under(&self.file, path) || under(path, &self.file)
    }
}

impl fmt::Display for Anchor {
    /// `file` or `file::symbol`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.file)?;
        match &self.symbol {
            Some(symbol) => write!(f, "::{symbol}"),
            None => Ok(()),
        }
    }
}

/// Why an anchor is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorFault {
    EmptyPath,
    PathTooLong,
    Absolute,
    /// A `..` segment, which climbs out of the directory it names.
    Traversal,
    /// An empty or `.` segment: the path has a shorter spelling.
    Unnormalized,
    Backslash,
    /// A character no text may hold (`store::text`), at its byte offset.
    PathCharacter {
        found: char,
        at: usize,
    },
    EmptySymbol,
    SymbolTooLong,
    SymbolCharacter {
        found: char,
        at: usize,
    },
}

impl fmt::Display for AnchorFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPath => f.write_str("an anchor's `file` is empty"),
            Self::PathTooLong => write!(
                f,
                "an anchor's `file` is longer than {MAX_ANCHOR_PATH_BYTES} bytes"
            ),
            Self::Absolute => f.write_str("an anchor's `file` must be relative to the code root"),
            Self::Traversal => f.write_str("an anchor's `file` must not hold a `..` segment"),
            Self::Unnormalized => f.write_str(
                "an anchor's `file` must not hold an empty or `.` segment, \
                 nor end with `/`",
            ),
            Self::Backslash => f.write_str("an anchor's `file` separates segments with `/`"),
            Self::PathCharacter { found, at } => character(f, "file", *found, *at),
            Self::EmptySymbol => f.write_str("an anchor's `symbol` is empty"),
            Self::SymbolTooLong => write!(
                f,
                "an anchor's `symbol` is longer than {MAX_ANCHOR_SYMBOL_BYTES} bytes"
            ),
            Self::SymbolCharacter { found, at } => character(f, "symbol", *found, *at),
        }
    }
}

fn character(f: &mut fmt::Formatter<'_>, key: &str, found: char, at: usize) -> fmt::Result {
    write!(
        f,
        "an anchor's `{key}` contains {} U+{:04X} at byte {at}",
        character_class(found).unwrap_or("character"),
        u32::from(found)
    )
}

fn path_fault(file: &str) -> Option<AnchorFault> {
    let first = file.split('/').next().unwrap_or_default();
    if file.is_empty() {
        Some(AnchorFault::EmptyPath)
    } else if file.len() > MAX_ANCHOR_PATH_BYTES {
        Some(AnchorFault::PathTooLong)
    } else if let Some((found, at)) = hidden_character(file) {
        Some(AnchorFault::PathCharacter { found, at })
    } else if file.contains('\\') {
        Some(AnchorFault::Backslash)
    } else if file.starts_with('/') || first.ends_with(':') {
        // `C:` and `C:/x` name a drive, not a directory below the root.
        Some(AnchorFault::Absolute)
    } else if file.split('/').any(|segment| segment == "..") {
        Some(AnchorFault::Traversal)
    } else if file
        .split('/')
        .any(|segment| segment.is_empty() || segment == ".")
    {
        Some(AnchorFault::Unnormalized)
    } else {
        None
    }
}

fn symbol_fault(symbol: &str) -> Option<AnchorFault> {
    if symbol.trim().is_empty() {
        Some(AnchorFault::EmptySymbol)
    } else if symbol.len() > MAX_ANCHOR_SYMBOL_BYTES {
        Some(AnchorFault::SymbolTooLong)
    } else if let Some((found, at)) = hidden_character(symbol) {
        Some(AnchorFault::SymbolCharacter { found, at })
    } else {
        None
    }
}

// -- Constants ---------------------------------------------------------------

/// The format this binary reads and writes. A change to the on-disk shape
/// raises it and appends the step that reaches it to the migration registry
/// (`store/migration/steps.rs`).
pub const FORMAT_VERSION: FormatVersion = FormatVersion::new(0, 5, 0);

/// The component every project has without a file: its decisions are the
/// rules that hold everywhere.
pub const PROJECT_COMPONENT: &str = "project";

pub const STORE_DIR: &str = ".trurlic";

pub const PROJECT_FILE: &str = "project.toml";

pub const COMPONENTS_DIR: &str = "components";

pub const DECISIONS_DIR: &str = "decisions";

pub const STATE_DIR: &str = ".state";

#[cfg(test)]
mod tests {
    use super::*;

    const DECISION: &str = r#"
component = "auth"
choice = "Sign tokens"
reason = "Verification needs no session store"
status = "accepted"
created = "2025-06-01T10:30:00Z"
"#;

    fn decision(extra: &str) -> Result<Decision, toml::de::Error> {
        toml::from_str(&format!("{DECISION}{extra}"))
    }

    #[test]
    fn a_minimal_decision_reads_with_empty_sets() {
        let read = decision("").unwrap();
        assert_eq!(read.status, Status::Accepted { via: None });
        assert!(read.alternatives.is_empty() && read.tags.is_empty());
        assert!(read.anchors.is_empty() && read.depends_on.is_empty());
        assert_eq!(read.origin, None);
    }

    #[test]
    fn status_and_its_fields_read_as_one_value() {
        let accepted = decision("accepted_via = \"cli\"\n").unwrap();
        assert_eq!(
            accepted.status,
            Status::Accepted {
                via: Some(AcceptedVia::Cli)
            }
        );
        let revision = toml::from_str::<Decision>(&format!(
            "{}\n[accepted_text]\nchoice = \"Old\"\nreason = \"The text the owner accepted\"\n",
            DECISION.replace("accepted", "proposed")
        ))
        .unwrap();
        let Status::Proposed {
            accepted_text: Some(text),
        } = revision.status
        else {
            panic!("{:?}", revision.status);
        };
        assert_eq!(text.choice, "Old");
        assert_eq!(text.via, None);
        let revision = toml::from_str::<Decision>(&format!(
            "{}\n[accepted_text]\nchoice = \"Old\"\nreason = \"The text the owner accepted\"\n\
             accepted_via = \"elicitation\"\n",
            DECISION.replace("accepted", "proposed")
        ))
        .unwrap();
        let Status::Proposed {
            accepted_text: Some(text),
        } = revision.status
        else {
            panic!("{:?}", revision.status);
        };
        assert_eq!(text.via, Some(AcceptedVia::Elicitation));
    }

    #[test]
    fn a_status_its_fields_contradict_is_refused() {
        let proposed = DECISION.replace("accepted", "proposed");
        let err =
            toml::from_str::<Decision>(&format!("{proposed}accepted_via = \"cli\"\n")).unwrap_err();
        assert!(err.message().contains("`accepted_via` is set"), "{err}");

        let err = decision("\n[accepted_text]\nchoice = \"Old\"\nreason = \"Old reason text\"\n")
            .unwrap_err();
        assert!(err.message().contains("`accepted_text` is set"), "{err}");
    }

    #[test]
    fn an_unknown_key_is_refused_in_every_type() {
        assert!(decision("name = \"sign-tokens\"\n").is_err());
        assert!(decision("anchors = [{ file = \"a.rs\", line = 3 }]\n").is_err());
        assert!(
            decision("\n[accepted_text]\nchoice = \"c\"\nreason = \"r\"\nweight = 1\n").is_err()
        );
        assert!(toml::from_str::<Component>("description = \"\"\nname = \"auth\"\n").is_err());
        let project = "format_version = \"0.5.0\"\nname = \"x\"\ndescription = \"\"\n\
                       placement = \"tracked\"\n";
        assert!(toml::from_str::<Project>(project).is_ok());
        assert!(toml::from_str::<Project>(&format!("{project}[budget]\n")).is_err());
        assert!(toml::from_str::<Project>(&format!("{project}[quality]\nlimit = 1\n")).is_err());
    }

    #[test]
    fn an_anchor_path_has_one_spelling_below_the_root() {
        for (file, fault) in [
            ("", AnchorFault::EmptyPath),
            ("/etc/passwd", AnchorFault::Absolute),
            ("C:/x.rs", AnchorFault::Absolute),
            ("D:", AnchorFault::Absolute),
            ("../x.rs", AnchorFault::Traversal),
            ("src/../../x.rs", AnchorFault::Traversal),
            ("src\\x.rs", AnchorFault::Backslash),
            (
                "src/\u{0}x.rs",
                AnchorFault::PathCharacter { found: '\0', at: 4 },
            ),
            (
                "src/\nx.rs",
                AnchorFault::PathCharacter { found: '\n', at: 4 },
            ),
            (
                "src/x\u{202E}.rs",
                AnchorFault::PathCharacter {
                    found: '\u{202E}',
                    at: 5,
                },
            ),
            ("./src/x.rs", AnchorFault::Unnormalized),
            ("src//x.rs", AnchorFault::Unnormalized),
            ("src/", AnchorFault::Unnormalized),
        ] {
            assert_eq!(Anchor::new(file.into(), None), Err(fault), "{file:?}");
        }
        let long = "x".repeat(MAX_ANCHOR_PATH_BYTES + 1);
        assert_eq!(Anchor::new(long, None), Err(AnchorFault::PathTooLong));
        for file in ["src/store", "src/a..b.rs", "src/a:b.rs", "README.md"] {
            assert!(Anchor::new(file.into(), None).is_ok(), "{file}");
        }
    }

    #[test]
    fn an_anchor_symbol_is_one_non_blank_line() {
        let anchor = |symbol: &str| Anchor::new("src/a.rs".into(), Some(symbol.into()));
        assert_eq!(anchor(" "), Err(AnchorFault::EmptySymbol));
        assert_eq!(
            anchor("a\nb"),
            Err(AnchorFault::SymbolCharacter { found: '\n', at: 1 })
        );
        assert_eq!(
            anchor("a\u{200B}b"),
            Err(AnchorFault::SymbolCharacter {
                found: '\u{200B}',
                at: 1
            })
        );
        assert_eq!(
            anchor(&"s".repeat(MAX_ANCHOR_SYMBOL_BYTES + 1)),
            Err(AnchorFault::SymbolTooLong)
        );
        assert_eq!(
            anchor("Store::load_state").unwrap().to_string(),
            "src/a.rs::Store::load_state"
        );
    }

    #[test]
    fn an_anchor_overlaps_its_file_the_directories_above_and_the_files_below() {
        let anchor = Anchor::new("src/store/load.rs".into(), None).unwrap();
        for path in ["src/store/load.rs", "src/store", "src"] {
            assert!(anchor.overlaps(path), "{path}");
        }
        for path in ["src/store/load", "src/sto", "src/store/loader.rs", "lib"] {
            assert!(!anchor.overlaps(path), "{path}");
        }
        let directory = Anchor::new("src/store".into(), None).unwrap();
        assert!(directory.overlaps("src/store/load.rs"));
        assert!(!directory.overlaps("src/storage/load.rs"));
    }

    #[test]
    fn anchors_order_by_file_then_symbol_with_the_bare_file_first() {
        let anchor = |file: &str, symbol: Option<&str>| {
            Anchor::new(file.into(), symbol.map(str::to_owned)).unwrap()
        };
        let set: BTreeSet<Anchor> = [
            anchor("b.rs", None),
            anchor("a.rs", Some("g")),
            anchor("a.rs", None),
            anchor("a.rs", Some("f")),
        ]
        .into();
        let order: Vec<String> = set.iter().map(ToString::to_string).collect();
        assert_eq!(order, ["a.rs", "a.rs::f", "a.rs::g", "b.rs"]);
    }
}
