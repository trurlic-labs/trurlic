//! Reading an index's entry back, taking nothing on trust.

use crate::code::index::{Definition, FileIndex, FileKind, IndexFault, ModuleFile, RustFacts};
use crate::code::kinds::{DefinitionKind, Role, Visibility};
use crate::code::scope::{MAX_NESTING, Scope, ScopeId};
use crate::code::span::{Span, SpanHash};
use crate::code::symbol::Segment;

use super::{DEPTH_BYTES, Indexed, block, max_entry_bytes};

/// The index `bytes` hold for a file of `kind` of `source_bytes` bytes;
/// `None` when they are no encoding of one. A read charges each heap
/// block before it allocates it, as the allocator takes it ([`block`]),
/// against what [`max_entry_bytes`] leaves past the entry's own bytes.
/// An entry past that is no index. So every entry a write makes reads
/// back, and the entry and what its decode allocates together take no
/// more than that.
pub(crate) fn decode(bytes: &[u8], kind: FileKind, source_bytes: usize) -> Option<Indexed> {
    let mut reader = Reader::new(bytes, max_entry_bytes(kind, source_bytes))?;
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
    reader.is_done().then_some(indexed)
}

/// A reader of an entry's bytes, which charges each heap block a value
/// it reads takes against what the entry's room leaves.
pub(in crate::code) struct Reader<'b> {
    /// The bytes the decode may still allocate.
    left: usize,
    bytes: &'b [u8],
    at: usize,
}

impl<'b> Reader<'b> {
    /// A reader of `bytes`, an entry whose room is `room`: its bytes and
    /// the heap blocks its values take together; `None` when the bytes
    /// alone pass it.
    pub(in crate::code) fn new(bytes: &'b [u8], room: usize) -> Option<Self> {
        Some(Self {
            left: room.checked_sub(bytes.len())?,
            bytes,
            at: 0,
        })
    }

    /// Whether every byte is read.
    pub(in crate::code) const fn is_done(&self) -> bool {
        self.at == self.bytes.len()
    }

    pub(in crate::code) fn byte(&mut self) -> Option<u8> {
        let byte = *self.bytes.get(self.at)?;
        self.at += 1;
        Some(byte)
    }

    pub(in crate::code) fn number(&mut self) -> Option<usize> {
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

    /// Charge a heap block of `requested` bytes.
    fn own(&mut self, requested: usize) -> Option<()> {
        self.left = self.left.checked_sub(block(requested))?;
        Some(())
    }

    /// A count of values of `T`, the block of their collection charged.
    pub(in crate::code) fn items<T>(&mut self) -> Option<usize> {
        let count = self.count()?;
        self.own(count.checked_mul(size_of::<T>())?)?;
        Some(count)
    }

    fn count(&mut self) -> Option<usize> {
        let count = self.number()?;
        (count <= self.bytes.len() - self.at).then_some(count)
    }

    pub(in crate::code) fn text(&mut self) -> Option<String> {
        let length = self.count()?;
        self.own(length)?;
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

    fn role(&mut self) -> Option<Role> {
        [Role::Code, Role::Test]
            .get(usize::from(self.byte()?))
            .copied()
    }

    fn index(&mut self) -> Option<FileIndex> {
        let scopes = self.scopes()?;
        let definitions = self.definitions(scopes.len())?;
        let count = self.items::<ModuleFile>()?;
        let mut module_files = Vec::with_capacity(count);
        for _ in 0..count {
            module_files.push(self.module_file(scopes.len())?);
        }
        Some(FileIndex {
            scopes,
            definitions,
            rust: RustFacts {
                module_files,
                invoked: self.invoked()?,
            },
        })
    }

    fn module_file(&mut self, scopes: usize) -> Option<ModuleFile> {
        let scope = ScopeId::of(self.number()?).filter(|id| id.at() < scopes)?;
        let role = self.role()?;
        let path = match self.byte()? {
            0 => None,
            1 => Some(self.text()?),
            _ => return None,
        };
        Some(ModuleFile { scope, role, path })
    }

    /// The invoked names, each after the one before it.
    fn invoked(&mut self) -> Option<Vec<String>> {
        let count = self.items::<String>()?;
        let mut names: Vec<String> = Vec::with_capacity(count);
        for _ in 0..count {
            let name = self.text()?;
            if names.last().is_some_and(|last| *last >= name) {
                return None;
            }
            names.push(name);
        }
        Some(names)
    }

    /// The scopes, each parent below its child, of one segment at least,
    /// each path within [`MAX_NESTING`].
    fn scopes(&mut self) -> Option<Vec<Scope>> {
        let count = self.items::<Scope>()?;
        self.own(count.checked_mul(DEPTH_BYTES)?)?;
        let mut scopes = Vec::with_capacity(count);
        let mut depths: Vec<usize> = Vec::with_capacity(count);
        for own in 0..count {
            let parent = self.scope(own)?;
            // Every scope adds a segment: a forged entry cannot make
            // scopes of no bytes.
            let segments = self.items::<Segment>().filter(|count| *count > 0)?;
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
        let count = self.items::<Definition>()?;
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
}
