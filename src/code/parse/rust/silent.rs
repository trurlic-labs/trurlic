//! The tokens of a Rust tree that carry no meaning, which no span hash
//! holds. They are a list's trailing comma, but the one that makes a
//! one-element tuple; the braces around a closure's or a match arm's body
//! of one expression, with the `;` of a lone `return`, `break` or
//! `continue` there; the outer pair of nested parentheses around an
//! expression (`((a))`); and a match arm pattern's leading `|`. rustfmt's
//! edits to these move no hash. Three of its edits do move one: the
//! trailing comma it adds to a `vec![..]` it breaks, as a comma or a brace
//! inside a macro's tokens stays a token, which only the macro gives a
//! meaning; its merge of two `#[derive]` attributes into one; and its sort
//! of `use` declarations and of a use list's entries, which moves the hash
//! of every item that holds them, as a hash holds its tokens in order.

use tree_sitter::Node;

use crate::code::parse::tokens::is_comment;

/// The kinds of the nodes whose last `,`, before their closing delimiter
/// or at their end, carries no meaning.
const LISTS: [&str; 20] = [
    "arguments",
    "parameters",
    "type_arguments",
    "type_parameters",
    "for_lifetimes",
    "use_bounds",
    "where_clause",
    "use_list",
    "field_declaration_list",
    "ordered_field_declaration_list",
    "enum_variant_list",
    "field_initializer_list",
    "array_expression",
    "slice_pattern",
    "struct_pattern",
    "tuple_struct_pattern",
    "match_arm",
    "tuple_expression",
    "tuple_type",
    "tuple_pattern",
];

/// The lists of [`LISTS`] whose comma after one element makes them a
/// tuple (`(a,)`), where it is kept.
const TUPLES: [&str; 3] = ["tuple_expression", "tuple_type", "tuple_pattern"];

/// The kinds whose body block of one expression means that expression.
const BODIES: [&str; 2] = ["closure_expression", "match_arm"];

/// The expressions that diverge, so a body of one of them means the same
/// with its `;` or without.
const DIVERGING: [&str; 3] = [
    "return_expression",
    "break_expression",
    "continue_expression",
];

/// The start of each token under `root` that carries no meaning, sorted.
/// One walk: each node's children are read three times at most.
pub(super) fn silent_tokens(root: Node<'_>) -> Vec<usize> {
    let mut found = Vec::new();
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        if LISTS.contains(&node.kind()) {
            found.extend(trailing_comma(node));
        }
        if BODIES.contains(&node.kind()) {
            found.extend(bare_block(node));
        }
        if node.kind() == "parenthesized_expression" {
            found.extend(nested_parentheses(node));
        }
        if node.kind() == "match_pattern" {
            found.extend(leading_pipe(node));
        }
        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                found.sort_unstable();
                return found;
            }
        }
    }
}

/// The start of `list`'s last `,` when only its closing delimiter follows
/// it, and in a tuple two elements or more come before it.
fn trailing_comma(list: Node<'_>) -> Option<usize> {
    let mut cursor = list.walk();
    let tokens: Vec<Node<'_>> = list
        .children(&mut cursor)
        .filter(|child| !is_comment(*child) && child.kind() != "attribute_item")
        .collect();
    let comma = tokens.iter().rposition(|token| token.kind() == ",")?;
    let last = tokens
        .get(comma + 1)
        .is_none_or(|next| [")", "]", "}", ">"].contains(&next.kind()));
    let elements = tokens
        .iter()
        .filter(|token| !["(", ")", ","].contains(&token.kind()))
        .count();
    let silent = last && (!TUPLES.contains(&list.kind()) || elements >= 2);
    tokens.get(comma).filter(|_| silent).map(Node::start_byte)
}

/// The outer parentheses of `expression`, a parenthesized expression,
/// when all they hold is another one: `((a))` means `(a)`.
fn nested_parentheses(expression: Node<'_>) -> Vec<usize> {
    let mut cursor = expression.walk();
    let tokens: Vec<Node<'_>> = expression
        .children(&mut cursor)
        .filter(|child| !is_comment(*child))
        .collect();
    match tokens.as_slice() {
        [open, inner, close] if inner.kind() == "parenthesized_expression" => {
            vec![open.start_byte(), close.start_byte()]
        }
        _ => Vec::new(),
    }
}

/// The start of the `|` a match arm's `pattern` opens with, which joins
/// it to nothing.
fn leading_pipe(pattern: Node<'_>) -> Option<usize> {
    let mut first = pattern;
    loop {
        let mut cursor = first.walk();
        let next = first
            .children(&mut cursor)
            .find(|child| !is_comment(*child));
        match next {
            Some(next) => first = next,
            None => break,
        }
    }
    (first.kind() == "|").then(|| first.start_byte())
}

