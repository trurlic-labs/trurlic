//! What the parts of an entry cost and the room of a file of a kind:
//! each part's bytes, the slot each of its values takes in its collection
//! and each heap block it owns as the allocator takes it ([`block`]); a
//! file's room is its bytes times what the densest definitions of its kind
//! cost per byte ([`room_per_byte`]).

use crate::code::index::file::{Definition, FileKind};
use crate::code::index::scope::Scope;
use crate::code::index::symbol::Segment;

/// The smallest heap block the allocator takes.
const MIN_BLOCK_BYTES: usize = 32;

/// The bytes a heap block of `requested` bytes takes, as glibc's malloc
/// takes it on a 64-bit target: none for none, else the request and its
/// 8-byte header rounded up to 16, and [`MIN_BLOCK_BYTES`] at least.
pub(super) const fn block(requested: usize) -> usize {
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
pub(super) const DEPTH_BYTES: usize = size_of::<usize>();

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
/// entry as long as `max` makes it, as [`Measure`](super::write::Measure)
/// charges them.
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
pub(super) const COUNTS_BYTES: usize = 1 + 3 * 5;

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
pub(in crate::code::index) const fn room_of(kind: FileKind, bytes: usize) -> usize {
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
