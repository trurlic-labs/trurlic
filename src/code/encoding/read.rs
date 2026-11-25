//! Reading an index's entry back, taking nothing on trust.

use std::sync::Arc;

use crate::code::index::{
    After, Definition, Element, FileIndex, HandOff, IndexFault, Invocation, ModuleFile, Receives,
    Rule, TypeArm, TypeImpl, TypeMacro,
};
use crate::code::kinds::{DefinitionKind, Role, Visibility};
use crate::code::rust::MAX_OPENING;
use crate::code::scope::{MAX_NESTING, Scope, ScopeId};
use crate::code::span::{Span, SpanHash};
use crate::code::symbol::Segment;

use super::Indexed;

/// The items an index of a file of `source_bytes` bytes may hold past one
/// per byte: an empty file's, and room besides.
const ITEMS_BESIDES: usize = 64;

/// The index `bytes` hold for a file of `source_bytes` bytes; `None` when
/// they are no encoding of one. Each item of a real index (a scope, a
/// segment, a definition, a rule, an element, a token) comes from at
/// least one byte of its file. An entry that holds more than one item per
/// byte and [`ITEMS_BESIDES`] is no index, so what a decode allocates is
/// bounded by the file the entry is read for.
pub(crate) fn decode(bytes: &[u8], source_bytes: usize) -> Option<Indexed> {
    let mut reader = Reader {
        bytes,
        at: 0,
        items: source_bytes.saturating_add(ITEMS_BESIDES),
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
        _ => return None,
    };
    (reader.at == bytes.len()).then_some(indexed)
}

struct Reader<'b> {
    /// The items the index may still hold.
    items: usize,
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

    /// A count of items each at least a byte long: never more than the
    /// bytes left, so a forged count allocates nothing past the entry.
    /// A count of items of the index, each spent from what it may hold.
    fn items(&mut self) -> Option<usize> {
        let count = self.count()?;
        self.items = self.items.checked_sub(count)?;
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
        let mut macros = Vec::new();
        for _ in 0..self.items()? {
            macros.push(self.type_macro()?);
        }
        let mut invocations = Vec::new();
        for _ in 0..self.items()? {
            invocations.push(self.invocation(scopes.len())?);
        }
        let mut module_files = Vec::new();
        for _ in 0..self.items()? {
            module_files.push(self.module_file(scopes.len())?);
        }
        Some(FileIndex {
            scopes,
            definitions,
            macros,
            invocations,
            module_files,
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
        let count = self.items()?;
        let mut scopes = Vec::with_capacity(count);
        let mut depths: Vec<usize> = Vec::with_capacity(count);
        for own in 0..count {
            let parent = self.scope(own)?;
            // Every scope adds a segment: a forged entry cannot make
            // scopes of no bytes.
            let segments = self.items().filter(|count| *count > 0)?;
            let mut name = Vec::new();
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
        let count = self.items()?;
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
        let exported = match self.byte()? {
            0 => false,
            1 => true,
            _ => return None,
        };
        let mut arms = Vec::new();
        for _ in 0..self.items()? {
            arms.push(self.type_arm()?);
        }
        let mut rules = Vec::new();
        for _ in 0..self.items()? {
            rules.push(self.rule()?);
        }
        if arms.iter().any(|arm| arm.rule >= rules.len()) {
            return None;
        }
        Some(TypeMacro {
            name,
            exported,
            arms,
            rules,
        })
    }

    fn type_arm(&mut self) -> Option<TypeArm> {
        let mut words = Vec::new();
        for _ in 0..self.items()? {
            words.push(self.text()?);
        }
        let after = match self.byte()? {
            0 => After::End,
            1 => After::Token(self.text()?),
            2 => After::Unchecked,
            _ => return None,
        };
        Some(TypeArm {
            words,
            after,
            rule: self.number()?,
        })
    }

    /// A rule, each place that receives a name an `ident` of its opening.
    fn rule(&mut self) -> Option<Rule> {
        let opening = self.elements()?;
        let whole = self.flag()?;
        let mut receives = Vec::new();
        for _ in 0..self.items()? {
            let place = self.number()?;
            if opening.get(place) != Some(&Element::Ident) {
                return None;
            }
            receives.push((place, self.receives()?));
        }
        Some(Rule {
            opening,
            whole,
            receives,
            items: self.named()?,
        })
    }

    fn receives(&mut self) -> Option<Receives> {
        let kind = match self.byte()? {
            0 => None,
            1 => Some(self.kind()?),
            _ => return None,
        };
        let mut hand_offs = Vec::new();
        for _ in 0..self.items()? {
            let macro_name = self.text()?;
            let from_crate = self.flag()?;
            let arguments = self.elements()?;
            let whole = self.flag()?;
            let place = self.number().filter(|place| *place < arguments.len())?;
            hand_offs.push(HandOff {
                macro_name,
                from_crate,
                arguments,
                whole,
                place,
            });
        }
        let mut impls = Vec::new();
        for _ in 0..self.items()? {
            impls.push(self.type_impl()?);
        }
        Some(Receives {
            kind,
            hand_offs,
            impls,
        })
    }

    fn flag(&mut self) -> Option<bool> {
        match self.byte()? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }

    /// At most [`MAX_OPENING`] elements.
    fn elements(&mut self) -> Option<Vec<Element>> {
        let count = self.items().filter(|count| *count <= MAX_OPENING)?;
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
                            let mut tokens = Vec::new();
                            for _ in 0..self.items()? {
                                tokens.push(self.text()?);
                            }
                            Some(tokens)
                        }
                    };
                    Element::Group(open, tokens)
                }
                3 => Element::Ident,
                4 => Element::Tree,
                5 => Element::Variable,
                6 => Element::Many,
                _ => return None,
            });
        }
        Some(elements)
    }

    fn type_impl(&mut self) -> Option<TypeImpl> {
        let trait_tokens = match self.byte()? {
            0 => None,
            1 => {
                let mut tokens = Vec::new();
                for _ in 0..self.items()? {
                    tokens.push(self.text()?);
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
        let mut named = Vec::new();
        for _ in 0..self.items()? {
            named.push((Arc::new(self.segment()?), self.kind()?));
        }
        Some(named)
    }

    fn invocation(&mut self, scopes: usize) -> Option<Invocation> {
        let macro_name = self.text()?;
        let container = self.scope(scopes)?;
        let (span, hash) = self.span()?;
        let mut tokens = Vec::new();
        for _ in 0..self.items()? {
            tokens.push(self.text()?);
        }
        let role = [Role::Code, Role::Test]
            .get(usize::from(self.byte()?))
            .copied()?;
        Some(Invocation {
            macro_name,
            container,
            span,
            hash,
            tokens,
            role,
        })
    }
}
