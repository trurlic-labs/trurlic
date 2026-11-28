//! The check of a module's place among the crate's modules: each path its
//! non-test code names into the crate goes into a module or item of a
//! list, and none it re-exports goes out of the module.

use super::read::{Crate, Named, is_within};

impl Crate {
    /// Each path the non-test code of the module `module` (`store::write`,
    /// `bench`), a path from the crate root, names into the crate outside
    /// every path of `within`, as `file:line: names `path``, and each of
    /// the crate's [`refused`](Crate::refused) forms, files sorted. Each
    /// path of `within` is a module or an item from the crate root
    /// (`store::model`, `Error`), so a path through a re-export of a
    /// module above them names that module's item and is outside, and a
    /// glob of the crate root (`crate::*`) is outside every list. An alias
    /// of a module above them is outside too: what it names ends in `as`,
    /// which no listed path holds.
    pub(crate) fn places_beyond(&self, module: &str, within: &[&str]) -> Vec<String> {
        self.paths_beyond(module, |_, from_root| {
            let listed = within.iter().any(|listed| is_within(from_root, listed));
            (!listed).then_some("names")
        })
    }

    /// Each path the non-test code of the module `module` re-exports from
    /// outside it, as `file:line: re-exports `path``, and each of the
    /// crate's [`refused`](Crate::refused) forms, files sorted.
    pub(crate) fn re_exports_beyond(&self, module: &str) -> Vec<String> {
        self.paths_beyond(module, |named, from_root| {
            (named.exported && !is_within(from_root, module)).then_some("re-exports")
        })
    }

    /// The crate's refused forms, then each path the non-test code of the
    /// module `module` names into the crate that `fault` judges, given the
    /// path and its segments from the crate root, as `file:line: <what
    /// fault says> `path``.
    fn paths_beyond(
        &self,
        module: &str,
        fault: impl Fn(&Named, &[&str]) -> Option<&'static str>,
    ) -> Vec<String> {
        let mut found = self.refused.clone();
        for (path, file) in &self.files {
            for named in &file.paths {
                let Some(named_in) = file.modules.get(named.module) else {
                    continue;
                };
                if !is_within(named_in, module) {
                    continue;
                }
                let Some(from_root) = named.crate_path(named_in) else {
                    continue;
                };
                if let Some(what) = fault(named, &from_root) {
                    let spelled = named.segments.join("::");
                    found.push(format!("{path}:{}: {what} `{spelled}`", named.line));
                }
            }
        }
        found
    }
}
