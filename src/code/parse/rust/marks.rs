//! What an item's attributes and modifier state of it: whether it is
//! test code, and its visibility.
//!
//! An attribute makes its item test code when its path ends in `test`
//! (`#[test]`, `#[tokio::test]`, `#[async_std::test]`) or is `bench`, or
//! when it is a `cfg` whose predicate holds only under `test`:
//! `cfg(test)`, an `all(..)` that holds `test`, an `any(..)` whose every
//! member does. `cfg(any(test, feature = "x"))` is code a feature
//! builds, and `cfg(not(test))` code no test builds: neither is test
//! code. The attribute is read from its tokens.

use std::ops::ControlFlow;

use tree_sitter::Node;

use crate::code::index::{Role, Visibility};
use crate::code::parse::parser::text_of;
use crate::code::parse::tokens::{self, Token};

use super::lexicon::LEXICON;

/// What an attribute says of the item it is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Marks<'n> {
    pub(super) role: Role,
    /// `#[macro_export]`, or a `cfg_attr` that applies it.
    pub(super) macro_export: bool,
    /// The string of a `#[path = ".."]`.
    pub(super) path: Option<Node<'n>>,
}

impl<'n> Marks<'n> {
    /// What these and `more`, the marks of a later attribute, say
    /// together.
    pub(super) fn and(self, more: Self) -> Self {
        Self {
            role: self.role.or(more.role),
            macro_export: self.macro_export || more.macro_export,
            path: more.path.or(self.path),
        }
    }
}

/// What `attribute`, an `attribute_item` or `inner_attribute_item`, says
/// of the item it is on.
pub(super) fn attribute_item_marks<'n>(attribute: Node<'n>, text: &str) -> Marks<'n> {
    let mut cursor = attribute.walk();
    let Some(inner) = attribute
        .named_children(&mut cursor)
        .find(|child| child.kind() == "attribute")
    else {
        return Marks {
            role: Role::Code,
            macro_export: false,
            path: None,
        };
    };
    let nodes = tokens_of(inner, text);
    let tokens: Vec<&str> = nodes.iter().map(|token| text_of(*token, text)).collect();
    let macro_export = match tokens.as_slice() {
        ["macro_export", ..] => true,
        ["cfg_attr", rest @ ..] => rest.contains(&"macro_export"),
        _ => false,
    };
    let path = match (tokens.as_slice(), nodes.get(2)) {
        (["path", "=", _], Some(literal)) if literal.kind() == "string_literal" => Some(*literal),
        _ => None,
    };
    Marks {
        role: attribute_role(&tokens),
        macro_export,
        path,
    }
}

/// What an attribute spelled by `tokens`, those between its brackets,
/// makes of its item.
fn attribute_role(tokens: &[&str]) -> Role {
    let arguments = tokens
        .iter()
        .position(|token| matches!(*token, "(" | "="))
        .unwrap_or(tokens.len());
    let (path, predicate) = tokens.split_at(arguments);
    let test = match path {
        ["cfg"] => holds_only_under_test(predicate),
        [.., "test"] | ["bench"] => true,
        _ => false,
    };
    match test {
        true => Role::Test,
        false => Role::Code,
    }
}

/// How a `cfg` operator combines the truths of its members.
enum Operator {
    All,
    Any,
    /// `not`, or a name `cfg` does not know: never only under `test`.
    Other,
}

impl Operator {
    fn named(word: &str) -> Self {
        match word {
            "all" => Self::All,
            "any" => Self::Any,
            _ => Self::Other,
        }
    }

    /// Whether the operator over members of `values` holds only under
    /// `test`.
    fn holds_only_under_test(&self, values: &[bool]) -> bool {
        match self {
            Self::All => values.contains(&true),
            Self::Any => !values.is_empty() && !values.contains(&false),
            Self::Other => false,
        }
    }
}

