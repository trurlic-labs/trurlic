//! Reading an index's entry back, taking nothing on trust.

use crate::code::index::file::{
    Definition, Doc, FileIndex, FileKind, IndexFault, Indexed, ModuleFile, RustFacts,
};
use crate::code::index::import::{Before, Binds, Import, ImportKind, PathStep};
use crate::code::index::kinds::{DefinitionKind, Role, SelfType, Visibility};
use crate::code::index::manifest::{
    Dependency, DependencyKind, DependencySource, Manifest, Package, Workspace, WorkspaceDependency,
};
use crate::code::index::scope::{MAX_NESTING, Scope, ScopeId};
use crate::code::index::span::{Span, SpanHash};
use crate::code::index::symbol::Segment;

use super::room::{DEPTH_BYTES, block, max_entry_bytes};

/// The index `bytes` hold for a file of `kind` of `source_bytes` bytes;
/// `None` when they are no encoding of one, or an index of a file past
/// its kind's size, which has a fault alone. A read charges each heap
/// block before it allocates it, as the allocator takes it ([`block`]),
/// against what [`max_entry_bytes`] leaves past the entry's own bytes.
/// An entry past that is no index. So every entry a write makes reads
/// back, and the entry and what its decode allocates together take no
/// more than that.
pub(crate) fn decode(bytes: &[u8], kind: FileKind, source_bytes: usize) -> Option<Indexed> {
    let mut reader = Reader::new(bytes, max_entry_bytes(kind, source_bytes))?;
    let indexed = match reader.byte()? {
        0 if source_bytes <= kind.max_bytes() => Ok(reader.index()?),
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
        _ => return None,
    };
    reader.is_done().then_some(indexed)
}

/// The role whose code is `code`, as [`role_code`](super::codes::role_code)
/// writes it.
fn role_of(code: u8) -> Option<Role> {
    [Role::Code, Role::Test].get(usize::from(code)).copied()
}

