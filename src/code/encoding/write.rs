//! Writing an index's entry, and measuring what each part of it takes.

use std::sync::Arc;

use crate::code::index::{
    Definition, Element, FileIndex, Fragment, HandOff, IndexFault, Invocation, ModuleFile,
    Receives, TraitToken, TypeMacro,
};
use crate::code::kinds::DefinitionKind;
use crate::code::scope::ScopeId;
use crate::code::span::{Span, SpanHash};
use crate::code::symbol::Segment;

use super::{Indexed, kind_code, role_code, visibility_code, weight};

/// The bytes of `indexed`.
pub(crate) fn encode(indexed: &Indexed) -> Vec<u8> {
    let mut out = Writer(Vec::new());
    match indexed {
        Ok(index) => {
            out.byte(0);
            out.index(index);
        }
        Err(IndexFault::NotUtf8 { at }) => {
            out.byte(1);
            out.number(*at);
        }
        Err(IndexFault::Syntax { at }) => {
            out.byte(2);
            out.number(*at);
        }
        Err(IndexFault::TooComplex) => out.byte(3),
        Err(IndexFault::TooDeep) => out.byte(4),
        Err(IndexFault::Grammar) => out.byte(5),
        Err(IndexFault::TooLarge { bytes, max }) => {
            out.byte(6);
            out.number(*bytes);
            out.number(*max);
        }
        Err(IndexFault::TooManyIndentations { max }) => {
            out.byte(7);
            out.number(*max);
        }
        Err(IndexFault::TooManyContinuations { max }) => {
            out.byte(8);
            out.number(*max);
        }
    }
    out.0
}

/// What each part of an index costs in an entry, as [`encode`] writes it
/// and a read charges it: its bytes, and the [`weight`] of itself and of
/// each value it holds. A definition's span is counted whole, shared or
/// not.
pub(in crate::code) struct Measure;

impl Measure {
    pub(in crate::code) fn scope(parent: Option<ScopeId>, name: &[Segment]) -> usize {
        Self::of(weight::SCOPE, |out| out.scope_entry(parent, name))
    }

    pub(in crate::code) fn definition(definition: &Definition) -> usize {
        Self::of(weight::DEFINITION, |out| out.definition(definition, None))
    }

    pub(in crate::code) fn type_macro(found: &TypeMacro) -> usize {
        Self::of(weight::MACRO_PART, |out| out.type_macro(found))
    }

    pub(in crate::code) fn invocation(invocation: &Invocation) -> usize {
        Self::of(weight::INVOCATION, |out| out.invocation(invocation))
    }

    pub(in crate::code) fn module_file(module: &ModuleFile) -> usize {
        Self::of(weight::MODULE_FILE, |out| out.module_file(module))
    }

    /// The cost of the part `write` writes, a value of `weight` in its
    /// collection.
    fn of(weight: usize, write: impl FnOnce(&mut Writer<Cost>)) -> usize {
        let mut out = Writer(Cost(weight));
        write(&mut out);
        out.0.0
    }
}

/// Where a [`Writer`] puts its bytes: an entry, or their cost.
trait Sink {
    fn put(&mut self, bytes: &[u8]);

    /// Note `count` values of `weight` each.
    fn weigh(&mut self, count: usize, weight: usize);
}

impl Sink for Vec<u8> {
    fn put(&mut self, bytes: &[u8]) {
        self.extend_from_slice(bytes);
    }

    fn weigh(&mut self, _count: usize, _weight: usize) {}
}

/// The cost of what a writer would write: its bytes, and each value's
/// weight.
struct Cost(usize);

impl Sink for Cost {
    fn put(&mut self, bytes: &[u8]) {
        self.0 += bytes.len();
    }

    fn weigh(&mut self, count: usize, weight: usize) {
        self.0 = self.0.saturating_add(count.saturating_mul(weight));
    }
}

struct Writer<S>(S);

impl<S: Sink> Writer<S> {
    fn byte(&mut self, byte: u8) {
        self.0.put(&[byte]);
    }

    fn number(&mut self, mut number: usize) {
        loop {
            let low = u8::try_from(number & 0x7f).unwrap_or(0);
            number >>= 7;
            if number == 0 {
                self.byte(low);
                return;
            }
            self.byte(low | 0x80);
        }
    }

    /// The count of a collection of `count` values of `weight` each.
    fn count(&mut self, count: usize, weight: usize) {
        self.number(count);
        self.0.weigh(count, weight);
    }

    /// A text: its length and its bytes.
    fn text(&mut self, text: &str) {
        self.number(text.len());
        self.0.put(text.as_bytes());
    }

    /// An optional scope: 0 for none, else its id and one.
    fn scope(&mut self, scope: Option<ScopeId>) {
        self.number(scope.map_or(0, |id| id.at() + 1));
    }

    fn span(&mut self, span: Span, hash: SpanHash) {
        self.number(span.start);
        self.number(span.end - span.start);
        self.number(span.first_line);
        self.number(span.last_line - span.first_line);
        self.0.put(hash.as_bytes());
    }

    fn index(&mut self, index: &FileIndex) {
        self.count(index.scopes.len(), weight::SCOPE);
        for scope in &index.scopes {
            self.scope_entry(scope.parent, &scope.name);
        }
        self.count(index.definitions.len(), weight::DEFINITION);
        let mut previous: Option<Span> = None;
        for definition in &index.definitions {
            self.definition(definition, previous);
            previous = Some(definition.span);
        }
        self.count(index.rust.macros.len(), weight::MACRO_PART);
        for found in &index.rust.macros {
            self.type_macro(found);
        }
        self.count(index.rust.invocations.len(), weight::INVOCATION);
        for invocation in &index.rust.invocations {
            self.invocation(invocation);
        }
        self.count(index.rust.module_files.len(), weight::MODULE_FILE);
        for module in &index.rust.module_files {
            self.module_file(module);
        }
    }

