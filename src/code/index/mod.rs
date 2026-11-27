//! What a file's index is, whichever resolver builds it: the kind of a
//! file, the definitions and scopes it holds, what a Cargo manifest
//! states, the symbol grammar its paths are spelled in, the budgets and
//! bounds it is built within, the scope tree every resolver builds it
//! through, and its entry in the cache. Nothing here reads a grammar or
//! names a resolver: the resolvers stand on these values, never the other
//! way.

mod encoding;
mod file;
mod kinds;
mod manifest;
mod scope;
mod scope_tree;
mod span;
mod symbol;
mod work;

pub(in crate::code) use encoding::{ENTRY_OVERHEAD_BYTES, Reader, Writer, room_per_byte};
pub(crate) use encoding::{decode, encode, max_entry_bytes};
pub(crate) use file::{
    Definition, FileIndex, FileKind, IndexFault, Indexed, MAX_SOURCE_BYTES, ModuleFile,
};
pub(in crate::code) use file::{LEXICAL_MAX_BYTES, TREE_SITTER_MAX_BYTES};
pub(in crate::code) use kinds::What;
pub(crate) use kinds::{DefinitionKind, Role, Visibility};
pub(in crate::code) use manifest::{
    Dependency, DependencyKind, DependencySource, Manifest, Package, Workspace, WorkspaceDependency,
};
pub(crate) use scope::{MAX_NESTING, Scope, ScopeId, Scopes};
pub(in crate::code) use scope_tree::ScopeTree;
pub(in crate::code) use span::{CLOSE_MARK, Lines, NESTED_MARK, OPEN_MARK};
pub(crate) use span::{HASH_SCHEME, Span, SpanHash};
pub(in crate::code) use symbol::name_segments;
pub(super) use symbol::{Segment, Symbol, SymbolFault, Syntax};
pub(in crate::code) use work::{
    LEXICAL_WORK_PER_BYTE, MIN_PARSE_OPERATIONS, MIN_WORK, PARSE_OPERATIONS_PER_BYTE,
    SCAN_WORK_PER_BYTE, SERVED_BYTES_PER_BYTE, Work,
};
