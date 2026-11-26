//! What attributes and modifiers state of an item.

pub(super) fn for_the_parent() {}

pub(in crate::marks) fn for_a_path() {}

pub(self) fn for_itself() {}

crate fn the_old_spelling() {}

#[tokio::test]
async fn runs_on_a_runtime() {}

#[bench]
fn measures(b: &mut Bencher) {}

#[cfg(all(test, unix))]
fn only_under_test_on_unix() {}

#[cfg(any(test, feature = "bench"))]
pub fn also_built_for_benches() {}

#[cfg(not(test))]
fn never_under_test() {}

pub(crate) mod nested {
    #![cfg(test)]

    pub fn inside_a_test_module() {}
}
