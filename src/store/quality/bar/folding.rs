//! What translating a pattern's syntax tree will cost, counted before it is
//! translated. Translation folds the case of each class under a
//! case-insensitive flag code point by code point, which a short pattern
//! can make long: `(?i)[\s\S]` folds every code point. Everything else it
//! does is linear in the pattern's text or bounded by the size of its NFA.

use regex_syntax::ast::{self, Ast, ClassSetItem, Flag, GroupKind, Visitor};
use regex_syntax::hir::{Class, HirKind};

/// The Unicode scalar values: every code point but the surrogates.
const SCALAR_VALUES: u64 = 0x11_0000 - 0x800;

/// An upper bound on the code points translating `ast`, the syntax tree of
/// `pattern`, folds: none when no flag of it is case-insensitive, else the
/// span of each class it folds. A bracketed class folds what it holds once
/// for each bracket around it, and a Unicode class its own span besides;
/// a Perl class outside brackets folds nothing, since it is closed under
/// case folding.
pub(super) fn folded_code_points(pattern: &str, ast: &Ast) -> u64 {
    let counted = ast::visit(
        ast,
        Folding {
            pattern,
            insensitive: false,
            depth: 0,
            code_points: 0,
        },
    );
    match counted {
        Ok(folded) => folded,
        // The visitor refuses nothing.
        Err(never) => match never {},
    }
}

/// The count over one syntax tree.
struct Folding<'p> {
    pattern: &'p str,
    insensitive: bool,
    /// The brackets around the class item being visited.
    depth: u64,
    code_points: u64,
}

impl Folding<'_> {
    /// The code points of the class `span` spells, as written: negated
    /// when it is. Every scalar value when it does not parse alone.
    fn span_of(&self, span: &ast::Span) -> u64 {
        let text = self.pattern.get(span.start.offset..span.end.offset);
        let parsed = text.and_then(|text| regex_syntax::Parser::new().parse(text).ok());
        match parsed.as_ref().map(regex_syntax::hir::Hir::kind) {
            Some(HirKind::Class(Class::Unicode(class))) => class
                .ranges()
                .iter()
                .map(|range| u64::from(range.end()) - u64::from(range.start()) + 1)
                .sum(),
            // No class of code points alone: counted as every one.
            _ => SCALAR_VALUES,
        }
    }

    /// The code points of Unicode class `class` before its negation: what
    /// translation folds of it.
    fn positive(&self, class: &ast::ClassUnicode) -> u64 {
        let span = self.span_of(&class.span);
        if class.negated {
            SCALAR_VALUES.saturating_sub(span)
        } else {
            span
        }
    }

    fn add(&mut self, code_points: u64) {
        self.code_points = self.code_points.saturating_add(code_points);
    }

    /// Whether `flags` turn case insensitivity on.
    fn sets_insensitive(&mut self, flags: &ast::Flags) {
        self.insensitive |= flags.flag_state(Flag::CaseInsensitive) == Some(true);
    }
}

impl Visitor for Folding<'_> {
    type Output = u64;
    type Err = std::convert::Infallible;

    fn finish(self) -> Result<u64, Self::Err> {
        Ok(if self.insensitive {
            self.code_points
        } else {
            0
        })
    }

    fn visit_pre(&mut self, ast: &Ast) -> Result<(), Self::Err> {
        match ast {
            Ast::Flags(set) => self.sets_insensitive(&set.flags),
            Ast::Group(group) => {
                if let GroupKind::NonCapturing(flags) = &group.kind {
                    self.sets_insensitive(flags);
                }
            }
            Ast::ClassUnicode(class) => {
                let positive = self.positive(class);
                self.add(positive);
            }
            Ast::ClassBracketed(_) => self.depth += 1,
            // Nothing else folds a class: a literal folds its own few code
            // points, and a Perl class is closed under folding.
            Ast::Empty(_)
            | Ast::Literal(_)
            | Ast::Dot(_)
            | Ast::Assertion(_)
            | Ast::ClassPerl(_)
            | Ast::Repetition(_)
            | Ast::Alternation(_)
            | Ast::Concat(_) => {}
        }
        Ok(())
    }

    fn visit_post(&mut self, ast: &Ast) -> Result<(), Self::Err> {
        if let Ast::ClassBracketed(_) = ast {
            self.depth -= 1;
        }
        Ok(())
    }

    fn visit_class_set_item_pre(&mut self, item: &ClassSetItem) -> Result<(), Self::Err> {
        let held = match item {
            ClassSetItem::Literal(_) => 1,
            ClassSetItem::Range(range) => u64::from(range.end.c) - u64::from(range.start.c) + 1,
            // An ASCII class holds ASCII code points alone.
            ClassSetItem::Ascii(_) => 128,
            ClassSetItem::Perl(class) => self.span_of(&class.span),
            ClassSetItem::Unicode(class) => {
                // Folded on its own first, before its negation.
                let positive = self.positive(class);
                self.add(positive);
                self.span_of(&class.span)
            }
            ClassSetItem::Bracketed(nested) => {
                self.depth += 1;
                // What it holds is counted item by item; negated, the
                // bracket around it holds nearly every scalar value.
                if nested.negated { SCALAR_VALUES } else { 0 }
            }
            ClassSetItem::Empty(_) | ClassSetItem::Union(_) => 0,
        };
        self.add(held.saturating_mul(self.depth));
        Ok(())
    }

    fn visit_class_set_item_post(&mut self, item: &ClassSetItem) -> Result<(), Self::Err> {
        if let ClassSetItem::Bracketed(_) = item {
            self.depth -= 1;
        }
        Ok(())
    }
}
