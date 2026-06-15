//! Source file context assembly for the CLI bootstrap driver.
//!
//! Walks the project directory tree, reads source files within budget
//! limits, and formats them for LLM consumption. All I/O is bounded:
//! per-file and total size caps prevent OOM on oversized projects.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use crate::Result;

// ── Limits ──────────────────────────────────────────────────────────────────

/// Maximum bytes per source file. Files beyond this are listed but not
/// included. 64 KB covers virtually all single-module source files.
const MAX_FILE_BYTES: usize = 65_536;

/// Maximum total bytes of assembled context. Prevents context window
/// overflow for large projects. 256 KB ≈ ~65K tokens at ~4 chars/token.
const MAX_CONTEXT_BYTES: usize = 262_144;

/// Maximum directory tree depth to prevent runaway traversal.
const MAX_DEPTH: usize = 12;

// ── Skip lists ──────────────────────────────────────────────────────────────

/// Directories skipped unconditionally during traversal.
const SKIP_DIRS: &[&str] = &[
    ".git",
    ".trurlic",
    ".hg",
    ".svn",
    "node_modules",
    "target",
    "__pycache__",
    ".venv",
    "venv",
    "vendor",
    "dist",
    "build",
    ".next",
    ".cache",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
];

/// Source file extensions included in context. Lowercase, without the dot.
const SOURCE_EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "py", "go", "java", "rb", "c", "cpp", "cc", "h", "hpp", "cs",
    "swift", "kt", "scala", "clj", "cljc", "ex", "exs", "ml", "mli", "hs", "lua", "sh", "bash",
    "zsh", "sql", "proto", "graphql", "tf", "nix",
];

/// Config/build files included by exact name.
const CONFIG_FILES: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "package.json",
    "tsconfig.json",
    "pyproject.toml",
    "setup.py",
    "setup.cfg",
    "go.mod",
    "go.sum",
    "build.gradle",
    "pom.xml",
    "Makefile",
    "CMakeLists.txt",
    "Dockerfile",
    "docker-compose.yml",
    "docker-compose.yaml",
    ".github/workflows/ci.yml",
    ".github/workflows/ci.yaml",
    "deny.toml",
    "rustfmt.toml",
    "rust-toolchain.toml",
    ".eslintrc.json",
    ".prettierrc",
];

// ── Public API ──────────────────────────────────────────────────────────────

/// Assemble a directory tree listing plus config file contents.
///
/// Used as context for the `scan_project` step. Includes the directory
/// tree (all source files listed, not read) and the contents of build
/// configuration files.
pub(crate) fn gather_tree(project_root: &Path) -> Result<String> {
    let mut out = String::with_capacity(16_384);
    out.push_str("PROJECT STRUCTURE:\n\n");

    let mut tree = Vec::new();
    collect_tree(project_root, 0, &mut tree)?;
    for entry in &tree {
        out.push_str(entry);
        out.push('\n');
    }

    out.push('\n');
    let budget = MAX_CONTEXT_BYTES.saturating_sub(out.len());
    append_config_files(&mut out, project_root, budget)?;

    Ok(out)
}

/// Assemble source file contents for a single component.
///
/// Used as context for the `extract_decisions` step. Tries
/// component-specific directories first (`src/{name}/`), falls back to
/// all source files under `src/` when no match is found.
pub(crate) fn gather_sources(project_root: &Path, component: &str) -> Result<String> {
    let mut out = String::with_capacity(32_768);

    // Try component-specific paths.
    let candidates = component_paths(project_root, component);
    let mut matched = false;

    for path in &candidates {
        if path.is_dir() {
            let _ = write!(
                out,
                "SOURCE CODE FOR [{component}] ({}):\n\n",
                path.strip_prefix(project_root).unwrap_or(path).display()
            );
            let budget = MAX_CONTEXT_BYTES.saturating_sub(out.len());
            append_source_files(&mut out, path, budget)?;
            matched = true;
            break;
        } else if path.is_file() {
            let _ = write!(out, "SOURCE CODE FOR [{component}]:\n\n");
            let budget = MAX_CONTEXT_BYTES.saturating_sub(out.len());
            append_single_file(&mut out, project_root, path, budget)?;
            matched = true;
            break;
        }
    }

    if !matched {
        // Fallback: include all source files under src/.
        out.push_str("SOURCE CODE (full project — no component directory found):\n\n");
        let src_dir = project_root.join("src");
        if src_dir.is_dir() {
            let budget = MAX_CONTEXT_BYTES.saturating_sub(out.len());
            append_source_files(&mut out, &src_dir, budget)?;
        }
    }

    Ok(out)
}

