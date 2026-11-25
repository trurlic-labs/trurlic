//! Reading an index's entry back, taking nothing on trust.

use std::sync::Arc;

use crate::code::index::{
    Definition, Element, FileIndex, FileKind, Fragment, HandOff, IndexFault, Invocation,
    ModuleFile, Receives, Rule, RustFacts, TraitToken, TypeImpl, TypeMacro,
};
use crate::code::kinds::{DefinitionKind, Role, Visibility};
use crate::code::rust::{MAX_GROUP_TOKENS, MAX_OPENING};
use crate::code::scope::{MAX_NESTING, Scope, ScopeId};
use crate::code::span::{Span, SpanHash};
use crate::code::symbol::Segment;

use super::{Indexed, max_entry_bytes, weight};

/// The index `bytes` hold for a file of `kind` of `source_bytes` bytes;
/// `None` when they are no encoding of one. A read charges an entry as
/// the scope tree charged its parts when the index was built: its bytes,
/// and the [`weight`] of each value, before it allocates the value. An
/// entry past the room of the file it is read for is no index. So every
/// entry a write makes reads back, and the entry and what its decode
/// allocates together take no more than that room.
pub(crate) fn decode(bytes: &[u8], kind: FileKind, source_bytes: usize) -> Option<Indexed> {
    let mut reader = Reader {
        bytes,
        at: 0,
        weight: max_entry_bytes(kind, source_bytes).checked_sub(bytes.len())?,
    };
    let indexed = match reader.byte()? {
        0 => Ok(reader.index()?),
        1 => Err(IndexFault::NotUtf8 {
            at: reader.number()?,
        }),
        2 => Err(IndexFault::Syntax {
            at: reader.number()?,
        }),
        3 => Err(IndexFault::TooComplex),
        4 => Err(IndexFault::TooDeep),
        5 => Err(IndexFault::Grammar),
        6 => Err(IndexFault::TooLarge {
            bytes: reader.number()?,
            max: reader.number()?,
        }),
        7 => Err(IndexFault::TooManyIndentations {
            max: reader.number()?,
        }),
        8 => Err(IndexFault::TooManyContinuations {
            max: reader.number()?,
        }),
        _ => return None,
    };
    (reader.at == bytes.len()).then_some(indexed)
}

struct Reader<'b> {
    /// The weight of the values the index may still hold.
    weight: usize,
    bytes: &'b [u8],
    at: usize,
}

