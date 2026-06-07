use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::{Error, Result};

// ── Provider ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Anthropic,
    OpenAi,
    OpenRouter,
}

impl Provider {
    pub fn name(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
            Self::OpenRouter => "openrouter",
        }
    }

    pub fn env_var(self) -> &'static str {
        match self {
            Self::Anthropic => "ANTHROPIC_API_KEY",
            Self::OpenAi => "OPENAI_API_KEY",
            Self::OpenRouter => "OPENROUTER_API_KEY",
        }
    }

    fn default_model(self) -> &'static str {
        match self {
            Self::Anthropic => "claude-sonnet-4-20250514",
            Self::OpenAi => "gpt-4o",
            Self::OpenRouter => "anthropic/claude-sonnet-4-20250514",
        }
    }
}

const ALL_PROVIDERS: [Provider; 3] = [Provider::Anthropic, Provider::OpenAi, Provider::OpenRouter];

impl std::fmt::Display for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

fn parse_provider(name: &str) -> Result<Provider> {
    match name {
        "anthropic" | "claude" => Ok(Provider::Anthropic),
        "openai" | "gpt" => Ok(Provider::OpenAi),
        "openrouter" => Ok(Provider::OpenRouter),
        _ => Err(Error::ProviderConfig(format!(
            "unknown provider `{name}` — expected: anthropic, openai, openrouter"
        ))),
    }
}

// ── ApiKey ────────────────────────────────────────────────────────────────────

/// An API key that is zeroed from memory on drop.
/// `Display` and `Debug` show only a redacted form (last 4 chars).
/// Use [`expose`](ApiKey::expose) to access the raw value — only for
/// HTTP Authorization headers.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct ApiKey {
    inner: String,
}

impl ApiKey {
    pub fn new(key: String) -> Self {
        Self { inner: key }
    }

    /// Raw key value. **Only** for building HTTP Authorization headers.
    pub fn expose(&self) -> &str {
        &self.inner
    }

    /// Redacted form for diagnostics: `"…abcd"` (last 4 chars).
    /// Uses character boundaries (not byte offsets) so multi-byte
    /// suffixes never cause a panic.
    pub fn redacted(&self) -> String {
        match self.inner.char_indices().rev().nth(3) {
            Some((offset, _)) => format!("…{}", &self.inner[offset..]),
            None => "…****".into(),
        }
    }
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ApiKey({})", self.redacted())
    }
}

impl std::fmt::Display for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.redacted())
    }
}

// ── ProviderConfig ───────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct ProviderConfig {
    pub provider: Provider,
    pub key: ApiKey,
    pub model: String,
}

// ── ConfigFile (on-disk format) ──────────────────────────────────────────────

/// Deserialized `~/.config/trurl/config.toml`.
/// All fields optional — an empty file is valid.
/// ```toml
/// default_provider = "anthropic"
/// default_model = "claude-sonnet-4-20250514"
/// anthropic_api_key = "sk-ant-..."
/// openai_api_key = "sk-..."
/// openrouter_api_key = "sk-or-..."
/// ```
#[derive(Deserialize, Default, Zeroize, ZeroizeOnDrop)]
struct ConfigFile {
    #[zeroize(skip)]
    default_provider: Option<String>,
    #[zeroize(skip)]
    default_model: Option<String>,
    anthropic_api_key: Option<String>,
    openai_api_key: Option<String>,
    openrouter_api_key: Option<String>,
}

impl ConfigFile {
    fn key_for(&self, provider: Provider) -> Option<&str> {
        let val = match provider {
            Provider::Anthropic => self.anthropic_api_key.as_deref(),
            Provider::OpenAi => self.openai_api_key.as_deref(),
            Provider::OpenRouter => self.openrouter_api_key.as_deref(),
        };
        val.filter(|s| !s.is_empty())
    }
}

// ── EnvKeys ──────────────────────────────────────────────────────────────────

/// Snapshot of API key environment variables, zeroed on drop.
/// Read once at resolution time to decouple I/O from logic (testability).
#[derive(Zeroize, ZeroizeOnDrop)]
struct EnvKeys {
    anthropic: Option<String>,
    openai: Option<String>,
    openrouter: Option<String>,
}

