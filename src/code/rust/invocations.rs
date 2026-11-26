//! A macro invocation defines nothing: what it produces is its macro's,
//! which the index does not run, whether a `macro_rules!` body or the
//! invocation's own tokens spell it. The names the top-level tokens of an
//! invocation where an item stands spell are kept apart
//! ([`RustFacts::invoked`](crate::code::index::RustFacts)), so an anchor
//! that names one is told why it names nothing.

use std::collections::BTreeSet;

use tree_sitter::Node;

use crate::code::syntax::text_of;

/// The distinct names the top-level tokens of `invocation`, a
/// `macro_invocation`, spell, added to `names`: its token tree's
/// identifiers, no keyword, no name inside a nested group.
pub(super) fn invoked_names<'t>(
    invocation: Node<'_>,
    text: &'t str,
    names: &mut BTreeSet<&'t str>,
) {
    let mut cursor = invocation.walk();
    let Some(tree) = invocation
        .named_children(&mut cursor)
        .find(|child| child.kind() == "token_tree")
    else {
        return;
    };
    let mut cursor = tree.walk();
    names.extend(
        tree.named_children(&mut cursor)
            .filter(|token| token.kind() == "identifier")
            .map(|token| text_of(token, text)),
    );
}

#[cfg(test)]
mod tests {
    use crate::code::index::FileKind;

    fn invoked(text: &str) -> Vec<String> {
        crate::code::testing::index(FileKind::Rust, text.as_bytes())
            .unwrap()
            .rust
            .invoked
    }

    #[test]
    fn the_names_an_invocation_where_an_item_stands_spells_at_its_top_are_kept_once() {
        let text = "view! { pub struct Ledger { open: u8 } }\nmod m { ids::id!(Rank(u8), noun = \"rank\"); }\nimpl T { forward! { fn made() {} } }\nfn f() { local!(Inner); }\nconst X: u8 = m!(Value);\nview! { struct Ledger; }\n";

        assert_eq!(invoked(text), ["Ledger", "Rank", "made", "noun"]);
    }
}