impl Reader<'_> {
    fn byte(&mut self) -> Option<u8> {
        let byte = *self.bytes.get(self.at)?;
        self.at += 1;
        Some(byte)
    }

    fn number(&mut self) -> Option<usize> {
        let mut number: u64 = 0;
        for shift in (0..64).step_by(7) {
            let byte = self.byte()?;
            number |= u64::from(byte & 0x7f).checked_shl(shift)?;
            if byte & 0x80 == 0 {
                return usize::try_from(number).ok();
            }
        }
        None
    }

    /// A count of values of the index of `weight` each, charged.
    fn items(&mut self, weight: usize) -> Option<usize> {
        let count = self.count()?;
        self.weight = self.weight.checked_sub(count.checked_mul(weight)?)?;
        Some(count)
    }

    fn count(&mut self) -> Option<usize> {
        let count = self.number()?;
        (count <= self.bytes.len() - self.at).then_some(count)
    }

    fn text(&mut self) -> Option<String> {
        let length = self.count()?;
        let bytes = self.bytes.get(self.at..self.at + length)?;
        self.at += length;
        String::from_utf8(bytes.to_vec()).ok()
    }

    /// An optional scope below `bound`: 0 for none.
    fn scope(&mut self, bound: usize) -> Option<Option<ScopeId>> {
        match self.number()? {
            0 => Some(None),
            id if id <= bound => ScopeId::of(id - 1).map(Some),
            _ => None,
        }
    }

    fn span(&mut self) -> Option<(Span, SpanHash)> {
        let start = self.number()?;
        let end = start.checked_add(self.number()?)?;
        let first_line = self.number()?;
        let last_line = first_line.checked_add(self.number()?)?;
        let hash: [u8; 32] = self.bytes.get(self.at..self.at + 32)?.try_into().ok()?;
        self.at += 32;
        let span = Span {
            start,
            end,
            first_line,
            last_line,
        };
        Some((span, SpanHash::from_bytes(hash)))
    }

    fn kind(&mut self) -> Option<DefinitionKind> {
        DefinitionKind::ALL.get(usize::from(self.byte()?)).copied()
    }

    fn index(&mut self) -> Option<FileIndex> {
        let scopes = self.scopes()?;
        let definitions = self.definitions(scopes.len())?;
        let count = self.items(weight::MACRO_PART)?;
        let mut macros = Vec::with_capacity(count);
        for _ in 0..count {
            macros.push(self.type_macro()?);
        }
        let count = self.items(weight::INVOCATION)?;
        let mut invocations = Vec::with_capacity(count);
        for _ in 0..count {
            invocations.push(self.invocation(scopes.len())?);
        }
        let count = self.items(weight::MODULE_FILE)?;
        let mut module_files = Vec::with_capacity(count);
        for _ in 0..count {
            module_files.push(self.module_file(scopes.len())?);
        }
        Some(FileIndex {
            scopes,
            definitions,
            rust: RustFacts {
                macros,
                invocations,
                module_files,
            },
        })
    }

    fn module_file(&mut self, scopes: usize) -> Option<ModuleFile> {
        let scope = ScopeId::of(self.number()?).filter(|id| id.at() < scopes)?;
        let role = [Role::Code, Role::Test]
            .get(usize::from(self.byte()?))
            .copied()?;
        let path = match self.byte()? {
            0 => None,
            1 => Some(self.text()?),
            _ => return None,
        };
        Some(ModuleFile { scope, role, path })
    }

    /// The scopes, each parent below its child, of one segment at least,
    /// each path within [`MAX_NESTING`].
    fn scopes(&mut self) -> Option<Vec<Scope>> {
        let count = self.items(weight::SCOPE)?;
        let mut scopes = Vec::with_capacity(count);
        let mut depths: Vec<usize> = Vec::with_capacity(count);
        for own in 0..count {
            let parent = self.scope(own)?;
            // Every scope adds a segment: a forged entry cannot make
            // scopes of no bytes.
            let segments = self.items(weight::SEGMENT).filter(|count| *count > 0)?;
            let mut name = Vec::with_capacity(segments);
            for _ in 0..segments {
                name.push(self.segment()?);
            }
            let above = parent.map_or(Some(0), |id| depths.get(id.at()).copied())?;
            let depth = above + name.len();
            if depth > MAX_NESTING {
                return None;
            }
            depths.push(depth);
            scopes.push(Scope { parent, name });
        }
        Some(scopes)
    }

    fn definitions(&mut self, scopes: usize) -> Option<Vec<Definition>> {
        let count = self.items(weight::DEFINITION)?;
        let mut definitions: Vec<Definition> = Vec::with_capacity(count);
        for _ in 0..count {
            let scope = ScopeId::of(self.number()?).filter(|id| id.at() < scopes)?;
            let kind = self.kind()?;
            let marks = self.byte()?;
            let role = [Role::Code, Role::Test]
                .get(usize::from(marks & 1))
                .copied()?;
            let visibility = match marks >> 1 {
                0 => None,
                code => Some(*Visibility::ALL.get(usize::from(code) - 1)?),
            };
            let (span, hash) = match self.byte()? {
                0 => self.span()?,
                1 => definitions.last().map(|last| (last.span, last.hash))?,
                _ => return None,
            };
            definitions.push(Definition {
                scope,
                kind,
                span,
                hash,
                role,
                visibility,
            });
        }
        Some(definitions)
    }

    fn segment(&mut self) -> Option<Segment> {
        Some(match self.byte()? {
            0 => Segment::Name(self.text()?),
            1 => {
                let name = self.text()?;
                let arguments = match self.byte()? {
                    0 => None,
                    1 => Some(self.text()?),
                    _ => return None,
                };
                Segment::Impl { name, arguments }
            }
            _ => return None,
        })
    }

    fn type_macro(&mut self) -> Option<TypeMacro> {
        let name = self.text()?;
        let exported = self.flag()?;
        let count = self.items(weight::MACRO_PART)?;
        let mut rules = Vec::with_capacity(count);
        for _ in 0..count {
            rules.push(self.rule()?);
        }
        Some(TypeMacro {
            name,
            exported,
            rules,
        })
    }

    /// A rule, each place that receives a name a binding of an `ident` or
    /// a `tt` of its opening, each place an argument forwards a binding of
    /// it.
    fn rule(&mut self) -> Option<Rule> {
        let opening = self.elements()?;
        let whole = self.flag()?;
        let binds = |place: usize| matches!(opening.get(place), Some(Element::Binding(_)));
        let count = self.items(weight::MACRO_PART)?;
        let mut receives = Vec::with_capacity(count);
        for _ in 0..count {
            let place = self.number()?;
            let takes_a_name = matches!(
                opening.get(place),
                Some(Element::Binding(Fragment::Ident | Fragment::Tree))
            );
            if !takes_a_name {
                return None;
            }
            receives.push((place, self.receives()?));
        }
        let count = self.items(weight::MACRO_PART)?;
        let mut hand_offs = Vec::with_capacity(count);
        for _ in 0..count {
            let hand_off = self.hand_off()?;
            let bound = hand_off.arguments.iter().all(
                |argument| !matches!(argument, Element::Forwarded(Some(at), _) if !binds(*at)),
            );
            if !bound {
                return None;
            }
            hand_offs.push(hand_off);
        }
        Some(Rule {
            opening,
            whole,
            receives,
            hand_offs,
            items: self.named()?,
        })
    }

    fn receives(&mut self) -> Option<Receives> {
        let kind = match self.byte()? {
            0 => None,
            1 => Some(self.kind()?),
            _ => return None,
        };
        let count = self.items(weight::MACRO_PART)?;
        let mut impls = Vec::with_capacity(count);
        for _ in 0..count {
            impls.push(self.type_impl()?);
        }
        Some(Receives { kind, impls })
    }

    fn hand_off(&mut self) -> Option<HandOff> {
        Some(HandOff {
            macro_name: self.text()?,
            from_crate: self.flag()?,
            arguments: self.elements()?,
            whole: self.flag()?,
        })
    }

    fn flag(&mut self) -> Option<bool> {
        match self.byte()? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }

    fn fragment(&mut self) -> Option<Fragment> {
        Fragment::ALL.get(usize::from(self.byte()?)).copied()
    }

    /// At most [`MAX_OPENING`] elements, a group's tokens at most
    /// [`MAX_GROUP_TOKENS`].
    fn elements(&mut self) -> Option<Vec<Element>> {
        let count = self
            .items(weight::ELEMENT)
            .filter(|count| *count <= MAX_OPENING)?;
        let mut elements = Vec::with_capacity(count);
        for _ in 0..count {
            elements.push(match self.byte()? {
                0 => Element::Word(self.text()?),
                1 => Element::Token(self.text()?),
                2 => {
                    let open = char::from_u32(u32::try_from(self.number()?).ok()?)?;
                    let tokens = match self.flag()? {
                        false => None,
                        true => {
                            let count = self
                                .items(weight::ELEMENT)
                                .filter(|count| *count <= MAX_GROUP_TOKENS)?;
                            let mut tokens = Vec::with_capacity(count);
                            for _ in 0..count {
                                tokens.push(self.text()?);
                            }
                            Some(tokens)
                        }
                    };
                    Element::Group(open, tokens)
                }
                3 => Element::Binding(self.fragment()?),
                4 => {
                    let place = self.number()?.checked_sub(1);
                    Element::Forwarded(place, self.fragment()?)
                }
                5 => Element::Many,
                _ => return None,
            });
        }
        Some(elements)
    }

    fn type_impl(&mut self) -> Option<TypeImpl> {
        let trait_tokens = match self.byte()? {
            0 => None,
            1 => {
                let count = self.items(weight::MACRO_PART)?;
                let mut tokens = Vec::with_capacity(count);
                for _ in 0..count {
                    tokens.push(match self.byte()? {
                        0 => TraitToken::Text(self.text()?),
                        1 => TraitToken::Name,
                        _ => return None,
                    });
                }
                Some(tokens)
            }
            _ => return None,
        };
        Some(TypeImpl {
            trait_tokens,
            members: self.named()?,
        })
    }

    /// Names a macro gives, each with its kind.
    fn named(&mut self) -> Option<Vec<(Arc<Segment>, DefinitionKind)>> {
        let count = self.items(weight::MACRO_PART)?;
        let mut named = Vec::with_capacity(count);
        for _ in 0..count {
            named.push((Arc::new(self.segment()?), self.kind()?));
        }
        Some(named)
    }

    fn invocation(&mut self, scopes: usize) -> Option<Invocation> {
        let macro_name = self.text()?;
        let container = self.scope(scopes)?;
        let (span, hash) = self.span()?;
        let arguments = self.elements()?;
        let whole = self.flag()?;
        let role = [Role::Code, Role::Test]
            .get(usize::from(self.byte()?))
            .copied()?;
        Some(Invocation {
            macro_name,
            container,
            span,
            hash,
            arguments,
            whole,
            role,
        })
    }
}