/// Assemble config and top-level files for project-rule extraction.
///
/// Used as context for the `project_rules` step.
pub(crate) fn gather_project_config(project_root: &Path) -> Result<String> {
    let mut out = String::with_capacity(16_384);
    out.push_str("PROJECT CONFIGURATION:\n\n");

    let budget = MAX_CONTEXT_BYTES.saturating_sub(out.len());
    append_config_files(&mut out, project_root, budget)?;

    // Also include top-level source entry points.
    for name in &[
        "src/main.rs",
        "src/lib.rs",
        "src/mod.rs",
        "src/index.ts",
        "src/index.js",
        "src/app.py",
    ] {
        let path = project_root.join(name);
        if path.is_file() {
            let remaining = MAX_CONTEXT_BYTES.saturating_sub(out.len());
            append_single_file(&mut out, project_root, &path, remaining)?;
        }
    }

    Ok(out)
}

// ── Directory tree ──────────────────────────────────────────────────────────

fn collect_tree(dir: &Path, depth: usize, entries: &mut Vec<String>) -> Result<()> {
    if depth > MAX_DEPTH {
        return Ok(());
    }

    let mut children: Vec<PathBuf> = match fs::read_dir(dir) {
        Ok(rd) => rd.filter_map(|e| e.ok().map(|e| e.path())).collect(),
        Err(e) => {
            eprintln!("warning: cannot read directory {}: {e}", dir.display());
            return Ok(());
        }
    };
    children.sort();

    let indent = "  ".repeat(depth);

    for child in &children {
        let name = match child.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };

        if child.is_dir() {
            if SKIP_DIRS.contains(&name) {
                continue;
            }
            if let Some(last) = entries.last_mut() {
                let _ = writeln!(last);
            }
            entries.push(format!("{indent}{name}/"));
            collect_tree(child, depth + 1, entries)?;
        } else if is_source_file(name) || is_config_name(name) {
            entries.push(format!("{indent}{name}"));
        }
    }

    Ok(())
}

// ── Source file assembly ────────────────────────────────────────────────────

fn append_source_files(out: &mut String, dir: &Path, mut budget: usize) -> Result<()> {
    let files = collect_source_files(dir)?;
    for path in &files {
        if budget == 0 {
            out.push_str("(context budget reached — remaining files omitted)\n");
            break;
        }
        let consumed = append_single_file(out, dir, path, budget)?;
        budget = budget.saturating_sub(consumed);
    }
    Ok(())
}

fn append_single_file(out: &mut String, root: &Path, path: &Path, budget: usize) -> Result<usize> {
    let rel = path.strip_prefix(root).unwrap_or(path);
    let meta = fs::symlink_metadata(path)?;
    let size = usize::try_from(meta.len()).unwrap_or(usize::MAX);

    if size > MAX_FILE_BYTES {
        let _ = writeln!(
            out,
            "--- {} --- (skipped: {} bytes > limit)",
            rel.display(),
            size
        );
        return Ok(0);
    }
    if size > budget {
        let _ = writeln!(out, "--- {} --- (skipped: budget exhausted)", rel.display());
        return Ok(0);
    }

    let content = match fs::read(path) {
        Ok(bytes) => {
            if is_binary(&bytes) {
                let _ = writeln!(out, "--- {} --- (skipped: binary)", rel.display());
                return Ok(0);
            }
            String::from_utf8_lossy(&bytes).into_owned()
        }
        Err(e) => {
            eprintln!("warning: cannot read file {}: {e}", path.display());
            return Ok(0);
        }
    };

    let _ = writeln!(out, "--- {} ---", rel.display());
    out.push_str(&content);
    if !content.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');

    Ok(content.len())
}

fn collect_source_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = BTreeSet::new();
    collect_source_files_recursive(dir, 0, &mut files)?;
    Ok(files.into_iter().collect())
}

fn collect_source_files_recursive(
    dir: &Path,
    depth: usize,
    files: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    if depth > MAX_DEPTH {
        return Ok(());
    }
    let entries = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return Ok(()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };

        if path.is_dir() {
            if !SKIP_DIRS.contains(&name.as_str()) {
                collect_source_files_recursive(&path, depth + 1, files)?;
            }
        } else if is_source_file(&name) {
            files.insert(path);
        }
    }
    Ok(())
}

// ── Config files ────────────────────────────────────────────────────────────

fn append_config_files(out: &mut String, project_root: &Path, mut budget: usize) -> Result<()> {
    for name in CONFIG_FILES {
        if budget == 0 {
            break;
        }
        let path = project_root.join(name);
        if path.is_file() {
            let consumed = append_single_file(out, project_root, &path, budget)?;
            budget = budget.saturating_sub(consumed);
        }
    }
    Ok(())
}

// ── Classification helpers ──────────────────────────────────────────────────

fn is_source_file(name: &str) -> bool {
    let ext = match name.rsplit('.').next() {
        Some(e) if e != name => e,
        _ => return false,
    };
    SOURCE_EXTENSIONS
        .iter()
        .any(|s| s.eq_ignore_ascii_case(ext))
}

fn is_config_name(name: &str) -> bool {
    CONFIG_FILES.iter().any(|c| {
        // Match just the filename portion (not paths like .github/workflows/ci.yml).
        c.rsplit('/').next().is_some_and(|base| base == name)
    })
}

