use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::cli::InstallIde;
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DryRun {
    Yes,
    No,
}

pub fn install(ide: InstallIde, binary_path: Option<&Path>, dry_run: DryRun) -> Result<()> {
    let binary = resolve_binary(binary_path)?;

    if matches!(ide, InstallIde::ClaudeCode) {
        return install_claude_code(&binary, dry_run);
    }

    let path = ide_config_path(&ide)?;

    if dry_run == DryRun::Yes {
        let snippet = build_dry_run_snippet(&ide, &binary);
        println!("{snippet}");
        return Ok(());
    }

    match ide {
        InstallIde::Codex => write_toml_config(&path, &binary)?,
        InstallIde::HermesAgent => write_yaml_config(&path, &binary)?,
        InstallIde::OpenCode => {
            let entry = build_server_entry(&binary);
            write_json_opencode(&path, &entry)?;
        }
        InstallIde::Copilot => {
            let entry = build_server_entry(&binary);
            write_json_servers(&path, &entry)?;
        }
        InstallIde::ClaudeCode => {
            return install_claude_code(&binary, dry_run);
        }
        InstallIde::Claude
        | InstallIde::Cursor
        | InstallIde::Cline
        | InstallIde::Windsurf
        | InstallIde::OpenClaw
        | InstallIde::Antigravity => {
            let entry = build_server_entry(&binary);
            write_json_mcp_servers(&path, &entry)?;
        }
    }

    println!(
        "Installed trurlic MCP server for {}",
        ide_display_name(&ide)
    );
    println!("Config: {}", path.display());
    Ok(())
}

fn resolve_binary(explicit: Option<&Path>) -> Result<PathBuf> {
    let path = match explicit {
        Some(p) => p.to_path_buf(),
        None => std::env::current_exe().map_err(|_| Error::BinaryNotFound)?,
    };
    if !path.is_file() {
        return Err(Error::BinaryNotFound);
    }
    Ok(path)
}

fn build_server_entry(binary: &Path) -> Value {
    serde_json::json!({
        "command": binary.to_string_lossy(),
        "args": ["serve"]
    })
}

fn home_dir() -> Result<PathBuf> {
    #[cfg(unix)]
    {
        std::env::var("HOME")
            .map(PathBuf::from)
            .map_err(|_| Error::HomeNotFound)
    }
    #[cfg(windows)]
    {
        std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .map(PathBuf::from)
            .map_err(|_| Error::HomeNotFound)
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(Error::HomeNotFound)
    }
}

fn ide_config_path(ide: &InstallIde) -> Result<PathBuf> {
    let home = home_dir()?;
    let path = match ide {
        InstallIde::Claude => {
            if cfg!(target_os = "macos") {
                home.join("Library/Application Support/Claude/claude_desktop_config.json")
            } else if cfg!(target_os = "windows") {
                std::env::var("APPDATA")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| home.join("AppData/Roaming"))
                    .join("Claude/claude_desktop_config.json")
            } else {
                std::env::var("XDG_CONFIG_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| home.join(".config"))
                    .join("Claude/claude_desktop_config.json")
            }
        }
        InstallIde::ClaudeCode => home.join(".claude/settings.json"),
        InstallIde::Cursor => home.join(".cursor/mcp.json"),
        InstallIde::Cline => {
            if cfg!(target_os = "macos") {
                home.join("Library/Application Support/Code/User/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json")
            } else if cfg!(target_os = "windows") {
                std::env::var("APPDATA")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| home.join("AppData/Roaming"))
                    .join("Code/User/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json")
            } else {
                home.join(".config/Code/User/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json")
            }
        }
        InstallIde::Windsurf => home.join(".codeium/windsurf/mcp_config.json"),
        InstallIde::Copilot => {
            if cfg!(target_os = "macos") {
                home.join("Library/Application Support/Code/User/mcp.json")
            } else if cfg!(target_os = "windows") {
                std::env::var("APPDATA")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| home.join("AppData/Roaming"))
                    .join("Code/User/mcp.json")
            } else {
                std::env::var("XDG_CONFIG_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| home.join(".config"))
                    .join("Code/User/mcp.json")
            }
        }
        InstallIde::Codex => std::env::var("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home.join(".codex"))
            .join("config.toml"),
        InstallIde::OpenCode => std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home.join(".config"))
            .join("opencode/opencode.json"),
        InstallIde::OpenClaw => home.join(".openclaw/workspace/config/mcporter.json"),
        InstallIde::HermesAgent => home.join(".hermes/config.yaml"),
        InstallIde::Antigravity => home.join(".gemini/config/mcp_config.json"),
    };
    Ok(path)
}

