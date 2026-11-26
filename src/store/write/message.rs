//! The message of a write's history commit: the tool that wrote as its
//! subject, and the `Origin:` and `Session:` trailers the caller passed.
//! The journal carries it, so the opener that finishes a stopped write
//! commits with the caller's trailers.

use serde::{Deserialize, Serialize};

use crate::store::model::{TextField, fault};
use crate::{Error, Result};

/// The writes that commit to the graph's history, named by its subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Tool {
    Init,
    Fmt,
    Migrate,
    Apply,
}

impl Tool {
    #[must_use]
    const fn as_str(self) -> &'static str {
        match self {
            Self::Init => "init",
            Self::Fmt => "fmt",
            Self::Migrate => "migrate",
            Self::Apply => "apply",
        }
    }
}

/// The message of a write's history commit: the tool as its subject, and
/// the `Origin:` and `Session:` trailers the caller passed, each one line.
/// The journal carries it, checked again when it is read back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "MessageFields")]
pub(crate) struct CommitMessage {
    tool: Tool,
    #[serde(skip_serializing_if = "Option::is_none")]
    origin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageFields {
    tool: Tool,
    origin: Option<String>,
    session: Option<String>,
}

impl TryFrom<MessageFields> for CommitMessage {
    type Error = Error;

    fn try_from(fields: MessageFields) -> Result<Self> {
        Self::new(fields.tool, fields.origin, fields.session)
    }
}

impl CommitMessage {
    /// Fails when the origin or the session breaks the one-line rule of
    /// `store::model::text`: a line break in either would forge a trailer.
    pub(in crate::store) fn new(
        tool: Tool,
        origin: Option<String>,
        session: Option<String>,
    ) -> Result<Self> {
        for (field, value) in [(TextField::Origin, &origin), (TextField::Session, &session)] {
            if let Some(fault) = value.as_deref().and_then(|text| fault(field, text)) {
                return Err(Error::TrailerText { field, fault });
            }
        }
        Ok(Self {
            tool,
            origin,
            session,
        })
    }

    /// The message of a write that carries no trailer, such as a command's.
    #[must_use]
    pub(in crate::store) const fn of(tool: Tool) -> Self {
        Self {
            tool,
            origin: None,
            session: None,
        }
    }

    /// The commit message: the subject, then the trailers the write has,
    /// as a paragraph of their own.
    #[must_use]
    pub(super) fn render(&self) -> String {
        let mut message = self.tool.as_str().to_owned();
        let trailers = [("Origin", &self.origin), ("Session", &self.session)];
        let mut first = true;
        for (key, value) in trailers {
            if let Some(value) = value {
                message.push_str(if first { "\n\n" } else { "\n" });
                first = false;
                message.push_str(&format!("{key}: {value}"));
            }
        }
        message.push('\n');
        message
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_names_the_tool_and_carries_the_trailers_it_has() {
        let both = CommitMessage::new(Tool::Fmt, Some("plan row".into()), Some("s-1".into()));
        assert_eq!(
            both.unwrap().render(),
            "fmt\n\nOrigin: plan row\nSession: s-1\n"
        );
        let session = CommitMessage::new(Tool::Migrate, None, Some("s-1".into()));
        assert_eq!(session.unwrap().render(), "migrate\n\nSession: s-1\n");
        assert_eq!(CommitMessage::of(Tool::Init).render(), "init\n");
    }

    #[test]
    fn a_trailer_that_is_not_one_plain_line_is_refused() {
        for (origin, session, field) in [
            (Some("a\nSession: forged"), None, TextField::Origin),
            (None, Some(" "), TextField::Session),
            (Some("ok"), Some("bidi \u{202E}"), TextField::Session),
        ] {
            let err = CommitMessage::new(
                Tool::Fmt,
                origin.map(str::to_owned),
                session.map(str::to_owned),
            )
            .unwrap_err();
            assert!(
                matches!(err, Error::TrailerText { field: f, .. } if f == field),
                "{err:?}"
            );
        }
        let long = "o".repeat(201);
        assert!(CommitMessage::new(Tool::Fmt, Some(long), None).is_err());
    }

    #[test]
    fn a_message_reads_back_from_the_journal_and_is_checked_again() {
        let message = CommitMessage::new(Tool::Fmt, Some("row".into()), Some("s".into())).unwrap();
        let text = toml::to_string(&message).unwrap();
        assert_eq!(toml::from_str::<CommitMessage>(&text).unwrap(), message);
        let bare = toml::to_string(&CommitMessage::of(Tool::Init)).unwrap();
        assert_eq!(bare, "tool = \"init\"\n");

        let forged = "tool = \"fmt\"\norigin = \"a\\nSession: b\"\n";
        assert!(toml::from_str::<CommitMessage>(forged).is_err());
    }
}