/// Whether the `cfg` predicate spelled by `tokens`, from its opening
/// parenthesis, holds only when `test` does. One pass with an explicit
/// stack; a malformed predicate holds under no `test` alone.
fn holds_only_under_test(tokens: &[&str]) -> bool {
    let mut frames: Vec<(Operator, Vec<bool>)> = Vec::new();
    let mut at = 0;
    while let Some(&token) = tokens.get(at) {
        let next = tokens.get(at + 1).copied();
        at += 1;
        let value = match (token, next) {
            ("(", _) => {
                frames.push((Operator::All, Vec::new()));
                continue;
            }
            (",", _) => continue,
            (")", _) => match frames.pop() {
                Some((operator, values)) => operator.holds_only_under_test(&values),
                None => return false,
            },
            (word, Some("(")) => {
                at += 1;
                frames.push((Operator::named(word), Vec::new()));
                continue;
            }
            // `feature = "x"`: its literal is passed over too.
            (_, Some("=")) => {
                at += 2;
                false
            }
            (word, _) => word == "test",
        };
        match frames.last_mut() {
            Some((_, values)) => values.push(value),
            None => return value && at == tokens.len(),
        }
    }
    false
}

/// `role`, or test code when an inner attribute among the children of
/// `node` (`#![cfg(test)]`) makes it.
pub(super) fn inner_role(node: Node<'_>, role: Role, text: &str) -> Role {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|child| child.kind() == "inner_attribute_item")
        .map(|attribute| attribute_item_marks(attribute, text).role)
        .fold(role, Role::or)
}

/// The nodes of the tokens of `node`, in order: its leaves, a literal as
/// one token, comments left out (`tokens::tokens`).
fn tokens_of<'n>(node: Node<'n>, text: &str) -> Vec<Node<'n>> {
    let mut found = Vec::new();
    tokens::tokens(node, text, &LEXICON, |token| {
        if let Token::Node(node) = token {
            found.push(node);
        }
        ControlFlow::Continue(())
    });
    found
}

/// The visibility `item`'s modifier states, `Private` when it has none.
/// The modifier is read from its tokens, so a comment inside it
/// (`pub(/* why */ crate)`) changes nothing.
pub(super) fn visibility_of(item: Node<'_>, text: &str) -> Visibility {
    let mut cursor = item.walk();
    let Some(modifier) = item
        .named_children(&mut cursor)
        .find(|child| child.kind() == "visibility_modifier")
    else {
        return Visibility::Private;
    };
    let tokens: Vec<&str> = tokens_of(modifier, text)
        .into_iter()
        .map(|token| text_of(token, text))
        .collect();
    Visibility::of_modifier(&tokens.join(" ")).unwrap_or(Visibility::Private)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::index::{FileKind, Visibility};
    use crate::code::testing;

    fn role(attribute: &str) -> Role {
        let tokens: Vec<&str> = attribute.split(' ').collect();
        attribute_role(&tokens)
    }

    #[test]
    fn an_attribute_makes_test_code_only_when_it_holds_only_under_test() {
        for (attribute, expected) in [
            ("test", Role::Test),
            ("tokio :: test", Role::Test),
            ("tokio :: test ( flavor = \"current_thread\" )", Role::Test),
            ("bench", Role::Test),
            ("cfg ( test )", Role::Test),
            ("cfg ( all ( test , feature = \"x\" ) )", Role::Test),
            (
                "cfg ( all ( unix , any ( test , all ( test , windows ) ) ) )",
                Role::Test,
            ),
            ("cfg ( any ( test , feature = \"bench\" ) )", Role::Code),
            ("cfg ( not ( test ) )", Role::Code),
            ("cfg ( any ( ) )", Role::Code),
            ("cfg ( unix )", Role::Code),
            ("cfg ( test", Role::Code),
            ("cfg ( test ) )", Role::Code),
            ("test_case ( 1 )", Role::Code),
            ("derive ( Debug )", Role::Code),
            ("cfg_attr ( test , derive ( Debug ) )", Role::Code),
        ] {
            assert_eq!(role(attribute), expected, "{attribute}");
        }
    }

    #[test]
    fn a_macro_is_exported_by_its_attribute_not_by_a_word_in_its_docs() {
        let text = "/// Not #[macro_export]: local.\nmacro_rules! local { () => {} }\n#[cfg_attr(feature = \"x\", macro_export)]\nmacro_rules! maybe { () => {} }\n#[macro_export]\nmacro_rules! shared { () => {} }\n";
        let indexed = testing::index(FileKind::Rust, text.as_bytes()).unwrap();
        let visibility: Vec<_> = indexed.definitions.iter().map(|d| d.visibility).collect();

        assert_eq!(
            visibility,
            [
                Some(Visibility::Private),
                Some(Visibility::Public),
                Some(Visibility::Public)
            ]
        );
    }
}
