//! The bytes of a file's lexical pass in the cache, and back, in the
//! index entry's form (`code/encoding`): a read charges each heap block
//! before it allocates it against the room of the file it is read for and
//! refuses an entry past it, so bytes no write made cost no more than
//! that room.

use crate::code::index::{Reader, Writer};

use super::{Body, Define, Defines, Lexed};

/// The room a lexical entry may take per byte of its file, its bytes and
/// the heap blocks its values take once read together: a one-letter name
/// of a `#define`'s body, two bytes of the file, costs its byte and its
/// length, its slot in its list and its smallest heap block, the densest
/// a file gets.
const MAX_LEXED_BYTES_PER_SOURCE_BYTE: usize = 32;

/// The room an entry may take past its file's: its counts.
const LEXED_OVERHEAD_BYTES: usize = 64;

/// The room of the lexical entry of a file of `source_bytes` bytes.
pub(crate) const fn max_lexed_bytes(source_bytes: usize) -> usize {
    source_bytes
        .saturating_mul(MAX_LEXED_BYTES_PER_SOURCE_BYTE)
        .saturating_add(LEXED_OVERHEAD_BYTES)
}

/// The bytes of `lexed`.
pub(crate) fn encode_lexed(lexed: &Lexed) -> Vec<u8> {
    let mut out = Writer::entry();
    out.number(lexed.defines.0.len());
    for define in &lexed.defines.0 {
        out.text(&define.name);
        out.byte(u8::from(define.function_like));
        match &define.body {
            Body::Code => out.byte(0),
            Body::Opens => out.byte(1),
            Body::Closes => out.byte(2),
            Body::Attribute {
                names,
                calls,
                empty,
                holds_static,
            } => {
                out.byte(3 | u8::from(*empty) << 2 | u8::from(*holds_static) << 3);
                names_of(&mut out, names);
                names_of(&mut out, calls);
            }
        }
    }
    out.text(&lexed.spelled);
    out.into_bytes()
}

fn names_of(out: &mut Writer<Vec<u8>>, names: &[String]) {
    out.number(names.len());
    for name in names {
        out.text(name);
    }
}

/// The lexical pass `bytes` hold for a file of `source_bytes` bytes;
/// `None` when they are no encoding of one within its room.
pub(crate) fn decode_lexed(bytes: &[u8], source_bytes: usize) -> Option<Lexed> {
    let mut reader = Reader::new(bytes, max_lexed_bytes(source_bytes))?;
    let count = reader.items::<Define>()?;
    let mut defines = Vec::with_capacity(count);
    for _ in 0..count {
        defines.push(define(&mut reader)?);
    }
    let spelled = reader.text()?;
    reader.is_done().then_some(Lexed {
        defines: Defines(defines),
        spelled,
    })
}

fn define(reader: &mut Reader<'_>) -> Option<Define> {
    let name = reader.text()?;
    let function_like = match reader.byte()? {
        0 => false,
        1 => true,
        _ => return None,
    };
    let body = match reader.byte()? {
        0 => Body::Code,
        1 => Body::Opens,
        2 => Body::Closes,
        tag if tag & 3 == 3 && tag >> 4 == 0 => Body::Attribute {
            names: names(reader)?,
            calls: names(reader)?,
            empty: tag & 4 != 0,
            holds_static: tag & 8 != 0,
        },
        _ => return None,
    };
    Some(Define {
        name,
        function_like,
        body,
    })
}

fn names(reader: &mut Reader<'_>) -> Option<Vec<String>> {
    let count = reader.items::<String>()?;
    let mut found = Vec::with_capacity(count);
    for _ in 0..count {
        found.push(reader.text()?);
    }
    Some(found)
}