/// Binary detection: a file is binary if its first 512 bytes contain a
/// null byte. This heuristic matches Git's approach and catches compiled
/// objects, images, and archives without reading the full file.
fn is_binary(bytes: &[u8]) -> bool {
    let check = &bytes[..bytes.len().min(512)];
    check.contains(&0)
}

/// Candidate file/directory paths for a component name.
///
/// Returns paths in priority order: exact directory match first, then
/// single-file module, then alternative locations.
fn component_paths(project_root: &Path, component: &str) -> Vec<PathBuf> {
    let src = project_root.join("src");
    vec![
        src.join(component),
        src.join(format!("{component}.rs")),
        src.join("commands").join(format!("{component}.rs")),
        project_root.join(component),
        project_root.join("lib").join(component),
        project_root.join("packages").join(component),
    ]
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup_project(tmp: &Path) {
        let src = tmp.join("src");
        fs::create_dir_all(src.join("auth")).unwrap();
        fs::create_dir_all(src.join("store")).unwrap();
        fs::write(src.join("main.rs"), "fn main() {}\n").unwrap();
        fs::write(src.join("lib.rs"), "pub mod auth;\npub mod store;\n").unwrap();
        fs::write(src.join("auth").join("mod.rs"), "pub fn login() {}\n").unwrap();
        fs::write(src.join("auth").join("tokens.rs"), "pub struct Token;\n").unwrap();
        fs::write(src.join("store").join("mod.rs"), "pub fn save() {}\n").unwrap();
        fs::write(tmp.join("Cargo.toml"), "[package]\nname = \"test\"\n").unwrap();
        // Create a directory that should be skipped.
        fs::create_dir_all(tmp.join("target").join("debug")).unwrap();
        fs::write(tmp.join("target").join("debug").join("test"), "binary").unwrap();
    }

    #[test]
    fn gather_tree_includes_source_files() {
        let tmp = TempDir::new().unwrap();
        setup_project(tmp.path());
        let tree = gather_tree(tmp.path()).unwrap();

        assert!(tree.contains("main.rs"), "should list main.rs");
        assert!(tree.contains("auth/"), "should list auth directory");
        assert!(tree.contains("mod.rs"), "should list mod.rs");
        assert!(!tree.contains("target"), "should skip target/");
    }

    #[test]
    fn gather_tree_includes_cargo_toml() {
        let tmp = TempDir::new().unwrap();
        setup_project(tmp.path());
        let tree = gather_tree(tmp.path()).unwrap();

        assert!(tree.contains("Cargo.toml"), "should include config content");
        assert!(tree.contains("[package]"), "should include file content");
    }

    #[test]
    fn gather_sources_finds_component_dir() {
        let tmp = TempDir::new().unwrap();
        setup_project(tmp.path());
        let ctx = gather_sources(tmp.path(), "auth").unwrap();

        assert!(ctx.contains("[auth]"), "should mention component name");
        assert!(
            ctx.contains("pub fn login"),
            "should include source content"
        );
        assert!(ctx.contains("tokens.rs"), "should include submodule");
    }

    #[test]
    fn gather_sources_falls_back_to_all_sources() {
        let tmp = TempDir::new().unwrap();
        setup_project(tmp.path());
        let ctx = gather_sources(tmp.path(), "nonexistent").unwrap();

        assert!(ctx.contains("no component directory found"));
        assert!(ctx.contains("main.rs"), "should include fallback sources");
    }

    #[test]
    fn gather_project_config_includes_build_files() {
        let tmp = TempDir::new().unwrap();
        setup_project(tmp.path());
        let ctx = gather_project_config(tmp.path()).unwrap();

        assert!(ctx.contains("Cargo.toml"));
        assert!(ctx.contains("[package]"));
        assert!(ctx.contains("lib.rs"), "should include entry points");
    }

    #[test]
    fn is_source_file_classification() {
        assert!(is_source_file("main.rs"));
        assert!(is_source_file("index.ts"));
        assert!(is_source_file("app.py"));
        assert!(!is_source_file("image.png"));
        assert!(!is_source_file("data.bin"));
        assert!(!is_source_file("noext"));
    }

    #[test]
    fn binary_detection() {
        assert!(is_binary(&[0x7f, 0x45, 0x4c, 0x46, 0x00])); // ELF header
        assert!(!is_binary(b"fn main() {}\n"));
        assert!(!is_binary(b"")); // empty is not binary
    }

    #[test]
    fn skips_oversized_files() {
        let tmp = TempDir::new().unwrap();
        let big = tmp.path().join("big.rs");
        fs::write(&big, "x".repeat(MAX_FILE_BYTES + 1)).unwrap();

        let mut out = String::new();
        append_single_file(&mut out, tmp.path(), &big, MAX_CONTEXT_BYTES).unwrap();

        assert!(out.contains("skipped"), "oversized file should be skipped");
        assert!(!out.contains("xxxxxxx"), "content should not be included");
    }
}