fn ide_display_name(ide: &InstallIde) -> &'static str {
    match ide {
        InstallIde::Claude => "Claude Desktop",
        InstallIde::ClaudeCode => "Claude Code",
        InstallIde::Cursor => "Cursor",
        InstallIde::Cline => "Cline",
        InstallIde::Windsurf => "Windsurf",
        InstallIde::Copilot => "GitHub Copilot",
        InstallIde::Codex => "Codex CLI",
        InstallIde::OpenCode => "OpenCode",
        InstallIde::OpenClaw => "OpenClaw",
        InstallIde::HermesAgent => "Hermes Agent",
        InstallIde::Antigravity => "Antigravity",
    }
}

fn build_dry_run_snippet(ide: &InstallIde, binary: &Path) -> String {
    let bin = binary.to_string_lossy();
    match ide {
        InstallIde::ClaudeCode => {
            format!("claude mcp add trurlic -s user -- {bin} serve")
        }
        InstallIde::Codex => {
            format!("[mcp_servers.trurlic]\ncommand = \"{bin}\"\nargs = [\"serve\"]")
        }
        InstallIde::HermesAgent => {
            format!("mcp_servers:\n  trurlic:\n    command: '{bin}'\n    args:\n    - serve")
        }
        InstallIde::OpenCode => {
            let entry = serde_json::json!({
                "mcp": {
                    "trurlic": {
                        "type": "local",
                        "command": bin,
                        "args": ["serve"]
                    }
                }
            });
            serde_json::to_string_pretty(&entry).unwrap_or_default()
        }
        InstallIde::Copilot => {
            let entry = serde_json::json!({
                "servers": {
                    "trurlic": {
                        "command": bin,
                        "args": ["serve"]
                    }
                }
            });
            serde_json::to_string_pretty(&entry).unwrap_or_default()
        }
        InstallIde::Claude
        | InstallIde::Cursor
        | InstallIde::Cline
        | InstallIde::Windsurf
        | InstallIde::OpenClaw
        | InstallIde::Antigravity => {
            let entry = serde_json::json!({
                "mcpServers": {
                    "trurlic": {
                        "command": bin,
                        "args": ["serve"]
                    }
                }
            });
            serde_json::to_string_pretty(&entry).unwrap_or_default()
        }
    }
}

// ── Atomic file write ────────────────────────────────────────────────────────

fn atomic_write(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, content).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        Error::Io(e)
    })?;
    let readback = fs::read_to_string(&tmp).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        Error::Io(e)
    })?;
    if readback != content {
        let _ = fs::remove_file(&tmp);
        return Err(Error::InvalidInstallConfig {
            path: path.to_path_buf(),
            detail: "round-trip verification failed: written content differs".into(),
        });
    }
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        Error::Io(e)
    })
}

// ── JSON writers ─────────────────────────────────────────────────────────────

