//! The bytes of a file's index in the cache, and back: a compact binary
//! form, whose size and decode are bounded by the file it indexes.
//!
//! Numbers are LEB128 varints, texts a length and UTF-8 bytes, a hash its
//! 32 bytes. A definition whose span is its predecessor's (the targets of
//! one Makefile rule) keeps one byte for it.
//!
//! Every resolver adds what an entry holds through the scope tree, which
//! charges each part its cost ([`Measure`]) and refuses the file past its
//! room ([`room_of`]). A part's cost is its bytes, the slot each of its
//! values takes in its collection (`size_of`), and each heap block it
//! owns as the allocator takes it ([`block`]). The room of a file is its
//! bytes times what the densest definitions of its kind cost per byte
//! ([`room_per_byte`]), so no file within its kind's size is refused for
//! the definitions it holds.
//!
//! A read takes nothing on trust. It charges an entry as the tree did,
//! and the block of each collection the index holds, and refuses one past
//! the room of the file it is read for, [`ENTRY_OVERHEAD_BYTES`] added,
//! before it allocates. So every entry a write makes reads back, and an
//! entry and the index it decodes to together take no more memory than
//! that, as the allocator rounds their blocks. Every count is bounded by
//! the bytes left, every id by what it names, every parent by its child
//! (so a path's walk ends), and a path by
//! [`MAX_NESTING`](super::scope::MAX_NESTING). Bytes that break any of it
//! are no index.

mod read;
mod write;

use super::file::{Definition, FileIndex, FileKind, IndexFault};
use super::kinds::{DefinitionKind, Role, Visibility};
use super::scope::Scope;
use super::symbol::Segment;

pub(in crate::code) use read::Reader;
pub(crate) use read::decode;
pub(crate) use write::encode;
pub(in crate::code) use write::{Measure, Writer};

/// The smallest heap block the allocator takes.
const MIN_BLOCK_BYTES: usize = 32;

/// The bytes a heap block of `requested` bytes takes, as glibc's malloc
/// takes it on a 64-bit target: none for none, else the request and its
/// 8-byte header rounded up to 16, and [`MIN_BLOCK_BYTES`] at least.
pub(in crate::code) const fn block(requested: usize) -> usize {
    if requested == 0 {
        return 0;
    }
    let rounded = requested.saturating_add(8).next_multiple_of(16);
    match rounded < MIN_BLOCK_BYTES {
        true => MIN_BLOCK_BYTES,
        false => rounded,
    }
}

/// The bytes of a hash in an entry.
const HASH_BYTES: usize = 32;

/// What a read keeps beside each scope: its depth.
pub(in crate::code) const DEPTH_BYTES: usize = size_of::<usize>();

/// The densest definitions a file of a kind holds: each takes `bytes` of
/// the file, `name` of them its name.
#[derive(Clone, Copy)]
struct Densest {
    bytes: usize,
    name: usize,
}

/// The densest definitions of `kind`. A definition takes its name and a
/// byte that ends it at least, and a name of one byte costs a block of
/// its own, so where a kind admits a name of none, one of none is the
/// densest.
const fn densest(kind: FileKind) -> Densest {
    match kind {
        // An enum variant or a name in a list, `A,`; a Makefile target of
        // a rule of many, `a `; a table of a dotted TOML key, `a.` of
        // `a.b.c = 0`.
        FileKind::Rust
        | FileKind::Python
        | FileKind::TypeScript
        | FileKind::TypeScriptDeclarations
        | FileKind::Tsx
        | FileKind::C
        | FileKind::Cpp
        | FileKind::Make
        | FileKind::Toml => Densest { bytes: 2, name: 1 },
        // A key of an empty name, `"":0,`.
        FileKind::Json => Densest { bytes: 5, name: 0 },
        // A heading of an empty title, `#` and its line's end.
        FileKind::Markdown => Densest { bytes: 2, name: 0 },
    }
}

