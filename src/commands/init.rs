//! `trurl init` — create a new `.trurl/` directory.

use std::fs;
use std::io::Write;
use std::path::Path;

use crate::store::schema::{Project, ProjectFile};
use crate::store::{COMPONENTS_DIR, DECISIONS_DIR, FORMAT_VERSION, STATE_DIR, STORE_DIR, Store};
use crate::{Error, Result};

/// Create a new `.trurl/` directory in `cwd`.
pub fn init(cwd: &Path) -> Result<()> {
    let root = cwd.join(STORE_DIR);
    if root.exists() {
        return Err(Error::StoreExists(root));
    }

    fs::create_dir_all(root.join(COMPONENTS_DIR))?;
    fs::create_dir_all(root.join(DECISIONS_DIR))?;
    fs::create_dir_all(root.join(STATE_DIR).join("sessions"))?;
    fs::create_dir_all(root.join(STATE_DIR).join("tmp"))?;

    let name = cwd
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("my-project")
        .to_string();

    let project = ProjectFile {
        trurl_version: FORMAT_VERSION.into(),
        project: Project {
            name,
            description: String::new(),
        },
    };

    let store = Store::at(root);
    let lock = store.lock()?;
    store.write_atomic(&lock, &store.root().join("project.toml"), &project)?;
    drop(lock);

    append_gitignore(cwd)?;
    println!("Initialized .trurl/");
    Ok(())
}

fn append_gitignore(cwd: &Path) -> Result<()> {
    let path = cwd.join(".gitignore");
    let entry = ".trurl/.state/";

    if path.exists() {
        let content = fs::read_to_string(&path)?;
        if content.lines().any(|line| line.trim() == entry) {
            return Ok(());
        }
        let mut file = fs::OpenOptions::new().append(true).open(&path)?;
        if !content.is_empty() && !content.ends_with('\n') {
            writeln!(file)?;
        }
        writeln!(file, "{entry}")?;
    } else {
        fs::write(&path, format!("{entry}\n"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::schema::ProjectFile;
    use tempfile::TempDir;

    #[test]
    fn init_creates_directory_structure() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();

        let root = tmp.path().join(STORE_DIR);
        assert!(root.join("project.toml").is_file());
        assert!(root.join(COMPONENTS_DIR).is_dir());
        assert!(root.join(DECISIONS_DIR).is_dir());
        assert!(root.join(STATE_DIR).is_dir());
        assert!(root.join(STATE_DIR).join("sessions").is_dir());
        assert!(root.join(STATE_DIR).join("tmp").is_dir());
    }

    #[test]
    fn init_writes_valid_project_toml() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();

        let content = fs::read_to_string(tmp.path().join(STORE_DIR).join("project.toml")).unwrap();
        let project: ProjectFile = toml::from_str(&content).unwrap();
        assert_eq!(project.trurl_version, FORMAT_VERSION);
    }

    #[test]
    fn init_refuses_if_exists() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        let err = init(tmp.path()).unwrap_err();
        assert!(matches!(err, Error::StoreExists(_)));
    }

    #[test]
    fn init_creates_gitignore_when_absent() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();

        let content = fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
        assert!(content.contains(".trurl/.state/"));
    }

    #[test]
    fn init_appends_to_existing_gitignore() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join(".gitignore"), "/target/\n").unwrap();
        init(tmp.path()).unwrap();

        let content = fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
        assert!(content.starts_with("/target/\n"));
        assert!(content.contains(".trurl/.state/"));
    }

    #[test]
    fn init_appends_newline_before_entry_if_missing() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join(".gitignore"), "/target/").unwrap();
        init(tmp.path()).unwrap();

        let content = fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
        assert!(content.contains("/target/\n.trurl/.state/\n"));
    }

    #[test]
    fn init_does_not_duplicate_gitignore_entry() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join(".gitignore"), ".trurl/.state/\n").unwrap();
        init(tmp.path()).unwrap();

        let content = fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
        assert_eq!(content.matches(".trurl/.state/").count(), 1);
    }
}