impl EnvKeys {
    fn from_env() -> Self {
        let read = |var: &str| std::env::var(var).ok().filter(|s| !s.is_empty());
        Self {
            anthropic: read("ANTHROPIC_API_KEY"),
            openai: read("OPENAI_API_KEY"),
            openrouter: read("OPENROUTER_API_KEY"),
        }
    }

    fn get(&self, provider: Provider) -> Option<&str> {
        match provider {
            Provider::Anthropic => self.anthropic.as_deref(),
            Provider::OpenAi => self.openai.as_deref(),
            Provider::OpenRouter => self.openrouter.as_deref(),
        }
    }
}

// ── Public API ───────────────────────────────────────────────────────────────

pub fn resolve_provider(
    provider_flag: Option<&str>,
    model_flag: Option<&str>,
) -> Result<ProviderConfig> {
    let config = load_config_file()?;
    let env_keys = EnvKeys::from_env();
    resolve_from_sources(provider_flag, model_flag, &config, &env_keys)
}

pub fn config_file_path() -> Option<PathBuf> {
    config_base_dir().map(|d| d.join("trurl").join("config.toml"))
}

// ── Resolution logic (pure, testable) ────────────────────────────────────────

fn resolve_from_sources(
    provider_flag: Option<&str>,
    model_flag: Option<&str>,
    config: &Option<ConfigFile>,
    env_keys: &EnvKeys,
) -> Result<ProviderConfig> {
    let provider = resolve_provider_choice(provider_flag, config, env_keys)?;
    let key = resolve_key(provider, env_keys, config)?;

    let model = model_flag
        .map(String::from)
        .or_else(|| config.as_ref().and_then(|c| c.default_model.clone()))
        .unwrap_or_else(|| provider.default_model().into());

    Ok(ProviderConfig {
        provider,
        key,
        model,
    })
}

fn resolve_provider_choice(
    flag: Option<&str>,
    config: &Option<ConfigFile>,
    env_keys: &EnvKeys,
) -> Result<Provider> {
    // CLI flag wins
    if let Some(name) = flag {
        return parse_provider(name);
    }

    // Config default
    if let Some(cfg) = config
        && let Some(ref default) = cfg.default_provider
    {
        return parse_provider(default);
    }

    // Auto-detect: exactly one key must be available
    auto_detect_provider(config, env_keys)
}

fn auto_detect_provider(config: &Option<ConfigFile>, env_keys: &EnvKeys) -> Result<Provider> {
    let has_key = |p: Provider| -> bool {
        env_keys.get(p).is_some() || config.as_ref().and_then(|c| c.key_for(p)).is_some()
    };

    let found: Vec<Provider> = ALL_PROVIDERS
        .iter()
        .copied()
        .filter(|&p| has_key(p))
        .collect();

    match found.len() {
        0 => {
            let path_hint = config_file_path()
                .map(|p| format!(" or add keys to {}", p.display()))
                .unwrap_or_default();
            Err(Error::ProviderConfig(format!(
                "no API key found. Set ANTHROPIC_API_KEY, OPENAI_API_KEY, \
                 or OPENROUTER_API_KEY{path_hint}"
            )))
        }
        1 => Ok(found[0]),
        _ => {
            let names: Vec<&str> = found.iter().map(|p| p.name()).collect();
            Err(Error::ProviderConfig(format!(
                "multiple API keys found ({}) — specify provider with --provider",
                names.join(", ")
            )))
        }
    }
}

fn resolve_key(
    provider: Provider,
    env_keys: &EnvKeys,
    config: &Option<ConfigFile>,
) -> Result<ApiKey> {
    if let Some(val) = env_keys.get(provider) {
        return Ok(ApiKey::new(val.to_string()));
    }

    if let Some(cfg) = config
        && let Some(val) = cfg.key_for(provider)
    {
        return Ok(ApiKey::new(val.to_string()));
    }

    let path_hint = config_file_path()
        .map(|p| format!(" or add `{}_api_key` to {}", provider.name(), p.display()))
        .unwrap_or_default();

    Err(Error::ProviderConfig(format!(
        "no API key for {}. Set {}{path_hint}",
        provider.name(),
        provider.env_var(),
    )))
}

// ── Config file loading ──────────────────────────────────────────────────────

