//! Writing an index's entry, and measuring what each part of it takes.

use std::sync::Arc;

use crate::code::index::{
    After, Definition, Element, FileIndex, IndexFault, Invocation, ModuleFile, Receives, TypeMacro,
};
use crate::code::kinds::DefinitionKind;
use crate::code::scope::ScopeId;
use crate::code::span::{Span, SpanHash};
use crate::code::symbol::Segment;

use super::{Indexed, kind_code, role_code, visibility_code};

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
    }
    out.0
}

/// The bytes each part of an index takes in an entry, as [`encode`]
/// writes it; a definition's span is counted whole, shared or not.
pub(in crate::code) struct Measure;

impl Measure {
    pub(in crate::code) fn scope(parent: Option<ScopeId>, name: &[Segment]) -> usize {
        let mut out = Writer(Count(0));
        out.scope_entry(parent, name);
        out.0.0
    }

    pub(in crate::code) fn definition(definition: &Definition) -> usize {
        let mut out = Writer(Count(0));
        out.definition(definition, None);
        out.0.0
    }

    pub(in crate::code) fn type_macro(found: &TypeMacro) -> usize {
        let mut out = Writer(Count(0));
        out.type_macro(found);
        out.0.0
    }

    pub(in crate::code) fn invocation(invocation: &Invocation) -> usize {
        let mut out = Writer(Count(0));
        out.invocation(invocation);
        out.0.0
    }

    pub(in crate::code) fn module_file(module: &ModuleFile) -> usize {
        let mut out = Writer(Count(0));
        out.module_file(module);
        out.0.0
    }
}

/// Where a [`Writer`] puts its bytes: an entry, or a count of them.
trait Sink {
    fn put(&mut self, bytes: &[u8]);
}

impl Sink for Vec<u8> {
    fn put(&mut self, bytes: &[u8]) {
        self.extend_from_slice(bytes);
    }
}

/// A count of the bytes an entry would take.
struct Count(usize);

impl Sink for Count {
    fn put(&mut self, bytes: &[u8]) {
        self.0 += bytes.len();
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
        self.number(index.scopes.len());
        for scope in &index.scopes {
            self.scope_entry(scope.parent, &scope.name);
        }
        self.number(index.definitions.len());
        let mut previous: Option<Span> = None;
        for definition in &index.definitions {
            self.definition(definition, previous);
            previous = Some(definition.span);
        }
        self.number(index.macros.len());
        for found in &index.macros {
            self.type_macro(found);
        }
        self.number(index.invocations.len());
        for invocation in &index.invocations {
            self.invocation(invocation);
        }
        self.number(index.module_files.len());
        for module in &index.module_files {
            self.module_file(module);
        }
    }

    fn scope_entry(&mut self, parent: Option<ScopeId>, name: &[Segment]) {
        self.scope(parent);
        self.number(name.len());
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
        self.number(found.arms.len());
        for arm in &found.arms {
            self.number(arm.words.len());
            for word in &arm.words {
                self.text(word);
            }
            match &arm.after {
                After::End => self.byte(0),
                After::Token(token) => {
                    self.byte(1);
                    self.text(token);
                }
                After::Unchecked => self.byte(2),
            }
            self.number(arm.rule);
        }
        self.number(found.rules.len());
        for rule in &found.rules {
            self.elements(&rule.opening);
            self.byte(u8::from(rule.whole));
            self.number(rule.receives.len());
            for (place, receives) in &rule.receives {
                self.number(*place);
                self.receives(receives);
            }
            self.named(&rule.items);
        }
    }

    /// Names a macro gives, each with its kind.
    fn named(&mut self, named: &[(Arc<Segment>, DefinitionKind)]) {
        self.number(named.len());
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
        self.number(receives.hand_offs.len());
        for hand_off in &receives.hand_offs {
            self.text(&hand_off.macro_name);
            self.byte(u8::from(hand_off.from_crate));
            self.elements(&hand_off.arguments);
            self.byte(u8::from(hand_off.whole));
            self.number(hand_off.place);
        }
        self.number(receives.impls.len());
        for found_impl in &receives.impls {
            match &found_impl.trait_tokens {
                None => self.byte(0),
                Some(tokens) => {
                    self.byte(1);
                    self.number(tokens.len());
                    for token in tokens {
                        self.text(token);
                    }
                }
            }
            self.named(&found_impl.members);
        }
    }

    fn elements(&mut self, elements: &[Element]) {
        self.number(elements.len());
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
                            self.number(tokens.len());
                            for token in tokens {
                                self.text(token);
                            }
                        }
                    }
                }
                Element::Ident => self.byte(3),
                Element::Tree => self.byte(4),
                Element::Variable => self.byte(5),
                Element::Many => self.byte(6),
            }
        }
    }

    fn invocation(&mut self, invocation: &Invocation) {
        self.text(&invocation.macro_name);
        self.scope(invocation.container);
        self.span(invocation.span, invocation.hash);
        self.number(invocation.tokens.len());
        for token in &invocation.tokens {
            self.text(token);
        }
        self.byte(role_code(invocation.role));
    }
}
