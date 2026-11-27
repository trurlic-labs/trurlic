//! Writing an index's entry, and measuring what each part of it costs.

use crate::code::index::file::{Definition, FileIndex, IndexFault, Indexed, ModuleFile};
use crate::code::index::manifest::{
    Dependency, DependencySource, Manifest, Package, WorkspaceDependency,
};
use crate::code::index::scope::{Scope, ScopeId};
use crate::code::index::span::{Span, SpanHash};
use crate::code::index::symbol::Segment;

use super::codes::{dependency_kind_code, kind_code, role_code, visibility_code};
use super::room::{DEPTH_BYTES, block};

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
    }
    out.0
}

/// What each part of an index costs in an entry, as [`encode`] writes it
/// and a read charges it: its bytes, its slot in its collection, and each
/// heap block it owns as the allocator takes it ([`block`]). A scope adds
/// the depth a read keeps beside it.
pub(in crate::code) struct Measure;

impl Measure {
    pub(in crate::code::index) fn scope(parent: Option<ScopeId>, name: &[Segment]) -> usize {
        Self::of(size_of::<Scope>() + DEPTH_BYTES, |out| {
            out.scope_entry(parent, name);
        })
    }

    /// `definition`, which comes after one of span `previous` in the entry.
    pub(in crate::code::index) fn definition(
        definition: &Definition,
        previous: Option<Span>,
    ) -> usize {
        Self::of(size_of::<Definition>(), |out| {
            out.definition(definition, previous);
        })
    }

    pub(in crate::code::index) fn module_file(module: &ModuleFile) -> usize {
        Self::of(size_of::<ModuleFile>(), |out| out.module_file(module))
    }

    /// `manifest`, each of its dependencies and excluded paths a value of
    /// its collection.
    pub(in crate::code::index) fn manifest(manifest: &Manifest) -> usize {
        let dependencies = manifest
            .package
            .as_ref()
            .map_or(0, |package| package.dependencies.len());
        let workspace = manifest.workspace.as_ref();
        let inherited = workspace.map_or(0, |workspace| workspace.dependencies.len());
        let excluded = workspace.map_or(0, |workspace| workspace.exclude.len());
        let slots = dependencies
            .saturating_mul(size_of::<Dependency>())
            .saturating_add(inherited.saturating_mul(size_of::<WorkspaceDependency>()))
            .saturating_add(excluded.saturating_mul(size_of::<String>()));
        Self::of(slots, |out| out.manifest(manifest))
    }

    /// The cost of the part `write` writes, a value of `slot` bytes in its
    /// collection.
    fn of(slot: usize, write: impl FnOnce(&mut Writer<Cost>)) -> usize {
        let mut out = Writer(Cost(slot));
        write(&mut out);
        out.0.0
    }
}

/// Where a [`Writer`] puts its bytes: an entry, or their cost.
pub(in crate::code) trait Sink {
    fn put(&mut self, bytes: &[u8]);

    /// Note a heap block of `requested` bytes the value written owns.
    fn own(&mut self, requested: usize);
}

impl Sink for Vec<u8> {
    fn put(&mut self, bytes: &[u8]) {
        self.extend_from_slice(bytes);
    }

    fn own(&mut self, _requested: usize) {}
}

/// The cost of what a writer would write: its bytes, and each block.
struct Cost(usize);

impl Sink for Cost {
    fn put(&mut self, bytes: &[u8]) {
        self.0 = self.0.saturating_add(bytes.len());
    }

    fn own(&mut self, requested: usize) {
        self.0 = self.0.saturating_add(block(requested));
    }
}

/// A writer of an entry's bytes, or of their cost.
pub(in crate::code) struct Writer<S>(S);

impl Writer<Vec<u8>> {
    /// A writer of an entry.
    pub(in crate::code) const fn entry() -> Self {
        Self(Vec::new())
    }

    /// The bytes written.
    pub(in crate::code) fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

impl<S: Sink> Writer<S> {
    pub(in crate::code) fn byte(&mut self, byte: u8) {
        self.0.put(&[byte]);
    }

    pub(in crate::code) fn number(&mut self, mut number: usize) {
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

    /// A text: its length and its bytes, which a read holds in a block.
    pub(in crate::code) fn text(&mut self, text: &str) {
        self.number(text.len());
        self.0.put(text.as_bytes());
        self.0.own(text.len());
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
        self.number(index.rust.module_files.len());
        for module in &index.rust.module_files {
            self.module_file(module);
        }
        match &index.manifest {
            None => self.byte(0),
            Some(manifest) => {
                self.byte(1);
                self.manifest(manifest);
            }
        }
    }

    /// A manifest, in the block of its box: its package and its
    /// workspace, each a byte 0 when it has none.
    fn manifest(&mut self, manifest: &Manifest) {
        self.0.own(size_of::<Manifest>());
        match &manifest.package {
            None => self.byte(0),
            Some(package) => {
                self.byte(1);
                self.package(package);
            }
        }
        match &manifest.workspace {
            None => self.byte(0),
            Some(workspace) => {
                self.byte(1);
                self.number(workspace.dependencies.len());
                for dependency in &workspace.dependencies {
                    self.text(&dependency.name);
                    self.optional_text(dependency.path.as_deref());
                }
                self.number(workspace.exclude.len());
                for path in &workspace.exclude {
                    self.text(path);
                }
            }
        }
    }

    fn package(&mut self, package: &Package) {
        self.optional_text(package.name.as_deref());
        self.optional_text(package.workspace.as_deref());
        self.number(package.dependencies.len());
        for dependency in &package.dependencies {
            self.byte(dependency_kind_code(dependency.kind));
            match &dependency.source {
                DependencySource::Path(path) => {
                    self.byte(0);
                    self.text(path);
                }
                DependencySource::Workspace(name) => {
                    self.byte(1);
                    self.text(name);
                }
            }
        }
    }

    /// An optional text: a byte 0 for none, else 1 and the text.
    fn optional_text(&mut self, text: Option<&str>) {
        match text {
            None => self.byte(0),
            Some(text) => {
                self.byte(1);
                self.text(text);
            }
        }
    }

    /// A scope, whose segments a read holds in a block of their own.
    fn scope_entry(&mut self, parent: Option<ScopeId>, name: &[Segment]) {
        self.scope(parent);
        self.number(name.len());
        self.0.own(name.len().saturating_mul(size_of::<Segment>()));
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
        self.optional_text(module.path.as_deref());
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
}