/// Load and parse the config file. Returns `None` if the file doesn't exist.
/// Enforces 0600 permissions on Unix. Zeros the raw TOML content after parsing.
fn load_config_file() -> Result<Option<ConfigFile>> {
    let path = match config_file_path() {
        Some(p) => p,
        None => return Ok(None),
    };

    if !path.exists() {
        return Ok(None);
    }

    #[cfg(unix)]
    check_config_permissions(&path)?;

    let mut content = fs::read_to_string(&path).map_err(|e| {
        Error::ProviderConfig(format!("cannot read config file {}: {e}", path.display()))
    })?;

    let result = toml::from_str::<ConfigFile>(&content);
    content.zeroize(); // Zero raw TOML before drop — may contain keys

    result
        .map(Some)
        .map_err(|e| Error::ProviderConfig(format!("invalid config file {}: {e}", path.display())))
}

// ── Platform-specific helpers ────────────────────────────────────────────────

#[cfg(unix)]
fn config_base_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
}

#[cfg(windows)]
fn config_base_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(PathBuf::from)
}

#[cfg(not(any(unix, windows)))]
fn config_base_dir() -> Option<PathBuf> {
    None
}

/// Verify config file has mode 0600 (owner read/write only).
/// Refuses group- or world-readable files to prevent key leakage via
/// shared accounts, accidental commits, or misconfigured mounts.
#[cfg(unix)]
fn check_config_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mode = fs::metadata(path)
        .map_err(|e| Error::ProviderConfig(format!("cannot stat {}: {e}", path.display())))?
        .permissions()
        .mode()
        & 0o777;

    if mode & 0o077 != 0 {
        return Err(Error::ProviderConfig(format!(
            "config file {} has mode {:04o} — must not be readable by group/world. \
             Fix with: chmod 600 {}",
            path.display(),
            mode,
            path.display(),
        )));
    }

    Ok(())
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── ApiKey ──────────────────────────────────────────────────────────

    #[test]
    fn api_key_redacts_to_last_four() {
        let key = ApiKey::new("sk-ant-api0xx-secret-key-1234".into());
        assert_eq!(key.redacted(), "…1234");
    }

    #[test]
    fn api_key_short_key_fully_redacted() {
        let key = ApiKey::new("abc".into());
        assert_eq!(key.redacted(), "…****");
    }

    #[test]
    fn api_key_never_leaks_in_display_or_debug() {
        let key = ApiKey::new("sk-ant-secret-key-1234".into());
        let debug = format!("{key:?}");
        let display = format!("{key}");
        assert!(!debug.contains("secret"), "Debug leaked key: {debug}");
        assert!(!display.contains("secret"), "Display leaked key: {display}");
        assert!(debug.contains("1234"));
        assert!(display.contains("1234"));
    }

    #[test]
    fn api_key_expose_returns_full_value() {
        let key = ApiKey::new("sk-ant-full-key".into());
        assert_eq!(key.expose(), "sk-ant-full-key");
    }

    #[test]
    fn api_key_redacted_handles_multibyte_utf8() {
        // A key ending with multi-byte characters must not panic.
        let key = ApiKey::new("sk-test-café".into());
        let redacted = key.redacted();
        assert!(redacted.starts_with('…'));
        // 'é' is 2 bytes — the old byte-slicing code would panic here.
        assert!(redacted.ends_with("afé"));
    }

    #[test]
    fn api_key_redacted_exact_four_chars() {
        let key = ApiKey::new("abcd".into());
        assert_eq!(key.redacted(), "…abcd");
    }

    // ── Provider ────────────────────────────────────────────────────────

    #[test]
    fn parse_provider_canonical_names() {
        assert_eq!(parse_provider("anthropic").unwrap(), Provider::Anthropic);
        assert_eq!(parse_provider("openai").unwrap(), Provider::OpenAi);
        assert_eq!(parse_provider("openrouter").unwrap(), Provider::OpenRouter);
    }

    #[test]
    fn parse_provider_aliases() {
        assert_eq!(parse_provider("claude").unwrap(), Provider::Anthropic);
        assert_eq!(parse_provider("gpt").unwrap(), Provider::OpenAi);
    }

    #[test]
    fn parse_provider_rejects_unknown() {
        let err = parse_provider("gemini").unwrap_err();
        match err {
            Error::ProviderConfig(msg) => assert!(msg.contains("gemini")),
            other => panic!("expected Validation, got: {other}"),
        }
    }

    #[test]
    fn provider_env_var_names() {
        assert_eq!(Provider::Anthropic.env_var(), "ANTHROPIC_API_KEY");
        assert_eq!(Provider::OpenAi.env_var(), "OPENAI_API_KEY");
        assert_eq!(Provider::OpenRouter.env_var(), "OPENROUTER_API_KEY");
    }

    // ── resolve_from_sources ─────────────────────────────────────────────

    fn env_with(
        anthropic: Option<&str>,
        openai: Option<&str>,
        openrouter: Option<&str>,
    ) -> EnvKeys {
        EnvKeys {
            anthropic: anthropic.map(String::from),
            openai: openai.map(String::from),
            openrouter: openrouter.map(String::from),
        }
    }

    fn env_anthropic() -> EnvKeys {
        env_with(Some("sk-ant-test-key-1234"), None, None)
    }

    fn config_with_key(provider: Provider, key: &str) -> Option<ConfigFile> {
        let mut cfg = ConfigFile::default();
        match provider {
            Provider::Anthropic => cfg.anthropic_api_key = Some(key.into()),
            Provider::OpenAi => cfg.openai_api_key = Some(key.into()),
            Provider::OpenRouter => cfg.openrouter_api_key = Some(key.into()),
        }
        Some(cfg)
    }

    fn config_with_defaults(provider: Option<&str>, model: Option<&str>) -> Option<ConfigFile> {
        let mut cfg = ConfigFile::default();
        cfg.default_provider = provider.map(String::from);
        cfg.default_model = model.map(String::from);
        Some(cfg)
    }

    #[test]
    fn resolve_env_key_with_explicit_provider() {
        let env = env_anthropic();
        let r = resolve_from_sources(Some("anthropic"), None, &None, &env).unwrap();
        assert_eq!(r.provider, Provider::Anthropic);
        assert_eq!(r.key.expose(), "sk-ant-test-key-1234");
    }

    #[test]
    fn resolve_auto_detects_single_provider() {
        let env = env_anthropic();
        let r = resolve_from_sources(None, None, &None, &env).unwrap();
        assert_eq!(r.provider, Provider::Anthropic);
    }

    #[test]
    fn resolve_env_overrides_config_key() {
        let env = env_anthropic();
        let config = config_with_key(Provider::Anthropic, "sk-ant-config-5678");
        let r = resolve_from_sources(Some("anthropic"), None, &config, &env).unwrap();
        assert_eq!(r.key.expose(), "sk-ant-test-key-1234");
    }

    #[test]
    fn resolve_falls_back_to_config_key() {
        let env = env_with(None, None, None);
        let config = config_with_key(Provider::Anthropic, "sk-ant-config-5678");
        let r = resolve_from_sources(Some("anthropic"), None, &config, &env).unwrap();
        assert_eq!(r.key.expose(), "sk-ant-config-5678");
    }

    #[test]
    fn resolve_config_default_provider() {
        let env = env_anthropic();
        let config = config_with_defaults(Some("anthropic"), None);
        let r = resolve_from_sources(None, None, &config, &env).unwrap();
        assert_eq!(r.provider, Provider::Anthropic);
    }

    #[test]
    fn resolve_provider_flag_overrides_config_default() {
        let env = env_with(Some("key-a"), Some("key-o"), None);
        let config = config_with_defaults(Some("anthropic"), None);
        let r = resolve_from_sources(Some("openai"), None, &config, &env).unwrap();
        assert_eq!(r.provider, Provider::OpenAi);
    }

    #[test]
    fn resolve_model_flag_overrides_all() {
        let env = env_anthropic();
        let config = config_with_defaults(None, Some("config-model"));
        let r = resolve_from_sources(Some("anthropic"), Some("flag-model"), &config, &env).unwrap();
        assert_eq!(r.model, "flag-model");
    }

    #[test]
    fn resolve_model_from_config() {
        let env = env_anthropic();
        let config = config_with_defaults(None, Some("custom-model"));
        let r = resolve_from_sources(Some("anthropic"), None, &config, &env).unwrap();
        assert_eq!(r.model, "custom-model");
    }

    #[test]
    fn resolve_model_default_per_provider() {
        let env = env_anthropic();
        let r = resolve_from_sources(Some("anthropic"), None, &None, &env).unwrap();
        assert_eq!(r.model, "claude-sonnet-4-20250514");

        let env = env_with(None, Some("key"), None);
        let r = resolve_from_sources(Some("openai"), None, &None, &env).unwrap();
        assert_eq!(r.model, "gpt-4o");
    }

    #[test]
    fn resolve_fails_no_keys() {
        let env = env_with(None, None, None);
        let err = resolve_from_sources(None, None, &None, &env).unwrap_err();
        match err {
            Error::ProviderConfig(msg) => assert!(msg.contains("no API key found")),
            other => panic!("expected Validation, got: {other}"),
        }
    }

    #[test]
    fn resolve_fails_multiple_keys_no_provider() {
        let env = env_with(Some("key-a"), Some("key-b"), None);
        let err = resolve_from_sources(None, None, &None, &env).unwrap_err();
        match err {
            Error::ProviderConfig(msg) => {
                assert!(msg.contains("multiple API keys"), "{msg}");
                assert!(msg.contains("--provider"), "{msg}");
            }
            other => panic!("expected Validation, got: {other}"),
        }
    }

    #[test]
    fn resolve_explicit_provider_picks_from_multiple() {
        let env = env_with(Some("key-a"), Some("key-b"), None);
        let r = resolve_from_sources(Some("openai"), None, &None, &env).unwrap();
        assert_eq!(r.provider, Provider::OpenAi);
        assert_eq!(r.key.expose(), "key-b");
    }

    #[test]
    fn resolve_fails_provider_without_key() {
        let env = env_anthropic();
        let err = resolve_from_sources(Some("openai"), None, &None, &env).unwrap_err();
        match err {
            Error::ProviderConfig(msg) => {
                assert!(msg.contains("openai"), "{msg}");
                assert!(msg.contains("OPENAI_API_KEY"), "{msg}");
            }
            other => panic!("expected Validation, got: {other}"),
        }
    }

    #[test]
    fn resolve_auto_detect_includes_config_keys() {
        let env = env_with(None, None, None);
        let config = config_with_key(Provider::OpenRouter, "sk-or-key");
        let r = resolve_from_sources(None, None, &config, &env).unwrap();
        assert_eq!(r.provider, Provider::OpenRouter);
    }

    // ── ConfigFile ──────────────────────────────────────────────────────

    #[test]
    fn config_file_parses_all_fields() {
        let toml = r#"
default_provider = "anthropic"
default_model = "claude-sonnet-4-20250514"
anthropic_api_key = "sk-ant-test"
openai_api_key = "sk-oai-test"
openrouter_api_key = "sk-or-test"
"#;
        let cfg: ConfigFile = toml::from_str(toml).unwrap();
        assert_eq!(cfg.default_provider.as_deref(), Some("anthropic"));
        assert_eq!(cfg.key_for(Provider::Anthropic), Some("sk-ant-test"));
        assert_eq!(cfg.key_for(Provider::OpenAi), Some("sk-oai-test"));
        assert_eq!(cfg.key_for(Provider::OpenRouter), Some("sk-or-test"));
    }

    #[test]
    fn config_file_empty_key_treated_as_absent() {
        let mut cfg = ConfigFile::default();
        cfg.anthropic_api_key = Some(String::new());
        assert!(cfg.key_for(Provider::Anthropic).is_none());
    }

    #[test]
    fn config_file_all_fields_optional() {
        let cfg: ConfigFile = toml::from_str("").unwrap();
        assert!(cfg.default_provider.is_none());
        assert!(cfg.key_for(Provider::Anthropic).is_none());
    }

    // ── Permissions (Unix only) ─────────────────────────────────────────

    #[cfg(unix)]
    #[test]
    fn permissions_rejects_group_readable() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o640)).unwrap();
        let err = check_config_permissions(tmp.path()).unwrap_err();
        match err {
            Error::ProviderConfig(msg) => {
                assert!(msg.contains("0640"), "should show actual mode: {msg}");
                assert!(msg.contains("chmod 600"), "should suggest fix: {msg}");
            }
            other => panic!("expected Validation, got: {other}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn permissions_rejects_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(check_config_permissions(tmp.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn permissions_accepts_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o600)).unwrap();
        check_config_permissions(tmp.path()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn permissions_accepts_owner_read_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o400)).unwrap();
        check_config_permissions(tmp.path()).unwrap();
    }
}
