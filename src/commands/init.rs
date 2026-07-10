use std::fs;
use std::io::Write;
use std::path::Path;

use chrono::Utc;

use crate::store::schema::{GraphIndex, NodeEntry, NodeKind, Project, ProjectFile};
use crate::store::{
    self, COMPONENTS_DIR, DECISIONS_DIR, FORMAT_VERSION, PATTERNS_DIR, STATE_DIR, STORE_DIR, Store,
};
use crate::{Error, Result};

/// Create a new `.trurlic/` directory in `cwd`.
pub fn init(cwd: &Path) -> Result<()> {
    let root = cwd.join(STORE_DIR);
    if root.exists() {
        return Err(Error::StoreExists(root));
    }

    fs::create_dir_all(root.join(COMPONENTS_DIR))?;
    fs::create_dir_all(root.join(DECISIONS_DIR))?;
    fs::create_dir_all(root.join(PATTERNS_DIR))?;
    fs::create_dir_all(root.join(STATE_DIR).join("tmp"))?;

    let name = cwd
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("my-project")
        .to_string();

    let project = ProjectFile {
        trurlic_version: FORMAT_VERSION.into(),
        project: Project {
            name,
            description: String::new(),
        },
    };

    let store = Store::at(root);
    let lock = store.lock()?;
    store.write_atomic(&lock, &store.root().join("project.toml"), &project)?;

    // Write initial graph.toml with the "project" virtual node.
    let project_hash = store::hash_file(&store.root().join("project.toml"))?;
    let index = GraphIndex {
        version: 1,
        rebuilt: Utc::now(),
        nodes: vec![NodeEntry {
            name: "project".into(),
            kind: NodeKind::Component,
            tags: vec![],
            hash: project_hash,
        }],
        edges: vec![],
    };
    store.write_atomic(&lock, &store.graph_path(), &index)?;

    drop(lock);

    append_gitignore(cwd)?;
    println!("Initialized .trurlic/");
    Ok(())
}

fn append_gitignore(cwd: &Path) -> Result<()> {
    let path = cwd.join(".gitignore");
    let entry = ".trurlic/.state/";

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
    use crate::store::schema::{GraphIndex, ProjectFile};
    use tempfile::TempDir;

    #[test]
    fn init_creates_directory_structure() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();

        let root = tmp.path().join(STORE_DIR);
        assert!(root.join("project.toml").is_file());
        assert!(root.join("graph.toml").is_file());
        assert!(root.join(COMPONENTS_DIR).is_dir());
        assert!(root.join(DECISIONS_DIR).is_dir());
        assert!(root.join(PATTERNS_DIR).is_dir());
        assert!(root.join(STATE_DIR).is_dir());
        assert!(!root.join(STATE_DIR).join("sessions").exists());
        assert!(root.join(STATE_DIR).join("tmp").is_dir());
    }

    #[test]
    fn init_writes_valid_project_toml() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();

        let content = fs::read_to_string(tmp.path().join(STORE_DIR).join("project.toml")).unwrap();
        let project: ProjectFile = toml::from_str(&content).unwrap();
        assert_eq!(project.trurlic_version, FORMAT_VERSION);
    }

    #[test]
    fn init_writes_valid_graph_toml() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();

        let content = fs::read_to_string(tmp.path().join(STORE_DIR).join("graph.toml")).unwrap();
        let index: GraphIndex = toml::from_str(&content).unwrap();
        assert_eq!(index.version, 1);
        assert_eq!(index.nodes.len(), 1);
        assert_eq!(index.nodes[0].name, "project");
        assert_eq!(index.nodes[0].kind, NodeKind::Component);
        assert!(index.edges.is_empty());
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
        assert!(content.contains(".trurlic/.state/"));
    }

    #[test]
    fn init_appends_to_existing_gitignore() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join(".gitignore"), "/target/\n").unwrap();
        init(tmp.path()).unwrap();

        let content = fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
        assert!(content.starts_with("/target/\n"));
        assert!(content.contains(".trurlic/.state/"));
    }

    #[test]
    fn init_appends_newline_before_entry_if_missing() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join(".gitignore"), "/target/").unwrap();
        init(tmp.path()).unwrap();

        let content = fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
        assert!(content.contains("/target/\n.trurlic/.state/\n"));
    }

    #[test]
    fn init_does_not_duplicate_gitignore_entry() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join(".gitignore"), ".trurlic/.state/\n").unwrap();
        init(tmp.path()).unwrap();

        let content = fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
        assert_eq!(content.matches(".trurlic/.state/").count(), 1);
    }
}
