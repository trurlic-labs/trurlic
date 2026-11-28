//! What the tests of other modules read through the shell: the facts of
//! this repository's own tree, read once per test process.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::OnceLock;

use tempfile::TempDir;

use super::reads::{Reads, Scope, read_tree};
use crate::code::CodeFacts;
use crate::store::Store;
use crate::store::testing::setup_store;

/// The facts of this repository's tree, as a command reads them with an
/// empty cache, and the paths the walk took.
pub(crate) fn this_repository() -> &'static (CodeFacts, BTreeSet<String>) {
    static READ: OnceLock<(CodeFacts, BTreeSet<String>)> = OnceLock::new();
    READ.get_or_init(|| {
        let tmp = TempDir::new().unwrap();
        let cache = setup_store(tmp.path());
        let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let store = Store::reached_from(cache.root().to_path_buf(), repository);
        let mut reads = Reads::of_tree(&store);
        let anchored = std::iter::empty();
        let (entries, walked) =
            read_tree(&store, store.code_root(), Scope::Tree, anchored, &mut reads);
        // clone: the facts own the walked set, which the tests iterate too.
        (CodeFacts::new(entries, walked.clone()), walked)
    })
}
