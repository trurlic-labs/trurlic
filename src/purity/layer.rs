//! The check of a module's place among the crate's modules: each path its
//! non-test code names into the crate goes into a module or item of a
//! list.

use super::path::{from_crate, paths};
use super::read::{Crate, is_within};

impl Crate {
    /// Each path the non-test code of the module `module` (`store::write`),
    /// a path from the crate root, names into the crate outside every path
    /// of `within`, as `file:line: names `path``, and each of the crate's
    /// [`refusals`](Crate::refusals), files sorted. Each path of `within`
    /// is a module or an item from the crate root (`store::model`,
    /// `Error`), so a path through a re-export of a module above them names
    /// that module's item and is outside, and a glob of the crate root
    /// (`crate::*`) is outside every list. An alias of a module above them
    /// is outside too: what it names ends in `as`, which no listed path
    /// holds.
    pub(crate) fn places_beyond(&self, module: &str, within: &[&str]) -> Vec<String> {
        let mut found = self.refusals();
        for (path, file) in self.files_of(module) {
            for named in paths(file) {
                let Some(from_root) = from_crate(&named.segments, &named.module) else {
                    continue;
                };
                if !within.iter().any(|listed| is_within(&from_root, listed)) {
                    let spelled = named.segments.join("::");
                    found.push(format!("{path}:{}: names `{spelled}`", named.line));
                }
            }
        }
        found
    }
}