/// The braces of `holder`'s body block when it holds one token tree of
/// meaning, and that `;` when it ends a diverging statement. A block of
/// one statement that does not diverge (`{ f(x); }`) means `()`, not
/// `f(x)`, so its braces stay. Braces around anything else would leave no
/// valid Rust without them, so they tell no two programs apart.
fn bare_block(holder: Node<'_>) -> Vec<usize> {
    let mut cursor = holder.walk();
    let Some(block) = holder
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "block")
        .last()
    else {
        return Vec::new();
    };
    let mut cursor = block.walk();
    let tokens: Vec<Node<'_>> = block
        .children(&mut cursor)
        .filter(|child| !is_comment(*child))
        .collect();
    let [open, only, close] = tokens.as_slice() else {
        return Vec::new();
    };
    let mut found = vec![open.start_byte(), close.start_byte()];
    let semicolon = only
        .child(only.child_count().saturating_sub(1))
        .filter(|end| only.kind() == "expression_statement" && end.kind() == ";");
    if let Some(end) = semicolon {
        let diverging = only
            .named_child(0)
            .is_some_and(|expression| DIVERGING.contains(&expression.kind()));
        if !diverging {
            return Vec::new();
        }
        found.push(end.start_byte());
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::index::SpanHash;
    use crate::code::parse::rust::LEXICON;
    use crate::code::parse::syntax::parse;
    use crate::code::parse::units::Leaves;

    /// The hash of the whole of `text`, a Rust file.
    fn hash(text: &str) -> SpanHash {
        let tree = parse(text, &tree_sitter_rust::LANGUAGE.into()).unwrap();
        let silent = silent_tokens(tree.root_node());
        let leaves = Leaves::of(&tree, text, &LEXICON, &silent);
        let units = leaves.within(&(0..text.len()));
        leaves.hash_each(text, std::slice::from_ref(&units))[0]
    }

    #[test]
    fn a_trailing_comma_that_carries_no_meaning_moves_no_hash() {
        let alike = [
            (
                "fn f(a: u8, b: u8) { g(a, b) }",
                "fn f(a: u8, b: u8,) { g(a, b,) }",
            ),
            (
                "fn f<T, U>() where T: A, U: B {}",
                "fn f<T, U,>() where T: A, U: B, {}",
            ),
            (
                "fn f() { S { a: 1, b: [1, 2] }; }",
                "fn f() { S { a: 1, b: [1, 2,], }; }",
            ),
            (
                "fn f(x: u8) { match x { 0 => 1, _ => 2 } }",
                "fn f(x: u8) { match x { 0 => 1, _ => 2, } }",
            ),
            (
                "fn f(x: u8) { match x { 0 => {} _ => {} } }",
                "fn f(x: u8) { match x { 0 => {}, _ => {}, } }",
            ),
            (
                "fn f() { let (a, b) = (1, 2); }",
                "fn f() { let (a, b,) = (1, 2,); }",
            ),
            ("fn f() -> (u8, u8) { g() }", "fn f() -> (u8, u8,) { g() }"),
            (
                "fn f() { let Some(a) = x; }",
                "fn f() { let Some(a,) = x; }",
            ),
        ];
        for (one, two) in alike {
            assert_eq!(hash(one), hash(two), "{two}");
        }
    }

    #[test]
    fn the_comma_of_a_one_element_tuple_or_in_a_macros_tokens_moves_the_hash() {
        let apart = [
            ("fn f() { let a = (1); }", "fn f() { let a = (1,); }"),
            ("fn f() -> (u8) { g() }", "fn f() -> (u8,) { g() }"),
            ("fn f() { let (a) = x; }", "fn f() { let (a,) = x; }"),
            ("fn f() { vec![1, 2] }", "fn f() { vec![1, 2,] }"),
        ];
        for (one, two) in apart {
            assert_ne!(hash(one), hash(two), "{two}");
        }
    }

    #[test]
    fn braces_around_a_closure_or_arm_body_of_one_expression_move_no_hash() {
        let alike = [
            ("fn f() { g(|x| h(x)) }", "fn f() { g(|x| { h(x) }) }"),
            (
                "fn f(x: u8) { match x { 0 => g(x), _ => h() } }",
                "fn f(x: u8) { match x { 0 => { g(x) } _ => { h() } } }",
            ),
            (
                "fn f(x: u8) { match x { 0 => return, _ => h() } }",
                "fn f(x: u8) { match x { 0 => { return; } _ => h() } }",
            ),
            (
                "fn f() { g(|x| match x { _ => 1 }) }",
                "fn f() { g(|x| { match x { _ => 1 } }) }",
            ),
        ];
        for (one, two) in alike {
            assert_eq!(hash(one), hash(two), "{two}");
        }
    }

    #[test]
    fn nested_parentheses_and_a_match_arms_leading_pipe_move_no_hash() {
        let alike = [
            ("fn f(a: u8) -> u8 { (a) }", "fn f(a: u8) -> u8 { (((a))) }"),
            (
                "fn f(a: u8) -> u8 { match a { 1 | 2 => 0, _ => 1 } }",
                "fn f(a: u8) -> u8 { match a { | 1 | 2 => 0, _ => 1 } }",
            ),
            (
                "fn f(a: u8) -> u8 { match a { 1 => 0, _ => 1 } }",
                "fn f(a: u8) -> u8 { match a { | 1 => 0, _ => 1 } }",
            ),
        ];
        for (one, two) in alike {
            assert_eq!(hash(one), hash(two), "{two}");
        }
    }

    #[test]
    fn parentheses_of_meaning_move_the_hash() {
        let apart = [
            ("fn f(a: u8) -> u8 { a }", "fn f(a: u8) -> u8 { (a) }"),
            ("fn f() { let (a) = 1; }", "fn f() { let ((a)) = 1; }"),
            (
                "fn f(a: u8) -> u8 { (a) + 1 }",
                "fn f(a: u8) -> u8 { ((a) + 1) }",
            ),
        ];
        for (one, two) in apart {
            assert_ne!(hash(one), hash(two), "{two}");
        }
    }

    /// A file as written, and as rustfmt 1.95's defaults format it.
    const UNFORMATTED: &str = "#[derive(Debug)]\n#[derive(Clone)]\npub struct S { a: u8 }\npub fn vecs() -> Vec<u32> { vec![1111111111, 2222222222, 3333333333, 4444444444, 5555555555, 6666666666, 7777777777] }\npub fn asserts(a: u32, b: u32) { assert_eq!(a, b, \"the values differ: {} and {} and some more text to break\", a, b); }\npub fn parens(a: u32) -> u32 { ((a)) }\npub fn pipes(a: u32) -> u32 { match a { | 1 | 2 => 0, _ => 1 } }\npub fn long_arm(a: u32) -> u32 { match a { 1 => some_function_with_a_long_name(a, a, a, a, a, a, a, a) + another_function_with_a_long_name(a, a), _ => 1 } }\nfn some_function_with_a_long_name(_: u32, _: u32, _: u32, _: u32, _: u32, _: u32, _: u32, _: u32) -> u32 { 0 }\npub fn closure() -> impl Fn(u32) -> u32 { |x| { x + 1 } }\npub fn ret(a: u32) -> u32 { match a { 0 => { return 1; } _ => 2 } }\npub fn uses() { use b::B; use a::A; }\npub mod listed { pub use super::{Zed, Alpha}; }\n";
    const FORMATTED: &str = "#[derive(Debug, Clone)]\npub struct S {\n    a: u8,\n}\npub fn vecs() -> Vec<u32> {\n    vec![\n        1111111111, 2222222222, 3333333333, 4444444444, 5555555555, 6666666666, 7777777777,\n    ]\n}\npub fn asserts(a: u32, b: u32) {\n    assert_eq!(\n        a, b,\n        \"the values differ: {} and {} and some more text to break\",\n        a, b\n    );\n}\npub fn parens(a: u32) -> u32 {\n    (a)\n}\npub fn pipes(a: u32) -> u32 {\n    match a {\n        1 | 2 => 0,\n        _ => 1,\n    }\n}\npub fn long_arm(a: u32) -> u32 {\n    match a {\n        1 => {\n            some_function_with_a_long_name(a, a, a, a, a, a, a, a)\n                + another_function_with_a_long_name(a, a)\n        }\n        _ => 1,\n    }\n}\nfn some_function_with_a_long_name(\n    _: u32,\n    _: u32,\n    _: u32,\n    _: u32,\n    _: u32,\n    _: u32,\n    _: u32,\n    _: u32,\n) -> u32 {\n    0\n}\npub fn closure() -> impl Fn(u32) -> u32 {\n    |x| x + 1\n}\npub fn ret(a: u32) -> u32 {\n    match a {\n        0 => {\n            return 1;\n        }\n        _ => 2,\n    }\n}\npub fn uses() {\n    use a::A;\n    use b::B;\n}\npub mod listed {\n    pub use super::{Alpha, Zed};\n}\n";

    #[test]
    fn rustfmt_moves_only_the_hash_of_merged_derives_a_comma_in_a_macros_tokens_and_sorted_uses() {
        let hashes = |text: &str| -> std::collections::BTreeMap<String, SpanHash> {
            let index =
                crate::code::testing::index(crate::code::FileKind::Rust, text.as_bytes()).unwrap();
            let scopes = crate::code::index::Scopes(&index.scopes);
            index
                .definitions
                .iter()
                .map(|d| (crate::code::testing::path(scopes, d.scope), d.hash))
                .collect()
        };
        let (before, after) = (hashes(UNFORMATTED), hashes(FORMATTED));
        let moved: Vec<&str> = before
            .iter()
            .filter(|(path, hash)| after.get(*path) != Some(hash))
            .map(|(path, _)| path.as_str())
            .collect();

        assert_eq!(before.len(), 14);
        assert_eq!(moved, ["S", "listed", "uses", "vecs"]);
    }

    #[test]
    fn braces_around_a_statement_or_more_than_one_move_the_hash() {
        let apart = [
            ("fn f() { g(|x| h(x)) }", "fn f() { g(|x| { h(x); }) }"),
            (
                "fn f() { g(|x| h(x)) }",
                "fn f() { g(|x| { let y = x; h(y) }) }",
            ),
            ("fn f() { let a = b; }", "fn f() { let a = { b }; }"),
        ];
        for (one, two) in apart {
            assert_ne!(hash(one), hash(two), "{two}");
        }
    }
}