    fn scope_entry(&mut self, parent: Option<ScopeId>, name: &[Segment]) {
        self.scope(parent);
        self.count(name.len(), weight::SEGMENT);
        for segment in name {
            self.segment(segment);
        }
    }

    /// A definition, its span written as one byte when it is `previous`.
    fn definition(&mut self, definition: &Definition, previous: Option<Span>) {
        self.number(definition.scope.at());
        self.byte(kind_code(definition.kind));
        self.byte(role_code(definition.role) | visibility_code(definition.visibility) << 1);
        match previous == Some(definition.span) {
            true => self.byte(1),
            false => {
                self.byte(0);
                self.span(definition.span, definition.hash);
            }
        }
    }

    fn module_file(&mut self, module: &ModuleFile) {
        self.number(module.scope.at());
        self.byte(role_code(module.role));
        match &module.path {
            None => self.byte(0),
            Some(path) => {
                self.byte(1);
                self.text(path);
            }
        }
    }

    fn segment(&mut self, segment: &Segment) {
        match segment {
            Segment::Name(name) => {
                self.byte(0);
                self.text(name);
            }
            Segment::Impl { name, arguments } => {
                self.byte(1);
                self.text(name);
                match arguments {
                    None => self.byte(0),
                    Some(arguments) => {
                        self.byte(1);
                        self.text(arguments);
                    }
                }
            }
        }
    }

    fn type_macro(&mut self, found: &TypeMacro) {
        self.text(&found.name);
        self.byte(u8::from(found.exported));
        self.count(found.rules.len(), weight::MACRO_PART);
        for rule in &found.rules {
            self.elements(&rule.opening);
            self.byte(u8::from(rule.whole));
            self.count(rule.receives.len(), weight::MACRO_PART);
            for (place, receives) in &rule.receives {
                self.number(*place);
                self.receives(receives);
            }
            self.count(rule.hand_offs.len(), weight::MACRO_PART);
            for hand_off in &rule.hand_offs {
                self.hand_off(hand_off);
            }
            self.named(&rule.items);
        }
    }

    /// Names a macro gives, each with its kind.
    fn named(&mut self, named: &[(Arc<Segment>, DefinitionKind)]) {
        self.count(named.len(), weight::MACRO_PART);
        for (name, kind) in named {
            self.segment(name);
            self.byte(kind_code(*kind));
        }
    }

    fn receives(&mut self, receives: &Receives) {
        match receives.kind {
            None => self.byte(0),
            Some(kind) => {
                self.byte(1);
                self.byte(kind_code(kind));
            }
        }
        self.count(receives.impls.len(), weight::MACRO_PART);
        for found_impl in &receives.impls {
            match &found_impl.trait_tokens {
                None => self.byte(0),
                Some(tokens) => {
                    self.byte(1);
                    self.count(tokens.len(), weight::MACRO_PART);
                    for token in tokens {
                        self.trait_token(token);
                    }
                }
            }
            self.named(&found_impl.members);
        }
    }

    fn trait_token(&mut self, token: &TraitToken) {
        match token {
            TraitToken::Text(text) => {
                self.byte(0);
                self.text(text);
            }
            TraitToken::Name => self.byte(1),
        }
    }

    fn hand_off(&mut self, hand_off: &HandOff) {
        self.text(&hand_off.macro_name);
        self.byte(u8::from(hand_off.from_crate));
        self.elements(&hand_off.arguments);
        self.byte(u8::from(hand_off.whole));
    }

    fn elements(&mut self, elements: &[Element]) {
        self.count(elements.len(), weight::ELEMENT);
        for element in elements {
            match element {
                Element::Word(word) => {
                    self.byte(0);
                    self.text(word);
                }
                Element::Token(token) => {
                    self.byte(1);
                    self.text(token);
                }
                Element::Group(open, tokens) => {
                    self.byte(2);
                    self.number(*open as usize);
                    match tokens {
                        None => self.byte(0),
                        Some(tokens) => {
                            self.byte(1);
                            self.count(tokens.len(), weight::ELEMENT);
                            for token in tokens {
                                self.text(token);
                            }
                        }
                    }
                }
                Element::Binding(fragment) => {
                    self.byte(3);
                    self.byte(fragment_code(*fragment));
                }
                Element::Forwarded(place, fragment) => {
                    self.byte(4);
                    self.number(place.map_or(0, |place| place + 1));
                    self.byte(fragment_code(*fragment));
                }
                Element::Many => self.byte(5),
            }
        }
    }

    fn invocation(&mut self, invocation: &Invocation) {
        self.text(&invocation.macro_name);
        self.scope(invocation.container);
        self.span(invocation.span, invocation.hash);
        self.elements(&invocation.arguments);
        self.byte(u8::from(invocation.whole));
        self.byte(role_code(invocation.role));
    }
}

/// A fragment's code, its place in [`Fragment::ALL`].
const fn fragment_code(fragment: Fragment) -> u8 {
    match fragment {
        Fragment::Ident => 0,
        Fragment::Tree => 1,
        Fragment::Literal => 2,
        Fragment::Lifetime => 3,
        Fragment::Other => 4,
    }
}