/// The self type of a definition of `kind` whose code is `code`, as
/// [`self_type_code`](super::codes::self_type_code) writes it: one for a
/// trait impl, none for every other kind; `None` for a code that breaks
/// that.
fn self_type_of(kind: DefinitionKind, code: u8) -> Option<Option<SelfType>> {
    let self_type = match code {
        0 => None,
        code => Some(*SelfType::ALL.get(usize::from(code) - 1)?),
    };
    (self_type.is_some() == (kind == DefinitionKind::Impl)).then_some(self_type)
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

    fn index(&mut self) -> Option<FileIndex> {
        let scopes = self.scopes()?;
        let definitions = self.definitions(scopes.len())?;
        let rust = self.rust_facts(scopes.len())?;
        let code = self.byte()?;
        let doc = self.doc(code)?;
        let manifest = match self.byte()? {
            0 => None,
            1 => {
                self.own(size_of::<Manifest>())?;
                Some(Box::new(self.manifest()?))
            }
            _ => return None,
        };
        Some(FileIndex {
            scopes,
            definitions,
            doc,
            rust,
            manifest,
        })
    }

    /// What a Rust file holds besides its definitions, in an index of
    /// `scopes` scopes: its module files, its steps, each after a step
    /// below it, its imports, each ending at one of them, and its
    /// modules that hold a macro invocation as an item.
    fn rust_facts(&mut self, scopes: usize) -> Option<RustFacts> {
        let count = self.items::<ModuleFile>()?;
        let mut module_files = Vec::with_capacity(count);
        for _ in 0..count {
            module_files.push(self.module_file(scopes)?);
        }
        let count = self.items::<PathStep>()?;
        let mut steps = Vec::with_capacity(count);
        for own in 0..count {
            steps.push(self.step(scopes, own)?);
        }
        let count = self.items::<Import>()?;
        let mut imports = Vec::with_capacity(count);
        for _ in 0..count {
            imports.push(self.import(steps.len())?);
        }
        let count = self.items::<Option<ScopeId>>()?;
        let mut macro_modules = Vec::with_capacity(count);
        for _ in 0..count {
            let module = self.scope(scopes)?;
            // Each once, in order, as the walk writes them.
            if macro_modules.last().is_some_and(|last| *last >= module) {
                return None;
            }
            macro_modules.push(module);
        }
        Some(RustFacts {
            module_files,
            steps,
            imports,
            macro_modules,
        })
    }

    /// The step at `own`, in an index of `scopes` scopes.
    fn step(&mut self, scopes: usize, own: usize) -> Option<PathStep> {
        let before = match self.byte()? {
            0 => Before::Start(self.scope(scopes)?),
            1 => Before::Step(self.number().filter(|at| *at < own)?),
            _ => return None,
        };
        let segment = self.text()?;
        Some(PathStep { before, segment })
    }

    /// An import whose last step is one of `steps`.
    fn import(&mut self, steps: usize) -> Option<Import> {
        let last = self.number().filter(|at| *at < steps)?;
        let marks = self.byte()?;
        let role = role_of(marks & 1)?;
        let kind = match marks >> 1 {
            0 => ImportKind::Code,
            1 => ImportKind::Use(self.binds()?),
            2 => ImportKind::Reexport(self.binds()?),
            3 => ImportKind::Unresolvable,
            _ => return None,
        };
        Some(Import { last, role, kind })
    }

    fn binds(&mut self) -> Option<Binds> {
        Some(match self.byte()? {
            0 => Binds::Last,
            1 => Binds::Alias(self.text()?),
            2 => Binds::Nothing,
            3 => Binds::Glob,
            _ => return None,
        })
    }

    /// The doc whose code, as [`doc_code`](super::codes::doc_code) writes
    /// it, is `code`: `Some(None)` for none, `None` for no code or a text
    /// no [`Doc`] holds.
    fn doc(&mut self, code: u8) -> Option<Option<Doc>> {
        let cut = match code {
            0 => return Some(None),
            1 => false,
            2 => true,
            _ => return None,
        };
        Doc::new(self.text()?, cut).map(Some)
    }

    fn module_file(&mut self, scopes: usize) -> Option<ModuleFile> {
        let scope = ScopeId::of(self.number()?).filter(|id| id.at() < scopes)?;
        let role = role_of(self.byte()?)?;
        let path = self.optional_text()?;
        Some(ModuleFile { scope, role, path })
    }

    /// A manifest, with a package or a workspace or both.
    fn manifest(&mut self) -> Option<Manifest> {
        let package = match self.byte()? {
            0 => None,
            1 => Some(self.package()?),
            _ => return None,
        };
        let workspace = match self.byte()? {
            0 => None,
            1 => Some(self.workspace()?),
            _ => return None,
        };
        (package.is_some() || workspace.is_some()).then_some(Manifest { package, workspace })
    }

    fn package(&mut self) -> Option<Package> {
        let name = self.optional_text()?;
        let workspace = self.optional_text()?;
        let count = self.items::<Dependency>()?;
        let mut dependencies = Vec::with_capacity(count);
        for _ in 0..count {
            let kind = *DependencyKind::ALL.get(usize::from(self.byte()?))?;
            let source = match self.byte()? {
                0 => DependencySource::Path(self.text()?),
                1 => DependencySource::Workspace(self.text()?),
                _ => return None,
            };
            dependencies.push(Dependency { kind, source });
        }
        Some(Package {
            name,
            workspace,
            dependencies,
        })
    }

    fn workspace(&mut self) -> Option<Workspace> {
        let count = self.items::<WorkspaceDependency>()?;
        let mut dependencies = Vec::with_capacity(count);
        for _ in 0..count {
            dependencies.push(WorkspaceDependency {
                name: self.text()?,
                path: self.optional_text()?,
            });
        }
        let count = self.items::<String>()?;
        let mut exclude = Vec::with_capacity(count);
        for _ in 0..count {
            exclude.push(self.text()?);
        }
        Some(Workspace {
            dependencies,
            exclude,
        })
    }

    /// An optional text: a byte 0 for none, else 1 and the text.
    fn optional_text(&mut self) -> Option<Option<String>> {
        match self.byte()? {
            0 => Some(None),
            1 => self.text().map(Some),
            _ => None,
        }
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
            let role = role_of(marks & 1)?;
            let visibility = match (marks >> 1) & 0b111 {
                0 => None,
                code => Some(*Visibility::ALL.get(usize::from(code) - 1)?),
            };
            let (span, hash) = match self.byte()? {
                0 => self.span()?,
                1 => definitions.last().map(|last| (last.span, last.hash))?,
                _ => return None,
            };
            let self_type = self_type_of(kind, marks >> 6)?;
            let doc = self.doc((marks >> 4) & 0b11)?;
            definitions.push(Definition {
                scope,
                kind,
                span,
                hash,
                role,
                visibility,
                doc,
                self_type,
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