/// The bytes of the varint of a number below `bound`.
const fn number_bytes(bound: usize) -> usize {
    let mut bytes = 1;
    let mut rest = bound >> 7;
    while rest > 0 {
        bytes += 1;
        rest >>= 7;
    }
    bytes
}

/// What a definition `densest` describes costs in a file of `max` bytes
/// at most: its scope of one segment and itself, each number of their
/// entry as long as `max` makes it, as [`Measure`] charges them.
const fn definition_cost(densest: Densest, max: usize) -> usize {
    let number = number_bytes(max);
    // Its parent, its count of segments, the segment's tag, its name's
    // length and its name.
    let scope_bytes = number + 3 + densest.name;
    let scope = size_of::<Scope>()
        + DEPTH_BYTES
        + block(size_of::<Segment>())
        + block(densest.name)
        + scope_bytes;
    // Its scope, kind, marks and span tag, then its span's four numbers
    // and its hash, which a span its predecessor's leaves out.
    scope + size_of::<Definition>() + number + 3 + 4 * number + HASH_BYTES
}

/// The room of each byte of a file of `kind`: what its densest
/// definitions cost per byte.
pub(in crate::code) const fn room_per_byte(kind: FileKind) -> usize {
    let densest = densest(kind);
    definition_cost(densest, kind.max_bytes()).div_ceil(densest.bytes)
}

/// The bytes of an index's outcome tag and its three counts, each a count
/// of at most `u32::MAX` items.
const COUNTS_BYTES: usize = 1 + 3 * 5;

/// The collections a decode allocates whole: the scopes, their depths,
/// the definitions and the module files. Each block rounds its slots up
/// by less than [`MIN_BLOCK_BYTES`].
const COLLECTIONS: usize = 4;

/// The bytes an entry and its decode take past the room of its file: the
/// counts of an index, and what the block of each collection rounds up.
pub(in crate::code) const ENTRY_OVERHEAD_BYTES: usize =
    COUNTS_BYTES + COLLECTIONS * MIN_BLOCK_BYTES;

/// What the parts of the index of a file of `kind` of `bytes` bytes,
/// within its kind's size, may cost together.
pub(in crate::code) const fn room_of(kind: FileKind, bytes: usize) -> usize {
    bytes.saturating_mul(room_per_byte(kind))
}

/// The most memory an entry of a file of `kind` of `bytes` bytes and its
/// decode take together: past the kind's size, a fault's alone.
pub(crate) const fn max_entry_bytes(kind: FileKind, bytes: usize) -> usize {
    match bytes > kind.max_bytes() {
        true => ENTRY_OVERHEAD_BYTES,
        false => room_of(kind, bytes).saturating_add(ENTRY_OVERHEAD_BYTES),
    }
}

/// What indexing a file's bytes gave.
pub(crate) type Indexed = Result<FileIndex, IndexFault>;

/// A kind's code: its place in [`DefinitionKind::ALL`], by which a read
/// takes it back.
pub(in crate::code) fn kind_code(kind: DefinitionKind) -> u8 {
    place_in(&DefinitionKind::ALL, &kind)
}

/// A visibility's code: 0 for none, else one past its place in
/// [`Visibility::ALL`].
pub(in crate::code) fn visibility_code(visibility: Option<Visibility>) -> u8 {
    visibility.map_or(0, |visibility| {
        place_in(&Visibility::ALL, &visibility).saturating_add(1)
    })
}

/// The place of `value` in `all`; past its end, a code no read takes back,
/// when `all` misses it.
fn place_in<T: PartialEq>(all: &[T], value: &T) -> u8 {
    let place = all.iter().position(|each| each == value);
    u8::try_from(place.unwrap_or(all.len())).unwrap_or(u8::MAX)
}

pub(in crate::code) const fn role_code(role: Role) -> u8 {
    match role {
        Role::Code => 0,
        Role::Test => 1,
    }
}

#[cfg(test)]
mod tests;
