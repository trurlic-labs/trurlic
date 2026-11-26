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
//! owns as the allocator takes it (`room.rs`). The room of a file is its
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

mod codes;
mod read;
mod room;
mod write;

pub(in crate::code) use read::Reader;
pub(crate) use read::decode;
pub(crate) use room::max_entry_bytes;
pub(in crate::code::index) use room::room_of;
pub(in crate::code) use room::{ENTRY_OVERHEAD_BYTES, room_per_byte};
pub(crate) use write::encode;
pub(in crate::code) use write::{Measure, Writer};

#[cfg(test)]
mod tests;
