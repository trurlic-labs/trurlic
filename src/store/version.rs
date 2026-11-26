//! The format version `project.toml` states, and the refusal of a store in
//! a format this binary does not read.
//!
//! The key holding the version ([`FormatVersion`]) was renamed with the
//! format: `trurl_version` in the pre-release `trurl` builds,
//! `trurlic_version` from 0.2.0 to 0.4.0, `format_version` since 0.4.1. It
//! is read under any of them, so an old store is told to migrate instead of
//! failing on a missing key. An older version the migration registry names
//! is told to migrate; one it does not name was never written by trurlic,
//! and is refused like one no chain of steps leads from to the current
//! format.

use std::path::{Path, PathBuf};

use crate::store::model::{FORMAT_VERSION, FormatVersion, PROJECT_FILE, Placement, Project};
use crate::{Error, Result};

use super::load::read_node_bytes;
use super::{Store, migration};

/// The keys that have held the format version, newest first.
const VERSION_KEYS: [&str; 3] = ["format_version", "trurlic_version", "trurl_version"];

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

    /// `project.toml` as a table and its bytes, refused unless it states
    /// [`FORMAT_VERSION`], before anything else in the store is read: an
    /// older format names `trurlic migrate`.
    pub(super) fn read_current_table(&self) -> Result<(toml::Table, Vec<u8>)> {
        let path = self.project_path();
        let (table, bytes) = read_table(&path)?;
        ensure_current(&path, &table)?;
        Ok((table, bytes))
    }

    /// The `project.toml` table [`read_current_table`] read, as a
    /// [`Project`].
    ///
    /// [`read_current_table`]: Self::read_current_table
    pub(super) fn project_of(&self, table: toml::Table) -> Result<Project> {
        table.try_into().map_err(Error::toml(&self.project_path()))
    }

    /// The placement `project.toml` states, read as the current format.
    /// No lock is taken: a commit replaces `project.toml` by a rename, so
    /// the file is read whole, before or after it.
    pub fn stated_placement(&self) -> Result<Placement> {
        Ok(self.read_current_project()?.0.placement)
    }

    /// `project.toml` in the current format and its bytes, read once. A
    /// store in another format is refused before the file is parsed as the
    /// current one, so the error names the version, not a missing key.
    pub(super) fn read_current_project(&self) -> Result<(Project, Vec<u8>)> {
        let (table, bytes) = self.read_current_table()?;
        Ok((self.project_of(table)?, bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::{setup_store, setup_store_with_version};
    use tempfile::TempDir;

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
        setup_store(tmp.path()).read_current_table().unwrap();
    }

    #[test]
    fn an_unparseable_version_is_refused_by_load() {
        let tmp = TempDir::new().unwrap();
        let store = setup_store_with_version(tmp.path(), "0.4.garbage");

        for err in [
            store.read_current_table().unwrap_err(),
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
            store.read_current_table().unwrap_err(),
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
            store.read_current_table().unwrap_err(),
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

        let err = store.read_current_table().unwrap_err();
        assert!(
            matches!(&err, Error::NoMigration { found, expected: FORMAT_VERSION, .. }
                if *found == FormatVersion::new(0, 3, 7)),
            "{err:?}"
        );
        assert!(err.to_string().contains("0.3.7"), "{err}");
    }
}
