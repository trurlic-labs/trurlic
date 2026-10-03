use std::path::Path;

use crate::Result;
use crate::console::out;

use super::open_store_mut;

/// `trurlic remove pattern`: delete a pattern and its edges. Member
/// decisions are kept.
pub(crate) fn remove_pattern(cwd: &Path, name: &str) -> Result<()> {
    let (store, lock, mut state) = open_store_mut(cwd)?;
    store.remove_pattern(&lock, &mut state, name)?;
    out!("Removed pattern `{name}`")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;
    use crate::commands::{add_component, decide, init, remove_decision};
    use crate::store::{RecordPatternParams, Store};
    use tempfile::TempDir;

    #[test]
    fn remove_pattern_lets_its_members_be_removed() {
        let tmp = TempDir::new().unwrap();
        init(tmp.path()).unwrap();
        add_component(tmp.path(), "auth", None).unwrap();
        decide(tmp.path(), "auth", "Use JWT", "Stateless tokens", &[], &[]).unwrap();
        decide(
            tmp.path(),
            "auth",
            "Rotate keys",
            "Bounded exposure",
            &[],
            &[],
        )
        .unwrap();
        let store = Store::discover(tmp.path()).unwrap();
        let mut state = store.load_state().unwrap();
        let lock = store.lock().unwrap();
        let members = ["use-jwt".to_owned(), "rotate-keys".to_owned()];
        let params = RecordPatternParams {
            name: "token-hygiene",
            description: "Coordinated token handling",
            decisions: &members,
            components: &[],
            tags: &[],
        };
        store.record_pattern(&lock, &mut state, params).unwrap();
        drop(lock);

        // The pattern's two-member floor blocks removing either member.
        let blocked = remove_decision(tmp.path(), "use-jwt").unwrap_err();
        assert!(matches!(blocked, Error::CascadeBlocked(_)), "{blocked}");

        remove_pattern(tmp.path(), "token-hygiene").unwrap();
        remove_decision(tmp.path(), "use-jwt").unwrap();
        assert!(!store.pattern_path("token-hygiene").exists());
    }
}
