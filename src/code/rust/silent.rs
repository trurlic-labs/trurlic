//! The tokens of a Rust tree that carry no meaning, which no span hash
//! holds: what rustfmt adds and removes as a line breaks or joins. They
//! are a list's trailing comma, but the one that makes a one-element
//! tuple, and the braces around a closure's or a match arm's body of one
//! expression, with the `;` of a lone `return`, `break` or `continue`
//! there. A comma or a brace inside a macro's tokens stays a token: only
//! the macro gives it a meaning.

use tree_sitter::Node;

use crate::code::syntax::is_comment;

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
/// One walk: each node's children are read once or twice.
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
    use crate::code::rust::LEXICON;
    use crate::code::span::SpanHash;
    use crate::code::syntax::parse;
    use crate::code::units::Leaves;

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
