//! The format version `project.toml` states, and the refusal of a store in
//! a format this binary does not read.
//!
//! A format version is `MAJOR.MINOR.PATCH`, ordered by its numbers and
//! independent of the crate's version. The key holding it was renamed with
//! the format: `trurl_version` in the pre-release `trurl` builds,
//! `trurlic_version` from 0.2.0 to 0.4.0, `format_version` since 0.4.1. It
//! is read under any of them, so an old store is told to migrate instead of
//! failing on a missing key. An older version the migration registry names
//! is told to migrate; one it does not name was never written by trurlic,
//! and is refused like one no chain of steps leads from to the current
//! format.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize, Serializer};

use crate::{Error, Result};

use super::load::read_node_bytes;
use super::schema::{FORMAT_VERSION, PROJECT_FILE, Project};
use super::{Store, migration};

/// The keys that have held the format version, newest first.
const VERSION_KEYS: [&str; 3] = ["format_version", "trurlic_version", "trurl_version"];

/// A format version, `MAJOR.MINOR.PATCH` in decimal without leading zeros.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FormatVersion {
    major: u32,
    minor: u32,
    patch: u32,
}

impl FormatVersion {
    pub(crate) const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// `None` unless `text` is exactly the form [`fmt::Display`] writes, so
    /// `0.4`, `0.04.0`, `+0.4.0` and ` 0.4.0` are refused, never read as
    /// some other version.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split('.').map(|part| {
            let canonical = part.bytes().all(|byte| byte.is_ascii_digit())
                && (part == "0" || !part.starts_with('0'));
            part.parse::<u32>().ok().filter(|_| canonical)
        });
        let version = Self::new(parts.next()??, parts.next()??, parts.next()??);
        parts.next().is_none().then_some(version)
    }
}

impl fmt::Display for FormatVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl Serialize for FormatVersion {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for FormatVersion {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        deserializer.deserialize_str(FormatVersionVisitor)
    }
}

struct FormatVersionVisitor;

impl Visitor<'_> for FormatVersionVisitor {
    type Value = FormatVersion;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a format version MAJOR.MINOR.PATCH such as 0.4.1")
    }

    fn visit_str<E: de::Error>(self, text: &str) -> std::result::Result<FormatVersion, E> {
        FormatVersion::parse(text).ok_or_else(|| E::invalid_value(de::Unexpected::Str(text), &self))
    }
}

/// The newest of the keys that have held the format version that
/// `project`, a `project.toml` table, has.
pub(super) fn version_key(project: &toml::Table) -> Option<&'static str> {
    VERSION_KEYS
        .into_iter()
        .find(|key| project.contains_key(*key))
}

/// The version `project`, the table of the `project.toml` at `path`,
/// states under [`version_key`].
fn stated_version(path: &Path, project: &toml::Table) -> Result<FormatVersion> {
    let value = version_key(project)
        .and_then(|key| project.get(key))
        .ok_or_else(|| Error::MissingFormatVersion {
            path: path.to_path_buf(),
        })?;
    let bad = |text: String| Error::BadFormatVersion {
        path: path.to_path_buf(),
        text,
    };
    match value {
        // clone: the error owns the text it names.
        toml::Value::String(text) => FormatVersion::parse(text).ok_or_else(|| bad(text.clone())),
        other => Err(bad(other.to_string())),
    }
}

/// Whether the store whose `project.toml` at `path` has the table
/// `project` is read as it is or migrated first. A newer format, and an
/// older one the migration registry does not name, are refused.
pub(super) fn standing(path: &Path, project: &toml::Table) -> Result<Standing> {
    let found = stated_version(path, project)?;
    if found == FORMAT_VERSION {
        Ok(Standing::Current)
    } else if found > FORMAT_VERSION {
        Err(Error::VersionMismatch {
            path: path.to_path_buf(),
            found,
            expected: FORMAT_VERSION,
        })
    } else if migration::names(found) {
        Ok(Standing::Older(found))
    } else {
        Err(Error::NoMigration {
            path: path.to_path_buf(),
            found,
            expected: FORMAT_VERSION,
        })
    }
}

/// Where a store stands against [`FORMAT_VERSION`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Standing {
    Current,
    /// A format a migration step names.
    Older(FormatVersion),
}

/// Refuse the store of [`standing`] unless it is in the current format.
fn ensure_current(path: &Path, project: &toml::Table) -> Result<()> {
    match standing(path, project)? {
        Standing::Current => Ok(()),
        Standing::Older(found) => Err(Error::VersionMismatch {
            path: path.to_path_buf(),
            found,
            expected: FORMAT_VERSION,
        }),
    }
}

/// The TOML file at `path` as a table, with its bytes; `project.toml` and
/// a node file are refused unread past their cap.
pub(super) fn read_table(path: &Path) -> Result<(toml::Table, Vec<u8>)> {
    let bytes = read_node_bytes(path)?;
    let table = toml::from_slice(&bytes).map_err(Error::toml(path))?;
    Ok((table, bytes))
}

impl Store {
    pub(super) fn project_path(&self) -> PathBuf {
        self.root.join(PROJECT_FILE)
    }

    /// Refuse a store whose format is not [`FORMAT_VERSION`], before
    /// anything else in it is read: an older one names `trurlic migrate`.
    pub fn check_version(&self) -> Result<()> {
        let path = self.project_path();
        ensure_current(&path, &read_table(&path)?.0)
    }

