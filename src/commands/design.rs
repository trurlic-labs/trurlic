use std::path::Path;

use crate::session::SessionMode;
use crate::store::{self};
use crate::{Error, Result};

use super::discover_store;

/// Validates the component exists before resolving provider configuration
/// so that a missing component never surfaces as a confusing API key error.
pub fn design(
    cwd: &Path,
    component: &str,
    mode: SessionMode,
    task: Option<&str>,
    provider_flag: Option<&str>,
    model_flag: Option<&str>,
) -> Result<()> {
    if component != "project" && !store::is_valid_kebab_case(component) {
        return Err(Error::InvalidName(component.into()));
    }

    let store = discover_store(cwd)?;

    // Fail fast: verify component exists before provider/API key resolution.
    if component != "project" {
        let names = store.list_components()?;
        if !names.iter().any(|n| n == component) {
            return Err(Error::ComponentNotFound(component.into()));
        }
    }

    let config = crate::config::resolve_provider(provider_flag, model_flag)?;
    let model = config.model.clone();
    let client = crate::provider::create_provider(config)?;
    eprintln!("Using {} ({})", client.provider_name(), model);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .map_err(|e| Error::Io(std::io::Error::other(e)))?;

    rt.block_on(crate::session::run_design(
        &store, &*client, component, mode, task,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::init;
    use tempfile::TempDir;

    #[test]
    fn design_rejects_invalid_component_name() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();

        let err = design(
            tmp.path(),
            "../escape",
            SessionMode::Fresh,
            None,
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, Error::InvalidName(_)));

        let err = design(tmp.path(), "", SessionMode::Fresh, None, None, None).unwrap_err();
        assert!(matches!(err, Error::InvalidName(_)));
    }

    #[test]
    fn design_rejects_nonexistent_component_before_provider() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();

        let err = design(tmp.path(), "ghost", SessionMode::Fresh, None, None, None).unwrap_err();
        assert!(matches!(err, Error::ComponentNotFound(ref n) if n == "ghost"));
    }
}
