//! A macro invocation where an item or a statement can stand: the items
//! its tokens spell (`spelled.rs`) are definitions under the invocation's
//! container, with the fields and variants they hold, and an invocation
//! where an item stands is kept for the expansion of the crate's
//! type-defining macros (`expand.rs`).

use crate::code::index::{IndexFault, Invocation};
use crate::code::kinds::DefinitionKind;
use crate::code::scope::ScopeId;
use crate::code::symbol::Segment;
use crate::code::tree::UNHASHED;

use crate::code::kinds::What;

use super::items::Within;
use super::spelled::{self, Spelled, SpelledItem};
use super::{Context, Visit, Walk, macros};

impl Walk<'_> {
    /// Define the items an invocation where an item or a statement can
    /// stand spells, and keep it for the expansion of the crate's macros.
    /// `outer` is the invocation, or the statement that holds it.
    pub(super) fn read_invocation(
        &mut self,
        invocation: Visit<'_>,
        outer: Visit<'_>,
    ) -> Result<(), IndexFault> {
        if !matches!(outer.parent, "source_file" | "declaration_list" | "block") {
            return Ok(());
        }
        let (node, context) = (invocation.node, invocation.context);
        // The scope of each item defined, by its place: a spelled module's
        // items are in its scope.
        let mut scopes: Vec<Option<ScopeId>> = Vec::new();
        for spelled in spelled::spelled_items(node, self.text, &mut self.build.work)? {
            let inner = match spelled.within {
                None => context,
                Some(module) => match scopes.get(module).copied().flatten() {
                    Some(scope) => Context {
                        scope: Some(scope),
                        ..context
                    },
                    None => {
                        scopes.push(None);
                        continue;
                    }
                },
            };
            scopes.push(self.define_spelled_item(inner, &spelled)?);
        }
        // A type-defining macro is invoked where an item stands.
        if !matches!(outer.parent, "source_file" | "declaration_list") {
            return Ok(());
        }
        let Some((macro_name, arguments)) = macros::invocation_arguments(node, self.text) else {
            return Ok(());
        };
        let range = outer.start..outer.node.end_byte();
        let invocation = Invocation {
            macro_name,
            container: context.scope,
            span: self.build.span(&range),
            hash: UNHASHED,
            arguments: arguments.elements,
            whole: arguments.whole,
            role: context.role,
        };
        self.build.invoke(invocation, range)
    }

    /// Define an item an invocation in `context` spells, and its fields
    /// and variants; its scope, `None` for a keyword of no item.
    fn define_spelled_item(
        &mut self,
        context: Context,
        spelled: &SpelledItem<'_>,
    ) -> Result<Option<ScopeId>, IndexFault> {
        let Some(kind) = DefinitionKind::of_keyword(spelled.keyword, context.within.container())
        else {
            return Ok(None);
        };
        let what = What {
            kind,
            role: context.role.or(spelled.item.role),
            visibility: context.within.visibility(spelled.item.visibility),
        };
        let scope = self.define_spelled(context.scope, &spelled.item, what)?;
        // A variant, and a variant's field, is as visible as its enum.
        let within = match kind == DefinitionKind::Enum {
            true => Within::Enum(spelled.item.visibility),
            false => Within::Items,
        };
        let inner = Context {
            scope: Some(scope),
            within,
            role: what.role,
        };
        for member in &spelled.members {
            let member_what = member_what(inner, member.kind, &member.member);
            let member_scope = self.define_spelled(inner.scope, &member.member, member_what)?;
            for field in &member.fields {
                let field_what = What {
                    kind: DefinitionKind::Field,
                    role: member_what.role.or(field.role),
                    visibility: member_what.visibility,
                };
                self.define_spelled(Some(member_scope), field, field_what)?;
            }
        }
        Ok(Some(scope))
    }

    /// Define `spelled` under `parent` as `what` says; its scope.
    fn define_spelled(
        &mut self,
        parent: Option<ScopeId>,
        spelled: &Spelled<'_>,
        what: What,
    ) -> Result<ScopeId, IndexFault> {
        let scope = self
            .build
            .scopes
            .enter(parent, vec![Segment::name(spelled.name)])?;
        self.build.define(scope, spelled.start..spelled.end, what)?;
        Ok(scope)
    }
}

/// What a member of a spelled item is: its kind, its item's role or its
/// own attributes', and its own visibility, or its enum's.
fn member_what(inner: Context, kind: DefinitionKind, member: &Spelled<'_>) -> What {
    What {
        kind,
        role: inner.role.or(member.role),
        visibility: inner.within.visibility(member.visibility),
    }
}

#[cfg(test)]
mod tests {
    use crate::code::index::FileKind;
    use crate::code::kinds::Role;
    use crate::code::testing;

    #[test]
    fn a_spelled_struct_and_enum_define_their_fields_and_variants() {
        let text = "view! {\n    pub struct View {\n        #[serde(default)]\n        pub(crate) contributions: u32,\n        hidden: u8,\n        (u8),\n    }\n}\nmirror! {\n    pub enum Held { Lent, Kept { since: u32 }, Lost(u8) = 3 }\n}\nrecord! { enum other::Named as Named { Value } }\n";
        let paths: Vec<String> = testing::defined(FileKind::Rust, text)
            .into_iter()
            .map(|(path, span)| format!("{path} = {span}"))
            .collect();

        assert_eq!(
            paths,
            [
                "View = pub struct View {\n        #[serde(default)]\n        pub(crate) contributions: u32,\n        hidden: u8,\n        (u8),\n    }",
                "View::contributions = #[serde(default)]\n        pub(crate) contributions: u32",
                "View::hidden = hidden: u8",
                "Held = pub enum Held { Lent, Kept { since: u32 }, Lost(u8) = 3 }",
                "Held::Lent = Lent",
                "Held::Kept = Kept { since: u32 }",
                "Held::Kept::since = since: u32",
                "Held::Lost = Lost(u8) = 3",
            ]
        );
    }

    #[test]
    fn a_test_module_or_condition_in_an_invocation_marks_what_it_holds() {
        let text = "m! { #[cfg(test)] mod t { fn helper() {} } }\ncfg_if! { if #[cfg(test)] { fn mocked() {} } else { fn real() {} } }\n";
        let indexed = crate::code::index::index(FileKind::Rust, text.as_bytes()).unwrap();
        let roles: Vec<_> = indexed.definitions.iter().map(|d| d.role).collect();

        assert_eq!(roles, [Role::Test, Role::Test, Role::Test, Role::Code]);
    }

    #[test]
    fn a_run_of_spelled_keywords_spans_each_item_alone_within_the_budget() {
        let run = format!("m! {{ {}{{}} }}\n", "fn a ".repeat(40_000));
        let spans = testing::defined(FileKind::Rust, &run);

        assert_eq!(spans.len(), 40_000);
        assert!(spans.iter().take(39_999).all(|(_, span)| *span == "fn a"));
    }
}
