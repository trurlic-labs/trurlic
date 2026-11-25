//! The bytes of a file's index in the cache, and back: a compact binary
//! form, whose size is bounded by the file it indexes.
//!
//! Numbers are LEB128 varints, texts a length and UTF-8 bytes, a hash its
//! 32 bytes. A definition whose span is its predecessor's (the targets of
//! one Makefile rule) keeps one byte for it. An entry is at most
//! [`MAX_ENTRY_BYTES_PER_SOURCE_BYTE`] times its file and
//! [`ENTRY_OVERHEAD_BYTES`] besides by construction: every resolver adds
//! what an entry holds through the scope tree, which charges each scope,
//! definition, macro, invocation and module file its bytes here
//! ([`room_of`], [`Measure`]) and refuses the file past its room.
//! A read takes nothing on trust: every count is bounded by the bytes left,
//! every id by what it names, every parent by its child (so a path's walk
//! ends), and a path by [`MAX_NESTING`]; bytes that break any of it are no
//! index.

use super::index::{
    Defines, Definition, FileIndex, IndexFault, Invocation, Member, ModuleFile, TypeArm, TypeMacro,
};
use super::kinds::{DefinitionKind, Role, Visibility};
use super::scope::{MAX_NESTING, Scope, ScopeId};
use super::span::{Span, SpanHash};
use super::symbol::Segment;

/// The bytes an entry may take per byte of its file: a definition of two
/// bytes (an enum variant `A,`) costs its scope, its span and its 32-byte
/// hash.
const MAX_ENTRY_BYTES_PER_SOURCE_BYTE: usize = 32;

/// The bytes an entry may take past [`MAX_ENTRY_BYTES_PER_SOURCE_BYTE`]
/// times its file: the counts of an empty index, and room besides.
const ENTRY_OVERHEAD_BYTES: usize = 64;

/// The bytes of an index's outcome tag and its five counts, each a count
/// of at most `u32::MAX` items.
const COUNTS_BYTES: usize = 1 + 5 * 5;

/// The largest entry of a file of `bytes` bytes.
pub(crate) const fn max_entry_bytes(bytes: usize) -> usize {
    bytes
        .saturating_mul(MAX_ENTRY_BYTES_PER_SOURCE_BYTE)
        .saturating_add(ENTRY_OVERHEAD_BYTES)
}

/// The bytes the scopes, definitions, macros, invocations and module
/// files of the index of a file of `bytes` bytes may take: an entry of
/// them, its counts added, is within [`max_entry_bytes`].
pub(super) const fn room_of(bytes: usize) -> usize {
    max_entry_bytes(bytes) - COUNTS_BYTES
}

/// What indexing a file's bytes gave.
pub(crate) type Indexed = Result<FileIndex, IndexFault>;

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
pub(super) struct Measure;

impl Measure {
    pub(super) fn scope(parent: Option<ScopeId>, name: &[Segment]) -> usize {
        let mut out = Writer(Count(0));
        out.scope_entry(parent, name);
        out.0.0
    }

    pub(super) fn definition(definition: &Definition) -> usize {
        let mut out = Writer(Count(0));
        out.definition(definition, None);
        out.0.0
    }

    pub(super) fn type_macro(found: &TypeMacro) -> usize {
        let mut out = Writer(Count(0));
        out.type_macro(found);
        out.0.0
    }

    pub(super) fn invocation(invocation: &Invocation) -> usize {
        let mut out = Writer(Count(0));
        out.invocation(invocation);
        out.0.0
    }

    pub(super) fn module_file(module: &ModuleFile) -> usize {
        let mut out = Writer(Count(0));
        out.module_file(module);
        out.0.0
    }
}