    /// `project.toml` in the current format and its bytes, read once. A
    /// store in another format is refused before the file is parsed as the
    /// current one, so the error names the version, not a missing key.
    pub(super) fn read_current_project(&self) -> Result<(Project, Vec<u8>)> {
        let path = self.project_path();
        let (project, bytes) = read_table(&path)?;
        ensure_current(&path, &project)?;
        let project = project.try_into().map_err(Error::toml(&path))?;
        Ok((project, bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::{setup_store, setup_store_with_version};
    use tempfile::TempDir;

    #[test]
    fn parse_reads_what_display_writes() {
        for (text, version) in [
            ("0.4.1", FormatVersion::new(0, 4, 1)),
            ("10.0.20", FormatVersion::new(10, 0, 20)),
            ("4294967295.0.0", FormatVersion::new(u32::MAX, 0, 0)),
        ] {
            assert_eq!(FormatVersion::parse(text), Some(version));
            assert_eq!(version.to_string(), text);
        }
    }

    #[test]
    fn parse_refuses_every_other_form() {
        for text in [
            "",
            "garbage",
            "0.4",
            "0.4.1.0",
            "0.04.1",
            "+0.4.1",
            "0.4.-1",
            " 0.4.1",
            "0.4.1 ",
            "0..1",
            "0.4.1-rc",
            "4294967296.0.0",
            "v0.4.1",
        ] {
            assert_eq!(FormatVersion::parse(text), None, "{text:?}");
        }
    }

    #[test]
    fn versions_order_by_number_not_text() {
        assert!(FormatVersion::new(0, 10, 0) > FormatVersion::new(0, 9, 9));
        assert!(FormatVersion::new(1, 0, 0) > FormatVersion::new(0, 99, 99));
        assert!(FormatVersion::new(0, 4, 1) > FormatVersion::new(0, 4, 0));
    }

    #[test]
    fn the_newest_version_key_wins() {
        let path = Path::new("project.toml");
        let project: toml::Table =
            toml::from_str("trurl_version = \"0.1.0\"\nformat_version = \"0.4.1\"").unwrap();
        assert_eq!(
            stated_version(path, &project).unwrap(),
            FormatVersion::new(0, 4, 1)
        );
    }

    #[test]
    fn a_store_in_the_current_format_passes() {
        let tmp = TempDir::new().unwrap();
        setup_store(tmp.path()).check_version().unwrap();
    }

    #[test]
    fn an_unparseable_version_is_refused_by_load() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store_with_version(tmp.path(), "0.4.garbage");

        for err in [
            store.check_version().unwrap_err(),
            store.load_state().map(|_| ()).unwrap_err(),
        ] {
            assert!(
                matches!(&err, Error::BadFormatVersion { path, text }
                    if *path == store.project_path() && text == "0.4.garbage"),
                "{err:?}"
            );
            let message = err.to_string();
            assert!(message.contains("project.toml"), "{message}");
            assert!(message.contains("`0.4.garbage`"), "{message}");
        }
    }

    #[test]
    fn a_version_that_is_not_a_string_is_refused_with_its_text() {
        let path = Path::new("project.toml");
        let project: toml::Table = toml::from_str("trurlic_version = 0.4").unwrap();
        let err = stated_version(path, &project).unwrap_err();
        assert!(
            matches!(&err, Error::BadFormatVersion { text, .. } if text == "0.4"),
            "{err:?}"
        );
    }

    #[test]
    fn a_missing_version_is_refused_naming_the_file() {
        let path = Path::new("project.toml");
        let project: toml::Table = toml::from_str("[project]\nname = \"x\"").unwrap();
        let err = stated_version(path, &project).unwrap_err();
        assert!(
            matches!(&err, Error::MissingFormatVersion { path: p } if p == path),
            "{err:?}"
        );
    }

    #[test]
    fn a_newer_format_is_refused_naming_both_versions() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store_with_version(tmp.path(), "99.0.0");

        for err in [
            store.check_version().unwrap_err(),
            store.load_state().map(|_| ()).unwrap_err(),
        ] {
            assert!(
                matches!(&err, Error::VersionMismatch { found, expected: FORMAT_VERSION, .. }
                    if *found == FormatVersion::new(99, 0, 0)),
                "{err:?}"
            );
            let message = err.to_string();
            assert!(message.contains("99.0.0"), "{message}");
            assert!(message.contains(&FORMAT_VERSION.to_string()), "{message}");
            assert!(message.contains("upgrade trurlic"), "{message}");
        }
    }

    #[test]
    fn an_older_format_is_refused_naming_migrate() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store_with_version(tmp.path(), "0.4.1");

        for err in [
            store.check_version().unwrap_err(),
            store.load_state().map(|_| ()).unwrap_err(),
        ] {
            assert!(
                matches!(&err, Error::VersionMismatch { found, .. }
                    if *found == FormatVersion::new(0, 4, 1)),
                "{err:?}"
            );
            assert!(err.to_string().contains("run `trurlic migrate`"), "{err}");
        }
    }

    #[test]
    fn an_older_format_no_step_names_is_refused_naming_both_versions() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store_with_version(tmp.path(), "0.3.7");

        let err = store.check_version().unwrap_err();
        assert!(
            matches!(&err, Error::NoMigration { found, expected: FORMAT_VERSION, .. }
                if *found == FormatVersion::new(0, 3, 7)),
            "{err:?}"
        );
        assert!(err.to_string().contains("0.3.7"), "{err}");
    }
}