fn read_or_empty_json(path: &Path) -> Result<Value> {
    match fs::read_to_string(path) {
        Ok(s) if s.trim().is_empty() => Ok(Value::Object(serde_json::Map::new())),
        Ok(s) => serde_json::from_str(&s).map_err(|e| Error::InvalidInstallConfig {
            path: path.to_path_buf(),
            detail: e.to_string(),
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(Value::Object(serde_json::Map::new()))
        }
        Err(e) => Err(Error::Io(e)),
    }
}

fn write_json_with_key(path: &Path, key: &str, entry: &Value) -> Result<()> {
    let mut root = read_or_empty_json(path)?;
    let obj = root
        .as_object_mut()
        .ok_or_else(|| Error::InvalidInstallStructure {
            path: path.to_path_buf(),
        })?;

    let servers = obj
        .entry(key)
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let servers_obj = servers
        .as_object_mut()
        .ok_or_else(|| Error::InvalidInstallStructure {
            path: path.to_path_buf(),
        })?;

    if servers_obj.contains_key("trurlic") {
        eprintln!(
            "warning: overwriting existing \"trurlic\" entry in {}",
            path.display()
        );
    }
    servers_obj.insert("trurlic".into(), entry.clone());

    let out = serde_json::to_string_pretty(&root).map_err(|e| Error::InvalidInstallConfig {
        path: path.to_path_buf(),
        detail: e.to_string(),
    })?;
    atomic_write(path, &format!("{out}\n"))
}

fn write_json_mcp_servers(path: &Path, entry: &Value) -> Result<()> {
    write_json_with_key(path, "mcpServers", entry)
}

fn write_json_servers(path: &Path, entry: &Value) -> Result<()> {
    write_json_with_key(path, "servers", entry)
}

fn write_json_opencode(path: &Path, entry: &Value) -> Result<()> {
    let mut oc_entry = entry.clone();
    if let Some(obj) = oc_entry.as_object_mut() {
        obj.insert("type".into(), Value::String("local".into()));
    }
    write_json_with_key(path, "mcp", &oc_entry)
}

// ── TOML writer ──────────────────────────────────────────────────────────────

fn write_toml_config(path: &Path, binary: &Path) -> Result<()> {
    let content = match fs::read_to_string(path) {
        Ok(s) if s.trim().is_empty() => String::new(),
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(Error::Io(e)),
    };

    let mut table: toml::Value =
        toml::from_str(&content).map_err(|e| Error::InvalidInstallToml {
            path: path.to_path_buf(),
            detail: e.to_string(),
        })?;

    let root = table
        .as_table_mut()
        .ok_or_else(|| Error::InvalidInstallStructure {
            path: path.to_path_buf(),
        })?;

    let servers = root
        .entry("mcp_servers")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    let servers_table = servers
        .as_table_mut()
        .ok_or_else(|| Error::InvalidInstallStructure {
            path: path.to_path_buf(),
        })?;

    let mut trurlic = toml::map::Map::new();
    trurlic.insert(
        "command".into(),
        toml::Value::String(binary.to_string_lossy().into_owned()),
    );
    trurlic.insert(
        "args".into(),
        toml::Value::Array(vec![toml::Value::String("serve".into())]),
    );

    if servers_table.contains_key("trurlic") {
        eprintln!(
            "warning: overwriting existing \"trurlic\" entry in {}",
            path.display()
        );
    }
    servers_table.insert("trurlic".into(), toml::Value::Table(trurlic));

    let out = toml::to_string_pretty(&table).map_err(|e| Error::InvalidInstallToml {
        path: path.to_path_buf(),
        detail: e.to_string(),
    })?;
    atomic_write(path, &out)
}

// ── YAML writer ──────────────────────────────────────────────────────────────

fn write_yaml_config(path: &Path, binary: &Path) -> Result<()> {
    let content = match fs::read_to_string(path) {
        Ok(s) if s.trim().is_empty() => String::new(),
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(Error::Io(e)),
    };

    let mut root: serde_yaml_ng::Value = if content.is_empty() {
        serde_yaml_ng::Value::Mapping(serde_yaml_ng::Mapping::new())
    } else {
        serde_yaml_ng::from_str(&content).map_err(|e| Error::InvalidInstallYaml {
            path: path.to_path_buf(),
            detail: e.to_string(),
        })?
    };

    let root_map = root
        .as_mapping_mut()
        .ok_or_else(|| Error::InvalidInstallStructure {
            path: path.to_path_buf(),
        })?;

    let servers_key = serde_yaml_ng::Value::String("mcp_servers".into());
    if !root_map.contains_key(&servers_key) {
        root_map.insert(
            servers_key.clone(),
            serde_yaml_ng::Value::Mapping(serde_yaml_ng::Mapping::new()),
        );
    }
    let servers = root_map
        .get_mut(&servers_key)
        .and_then(|v| v.as_mapping_mut())
        .ok_or_else(|| Error::InvalidInstallStructure {
            path: path.to_path_buf(),
        })?;

    let trurlic_key = serde_yaml_ng::Value::String("trurlic".into());
    if servers.contains_key(&trurlic_key) {
        eprintln!(
            "warning: overwriting existing \"trurlic\" entry in {}",
            path.display()
        );
    }

    let mut entry = serde_yaml_ng::Mapping::new();
    entry.insert(
        serde_yaml_ng::Value::String("command".into()),
        serde_yaml_ng::Value::String(binary.to_string_lossy().into_owned()),
    );
    entry.insert(
        serde_yaml_ng::Value::String("args".into()),
        serde_yaml_ng::Value::Sequence(vec![serde_yaml_ng::Value::String("serve".into())]),
    );
    servers.insert(trurlic_key, serde_yaml_ng::Value::Mapping(entry));

    let out = serde_yaml_ng::to_string(&root).map_err(|e| Error::InvalidInstallYaml {
        path: path.to_path_buf(),
        detail: e.to_string(),
    })?;
    atomic_write(path, &out)
}

// ── Claude Code handler ──────────────────────────────────────────────────────

fn find_claude_cli() -> Result<PathBuf> {
    if let Ok(p) = which("claude") {
        return Ok(p);
    }
    let home = home_dir()?;
    let candidates = [
        PathBuf::from("/usr/local/bin/claude"),
        home.join(".claude/bin/claude"),
    ];
    for c in &candidates {
        if c.is_file() {
            return Ok(c.clone());
        }
    }
    Err(Error::ClaudeCliNotFound)
}

fn which(name: &str) -> std::result::Result<PathBuf, ()> {
    let path_var = std::env::var("PATH").map_err(|_| ())?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(())
}

fn install_claude_code(binary: &Path, dry_run: DryRun) -> Result<()> {
    let claude = find_claude_cli()?;
    let bin_str = binary.to_string_lossy();

    if dry_run == DryRun::Yes {
        println!("claude mcp add trurlic -s user -- {bin_str} serve");
        return Ok(());
    }

    // Remove existing entry (ignore errors).
    let _ = Command::new(&claude)
        .args(["mcp", "remove", "trurlic", "-s", "user"])
        .output();

    let output = Command::new(&claude)
        .args([
            "mcp", "add", "trurlic", "-s", "user", "--", &bin_str, "serve",
        ])
        .output()
        .map_err(|e| Error::ClaudeCliExec(e.to_string()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::ClaudeCliExec(stderr.trim().to_string()));
    }

    println!("Installed trurlic MCP server for Claude Code");
    Ok(())
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn build_server_entry_structure() {
        let entry = build_server_entry(Path::new("/usr/bin/trurlic"));
        assert_eq!(entry["command"], "/usr/bin/trurlic");
        assert_eq!(entry["args"], serde_json::json!(["serve"]));
    }

    // ── JSON mcpServers writer ───────────────────────────────────────────────

    #[test]
    fn write_json_creates_new_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.json");
        let entry = build_server_entry(Path::new("/bin/trurlic"));

        write_json_mcp_servers(&path, &entry).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(content["mcpServers"]["trurlic"]["command"], "/bin/trurlic");
        assert_eq!(
            content["mcpServers"]["trurlic"]["args"],
            serde_json::json!(["serve"])
        );
    }

    #[test]
    fn write_json_preserves_other_servers() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.json");
        fs::write(&path, r#"{"mcpServers":{"other":{"command":"other-cmd"}}}"#).unwrap();

        let entry = build_server_entry(Path::new("/bin/trurlic"));
        write_json_mcp_servers(&path, &entry).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(content["mcpServers"]["other"]["command"], "other-cmd");
        assert_eq!(content["mcpServers"]["trurlic"]["command"], "/bin/trurlic");
    }

    #[test]
    fn write_json_preserves_non_mcp_keys() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.json");
        fs::write(&path, r#"{"theme":"dark","mcpServers":{}}"#).unwrap();

        let entry = build_server_entry(Path::new("/bin/trurlic"));
        write_json_mcp_servers(&path, &entry).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(content["theme"], "dark");
        assert!(content["mcpServers"]["trurlic"].is_object());
    }

    #[test]
    fn write_json_overwrites_existing_trurlic() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.json");
        fs::write(&path, r#"{"mcpServers":{"trurlic":{"command":"old"}}}"#).unwrap();

        let entry = build_server_entry(Path::new("/bin/trurlic-new"));
        write_json_mcp_servers(&path, &entry).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            content["mcpServers"]["trurlic"]["command"],
            "/bin/trurlic-new"
        );
    }

    #[test]
    fn write_json_handles_empty_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.json");
        fs::write(&path, "").unwrap();

        let entry = build_server_entry(Path::new("/bin/trurlic"));
        write_json_mcp_servers(&path, &entry).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(content["mcpServers"]["trurlic"].is_object());
    }

    #[test]
    fn write_json_creates_mcp_servers_if_missing() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.json");
        fs::write(&path, r#"{"theme":"dark"}"#).unwrap();

        let entry = build_server_entry(Path::new("/bin/trurlic"));
        write_json_mcp_servers(&path, &entry).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(content["theme"], "dark");
        assert!(content["mcpServers"]["trurlic"].is_object());
    }

    #[test]
    fn write_json_rejects_invalid_json() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.json");
        fs::write(&path, "not json").unwrap();

        let entry = build_server_entry(Path::new("/bin/trurlic"));
        let err = write_json_mcp_servers(&path, &entry).unwrap_err();
        assert!(matches!(err, Error::InvalidInstallConfig { .. }));
    }

    #[test]
    fn write_json_rejects_non_object_root() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.json");
        fs::write(&path, "[1,2,3]").unwrap();

        let entry = build_server_entry(Path::new("/bin/trurlic"));
        let err = write_json_mcp_servers(&path, &entry).unwrap_err();
        assert!(matches!(err, Error::InvalidInstallStructure { .. }));
    }

    // ── JSON servers writer (Copilot) ────────────────────────────────────────

    #[test]
    fn write_copilot_uses_servers_key() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("mcp.json");

        let entry = build_server_entry(Path::new("/bin/trurlic"));
        write_json_servers(&path, &entry).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(content["servers"]["trurlic"].is_object());
        assert!(content.get("mcpServers").is_none());
    }

    #[test]
    fn write_copilot_preserves_other_entries() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("mcp.json");
        fs::write(&path, r#"{"servers":{"other":{"command":"x"}}}"#).unwrap();

        let entry = build_server_entry(Path::new("/bin/trurlic"));
        write_json_servers(&path, &entry).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(content["servers"]["other"]["command"], "x");
        assert!(content["servers"]["trurlic"].is_object());
    }

    // ── JSON OpenCode writer ─────────────────────────────────────────────────

    #[test]
    fn write_opencode_uses_mcp_key() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("opencode.json");

        let entry = build_server_entry(Path::new("/bin/trurlic"));
        write_json_opencode(&path, &entry).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(content["mcp"]["trurlic"].is_object());
    }

    #[test]
    fn write_opencode_adds_type_local() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("opencode.json");

        let entry = build_server_entry(Path::new("/bin/trurlic"));
        write_json_opencode(&path, &entry).unwrap();

        let content: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(content["mcp"]["trurlic"]["type"], "local");
        assert_eq!(content["mcp"]["trurlic"]["command"], "/bin/trurlic");
    }

    // ── TOML writer (Codex) ──────────────────────────────────────────────────

    #[test]
    fn write_toml_creates_new_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");

        write_toml_config(&path, Path::new("/bin/trurlic")).unwrap();

        let content: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            content["mcp_servers"]["trurlic"]["command"].as_str(),
            Some("/bin/trurlic")
        );
        assert_eq!(
            content["mcp_servers"]["trurlic"]["args"]
                .as_array()
                .unwrap(),
            &[toml::Value::String("serve".into())]
        );
    }

    #[test]
    fn write_toml_preserves_existing_servers() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        fs::write(&path, "[mcp_servers.other]\ncommand = \"other-cmd\"\n").unwrap();

        write_toml_config(&path, Path::new("/bin/trurlic")).unwrap();

        let content: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            content["mcp_servers"]["other"]["command"].as_str(),
            Some("other-cmd")
        );
        assert_eq!(
            content["mcp_servers"]["trurlic"]["command"].as_str(),
            Some("/bin/trurlic")
        );
    }

    #[test]
    fn write_toml_preserves_other_keys() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        fs::write(&path, "model = \"o3\"\n").unwrap();

        write_toml_config(&path, Path::new("/bin/trurlic")).unwrap();

        let content: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(content["model"].as_str(), Some("o3"));
        assert!(content["mcp_servers"]["trurlic"].is_table());
    }

    #[test]
    fn write_toml_rejects_invalid_toml() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        fs::write(&path, "not valid toml [[[").unwrap();

        let err = write_toml_config(&path, Path::new("/bin/trurlic")).unwrap_err();
        assert!(matches!(err, Error::InvalidInstallToml { .. }));
    }

    // ── YAML writer (Hermes) ─────────────────────────────────────────────────

    #[test]
    fn write_yaml_creates_new_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.yaml");

        write_yaml_config(&path, Path::new("/bin/trurlic")).unwrap();

        let content: serde_yaml_ng::Value =
            serde_yaml_ng::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let servers = content["mcp_servers"].as_mapping().unwrap();
        let trurlic = servers
            .get(serde_yaml_ng::Value::String("trurlic".into()))
            .unwrap();
        assert_eq!(
            trurlic["command"],
            serde_yaml_ng::Value::String("/bin/trurlic".into())
        );
    }

    #[test]
    fn write_yaml_preserves_existing_servers() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.yaml");
        fs::write(&path, "mcp_servers:\n  other:\n    command: other-cmd\n").unwrap();

        write_yaml_config(&path, Path::new("/bin/trurlic")).unwrap();

        let content: serde_yaml_ng::Value =
            serde_yaml_ng::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let servers = content["mcp_servers"].as_mapping().unwrap();
        let other = servers
            .get(serde_yaml_ng::Value::String("other".into()))
            .unwrap();
        assert_eq!(
            other["command"],
            serde_yaml_ng::Value::String("other-cmd".into())
        );
        assert!(servers.contains_key(serde_yaml_ng::Value::String("trurlic".into())));
    }

    #[test]
    fn write_yaml_preserves_other_keys() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.yaml");
        fs::write(&path, "model: o3\n").unwrap();

        write_yaml_config(&path, Path::new("/bin/trurlic")).unwrap();

        let content: serde_yaml_ng::Value =
            serde_yaml_ng::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(content["model"], serde_yaml_ng::Value::String("o3".into()));
        assert!(content["mcp_servers"].as_mapping().is_some());
    }

    #[test]
    fn write_yaml_rejects_invalid_yaml() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.yaml");
        fs::write(&path, ":\n  - :\n    bad: [").unwrap();

        let err = write_yaml_config(&path, Path::new("/bin/trurlic")).unwrap_err();
        assert!(matches!(err, Error::InvalidInstallYaml { .. }));
    }

    // ── Config paths ─────────────────────────────────────────────────────────

    #[test]
    fn ide_config_path_cursor() {
        let path = ide_config_path(&InstallIde::Cursor).unwrap();
        assert!(path.ends_with(".cursor/mcp.json"));
    }

    #[test]
    fn ide_config_path_codex() {
        let path = ide_config_path(&InstallIde::Codex).unwrap();
        assert!(path.ends_with("config.toml"));
        let parent = path
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap();
        // Either ~/.codex/ (default) or $CODEX_HOME/.
        assert!(parent == ".codex" || std::env::var("CODEX_HOME").is_ok());
    }

    #[test]
    fn resolve_binary_rejects_nonexistent() {
        let err = resolve_binary(Some(Path::new("/nonexistent/trurlic"))).unwrap_err();
        assert!(matches!(err, Error::BinaryNotFound));
    }

    #[test]
    fn resolve_binary_accepts_real_file() {
        let tmp = TempDir::new().unwrap();
        let bin = tmp.path().join("trurlic");
        fs::write(&bin, "fake binary").unwrap();
        let result = resolve_binary(Some(&bin)).unwrap();
        assert_eq!(result, bin);
    }
}