/// The index `bytes` hold; `None` when they are no encoding of one.
pub(crate) fn decode(bytes: &[u8]) -> Option<Indexed> {
    let mut reader = Reader { bytes, at: 0 };
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

const fn kind_code(kind: DefinitionKind) -> u8 {
    match kind {
        DefinitionKind::Module => 0,
        DefinitionKind::Struct => 1,
        DefinitionKind::Union => 2,
        DefinitionKind::Enum => 3,
        DefinitionKind::Trait => 4,
        DefinitionKind::Impl => 5,
        DefinitionKind::Function => 6,
        DefinitionKind::Method => 7,
        DefinitionKind::Field => 8,
        DefinitionKind::Variant => 9,
        DefinitionKind::Constant => 10,
        DefinitionKind::Static => 11,
        DefinitionKind::TypeAlias => 12,
        DefinitionKind::AssociatedType => 13,
        DefinitionKind::Macro => 14,
        DefinitionKind::Reexport => 15,
        DefinitionKind::Key => 16,
        DefinitionKind::Target => 17,
        DefinitionKind::Heading => 18,
    }
}

/// A visibility's code; 0 is none.
const fn visibility_code(visibility: Option<Visibility>) -> u8 {
    match visibility {
        None => 0,
        Some(Visibility::Private) => 1,
        Some(Visibility::Crate) => 2,
        Some(Visibility::Super) => 3,
        Some(Visibility::InPath) => 4,
        Some(Visibility::Public) => 5,
    }
}

const fn role_code(role: Role) -> u8 {
    match role {
        Role::Code => 0,
        Role::Test => 1,
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
            self.defines(&arm.defines);
        }
        match &found.defines {
            None => self.byte(0),
            Some(defines) => {
                self.byte(1);
                self.defines(defines);
            }
        }
        self.number(found.members.len());
        for member in &found.members {
            match &member.trait_tokens {
                None => self.byte(0),
                Some(tokens) => {
                    self.byte(1);
                    self.number(tokens.len());
                    for token in tokens {
                        self.text(token);
                    }
                }
            }
            self.text(&member.name);
            self.byte(kind_code(member.kind));
        }
        self.number(found.items.len());
        for (name, kind) in &found.items {
            self.text(name);
            self.byte(kind_code(*kind));
        }
    }

    fn defines(&mut self, defines: &Defines) {
        match defines {
            Defines::Kind(kind) => {
                self.byte(0);
                self.byte(kind_code(*kind));
            }
            Defines::HandedTo(name) => {
                self.byte(1);
                self.text(name);
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

struct Reader<'b> {
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
        for _ in 0..self.count()? {
            macros.push(self.type_macro()?);
        }
        let mut invocations = Vec::new();
        for _ in 0..self.count()? {
            invocations.push(self.invocation(scopes.len())?);
        }
        let mut module_files = Vec::new();
        for _ in 0..self.count()? {
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
        let count = self.count()?;
        let mut scopes = Vec::with_capacity(count);
        let mut depths: Vec<usize> = Vec::with_capacity(count);
        for own in 0..count {
            let parent = self.scope(own)?;
            // Every scope adds a segment: a forged entry cannot make
            // scopes of no bytes.
            let segments = self.count().filter(|count| *count > 0)?;
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
        let count = self.count()?;
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
        for _ in 0..self.count()? {
            let mut words = Vec::new();
            for _ in 0..self.count()? {
                words.push(self.text()?);
            }
            arms.push(TypeArm {
                words,
                defines: self.defines()?,
            });
        }
        let defines = match self.byte()? {
            0 => None,
            1 => Some(self.defines()?),
            _ => return None,
        };
        let mut members = Vec::new();
        for _ in 0..self.count()? {
            members.push(self.member()?);
        }
        let mut items = Vec::new();
        for _ in 0..self.count()? {
            items.push((self.text()?, self.kind()?));
        }
        Some(TypeMacro {
            name,
            exported,
            arms,
            defines,
            members,
            items,
        })
    }

    fn defines(&mut self) -> Option<Defines> {
        Some(match self.byte()? {
            0 => Defines::Kind(self.kind()?),
            1 => Defines::HandedTo(self.text()?),
            _ => return None,
        })
    }

    fn member(&mut self) -> Option<Member> {
        let trait_tokens = match self.byte()? {
            0 => None,
            1 => {
                let mut tokens = Vec::new();
                for _ in 0..self.count()? {
                    tokens.push(self.text()?);
                }
                Some(tokens)
            }
            _ => return None,
        };
        Some(Member {
            trait_tokens,
            name: self.text()?,
            kind: self.kind()?,
        })
    }

    fn invocation(&mut self, scopes: usize) -> Option<Invocation> {
        let macro_name = self.text()?;
        let container = self.scope(scopes)?;
        let (span, hash) = self.span()?;
        let mut tokens = Vec::new();
        for _ in 0..self.count()? {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::index::{FileKind, index};

    #[test]
    fn every_kind_is_coded_by_its_place() {
        for (at, kind) in DefinitionKind::ALL.iter().enumerate() {
            assert_eq!(usize::from(kind_code(*kind)), at, "{kind:?}");
        }
        for (at, visibility) in Visibility::ALL.iter().enumerate() {
            assert_eq!(usize::from(visibility_code(Some(*visibility))), at + 1);
        }
    }

    #[test]
    fn an_index_reads_back_from_its_bytes() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        for (file, kind) in [
            (
                "tests/fixtures/code/crates/ledger/src/lib.rs",
                FileKind::Rust,
            ),
            (
                "tests/fixtures/code/crates/ledger/src/books/mod.rs",
                FileKind::Rust,
            ),
            ("tests/fixtures/code/settings.json", FileKind::Json),
            ("tests/fixtures/code/Makefile", FileKind::Make),
            ("tests/fixtures/code/docs/guide.md", FileKind::Markdown),
            ("Cargo.toml", FileKind::Toml),
        ] {
            let indexed = index(kind, &std::fs::read(root.join(file)).unwrap());
            assert_eq!(decode(&encode(&indexed)), Some(indexed), "{file}");
        }
        for fault in [
            IndexFault::NotUtf8 { at: 7 },
            IndexFault::Syntax { at: 300 },
            IndexFault::TooComplex,
            IndexFault::TooDeep,
            IndexFault::Grammar,
            IndexFault::TooLarge {
                bytes: 5_000_000,
                max: 1 << 20,
            },
        ] {
            assert_eq!(decode(&encode(&Err(fault))), Some(Err(fault)));
        }
    }

    #[test]
    fn bytes_that_break_the_form_are_no_index() {
        let indexed = index(FileKind::Rust, b"mod a { fn b() {} }").unwrap();
        let bytes = encode(&Ok(indexed));

        for cut in 0..bytes.len() {
            assert_eq!(decode(&bytes[..cut]), None, "cut at {cut}");
        }
        let mut longer = bytes.clone();
        longer.push(0);
        assert_eq!(decode(&longer), None);
        // A scope whose parent is itself, one of no segment, and a count
        // past the bytes.
        assert_eq!(decode(&[0, 1, 1, 0, 0, 0, 0]), None);
        assert_eq!(decode(&[0, 1, 0, 0, 0, 0, 0, 0]), None);
        assert_eq!(decode(&[0, 0x80, 0x80, 0x80, 0x80, 0x01]), None);
        assert_eq!(decode(&[9]), None);
    }

    /// The bytes [`Measure`] charges for what `index` holds, its counts
    /// added: what the scope tree spent of the room.
    fn measured(index: &FileIndex) -> usize {
        COUNTS_BYTES
            + index
                .scopes
                .iter()
                .map(|scope| Measure::scope(scope.parent, &scope.name))
                .sum::<usize>()
            + index
                .definitions
                .iter()
                .map(Measure::definition)
                .sum::<usize>()
            + index.macros.iter().map(Measure::type_macro).sum::<usize>()
            + index
                .invocations
                .iter()
                .map(Measure::invocation)
                .sum::<usize>()
            + index
                .module_files
                .iter()
                .map(Measure::module_file)
                .sum::<usize>()
    }

    #[test]
    fn an_entry_takes_no_more_than_its_parts_were_charged() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut pending = vec![root.join("tests/fixtures/code"), root.join("src/code")];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                    continue;
                }
                let name = path.to_str().unwrap().trim_end_matches(".fixture");
                let Some(kind) = FileKind::of(name) else {
                    continue;
                };
                let bytes = std::fs::read(&path).unwrap();
                let Ok(indexed) = index(kind, &bytes) else {
                    continue;
                };
                let entry = encode(&Ok(indexed.clone())).len();
                assert!(entry <= measured(&indexed), "{name}");
                assert!(entry <= max_entry_bytes(bytes.len()), "{name}");
            }
        }
    }

    #[test]
    fn an_entry_is_bounded_by_its_file_whatever_it_holds() {
        let variants = format!("enum E {{ {} }}", "A,".repeat(50_000));
        let fields = format!("struct S {{ {} }}", "a:u8,".repeat(20_000));
        let targets = format!("{}: x\n", "t ".repeat(50_000));
        let headings = "# a\n".repeat(25_000);
        let keys = format!("{{{}\"z\":0}}", "\"a\":0,".repeat(20_000));
        let tables: String = (0..10_000).map(|n| format!("[a{n}]\nb=0\n")).collect();
        let nested = format!("{}{}", "mod m{".repeat(60), "}".repeat(60));
        let reexported = format!(
            "pub use {}::{{{}}};",
            "a".repeat(100_000),
            "self,".repeat(20_000)
        );
        for (kind, text) in [
            (FileKind::Rust, variants.as_str()),
            (FileKind::Rust, fields.as_str()),
            (FileKind::Rust, nested.as_str()),
            (FileKind::Rust, reexported.as_str()),
            (FileKind::Make, targets.as_str()),
            (FileKind::Markdown, headings.as_str()),
            (FileKind::Json, keys.as_str()),
            (FileKind::Toml, tables.as_str()),
            (FileKind::Rust, ""),
        ] {
            let bytes = encode(&index(kind, text.as_bytes()));
            let bound = max_entry_bytes(text.len());
            assert!(
                bytes.len() <= bound,
                "{kind:?} of {} bytes: {} bytes, over {bound}",
                text.len(),
                bytes.len()
            );
        }
    }
}
