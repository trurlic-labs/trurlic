//! The paths of a file's definitions, held as a tree: each scope stores
//! its own name segments and its parent, so a path is walked, never
//! copied, and an index grows with its file, not with its file times its
//! nesting.
//!
//! A scope is a definition's place or a container that is no definition
//! (an inherent `impl`'s type). A parent is always created before its
//! children, so a parent's id is below its child's and a walk up ends.
//! Every segment counts toward [`MAX_NESTING`], those of a key or heading
//! split at `::` included.
//!
//! The tree is also the one way into an index: every resolver enters
//! its scopes and admits its definitions, macros, invocations and module
//! files through it, and it charges each the bytes its entry takes
//! against the room of the file ([`encoding::room_of`]), so no rule of any
//! resolver can make an index outgrow its file.

use super::encoding::{self, Measure};
use super::index::{Definition, IndexFault, Invocation, ModuleFile, TypeMacro};
use super::symbol::Segment;

/// The segments a path may hold. A file that nests one deeper has no
/// index.
pub(crate) const MAX_NESTING: usize = 64;

/// A scope of one file, by its place in the file's scopes, or past them
/// in the scopes a macro expansion adds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ScopeId(u32);

impl ScopeId {
    pub(crate) const fn at(self) -> usize {
        self.0 as usize
    }

    /// The id of the scope at `at`; `None` past `u32`, which no file
    /// within the size a source may have reaches.
    pub(crate) fn of(at: usize) -> Option<Self> {
        u32::try_from(at).ok().map(Self)
    }
}

/// A scope: its own name segments, under its parent, `None` at the top.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Scope {
    pub(crate) parent: Option<ScopeId>,
    pub(crate) name: Vec<Segment>,
}

/// The scopes of a file as a resolver makes them, each with its depth,
/// and the bytes the file's entry has left.
pub(super) struct ScopeTree {
    scopes: Vec<Scope>,
    depths: Vec<usize>,
    room: usize,
}

impl ScopeTree {
    /// The tree of a file of `bytes` bytes.
    pub(super) fn for_file(bytes: usize) -> Self {
        Self {
            scopes: Vec::new(),
            depths: Vec::new(),
            room: encoding::room_of(bytes),
        }
    }

    /// The scope `name` under `parent`; [`IndexFault::TooDeep`] past
    /// [`MAX_NESTING`] segments, [`IndexFault::TooComplex`] past the room.
    pub(super) fn enter(
        &mut self,
        parent: Option<ScopeId>,
        name: Vec<Segment>,
    ) -> Result<ScopeId, IndexFault> {
        let above = parent.map_or(0, |id| self.depths.get(id.at()).copied().unwrap_or(0));
        let depth = above + name.len();
        let id = ScopeId::of(self.scopes.len()).ok_or(IndexFault::TooComplex)?;
        if depth > MAX_NESTING {
            return Err(IndexFault::TooDeep);
        }
        self.charge(Measure::scope(parent, &name))?;
        self.scopes.push(Scope { parent, name });
        self.depths.push(depth);
        Ok(id)
    }

    /// Charge `definition` its bytes; [`IndexFault::TooComplex`] past the
    /// room.
    pub(super) fn admit_definition(&mut self, definition: &Definition) -> Result<(), IndexFault> {
        self.charge(Measure::definition(definition))
    }

    pub(super) fn admit_macro(&mut self, found: &TypeMacro) -> Result<(), IndexFault> {
        self.charge(Measure::type_macro(found))
    }

    pub(super) fn admit_invocation(&mut self, invocation: &Invocation) -> Result<(), IndexFault> {
        self.charge(Measure::invocation(invocation))
    }

    pub(super) fn admit_module_file(&mut self, module: &ModuleFile) -> Result<(), IndexFault> {
        self.charge(Measure::module_file(module))
    }

    fn charge(&mut self, bytes: usize) -> Result<(), IndexFault> {
        self.room = self.room.checked_sub(bytes).ok_or(IndexFault::TooComplex)?;
        Ok(())
    }

    pub(super) fn get(&self, id: ScopeId) -> Option<&Scope> {
        self.scopes.get(id.at())
    }

    pub(super) fn into_scopes(self) -> Vec<Scope> {
        self.scopes
    }
}

/// The scopes a path is walked through: a file's, and those a macro
/// expansion adds past them.
#[derive(Clone, Copy)]
pub(crate) struct Scopes<'a> {
    pub(crate) file: &'a [Scope],
    pub(crate) added: &'a [Scope],
}

impl<'a> Scopes<'a> {
    pub(crate) fn get(self, id: ScopeId) -> Option<&'a Scope> {
        match id.at().checked_sub(self.file.len()) {
            None => self.file.get(id.at()),
            Some(past) => self.added.get(past),
        }
    }

    /// The segments of the path of `id`, from its last: each scope's
    /// own, then its parent's. A parent's id is below its child's, so the
    /// walk ends.
    pub(crate) fn path_from_end(self, id: ScopeId) -> impl Iterator<Item = &'a Segment> + Clone {
        let mut next = Some(id);
        std::iter::from_fn(move || {
            let current = next?;
            let scope = self.get(current)?;
            next = scope.parent.filter(|parent| *parent < current);
            Some(scope.name.iter().rev())
        })
        .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_is_walked_through_its_parents_and_past_the_bound_is_refused() {
        let mut tree = ScopeTree::for_file(64);
        let outer = tree.enter(None, vec![Segment::name("a")]).unwrap();
        let inner = tree
            .enter(Some(outer), vec![Segment::name("b"), Segment::name("c")])
            .unwrap();
        let scopes = tree.into_scopes();
        let view = Scopes {
            file: &scopes,
            added: &[],
        };
        let spelled: Vec<String> = view.path_from_end(inner).map(ToString::to_string).collect();

        assert_eq!(spelled, ["c", "b", "a"]);

        let mut tree = ScopeTree::for_file(4096);
        let wide: Vec<Segment> = (0..MAX_NESTING).map(|_| Segment::name("x")).collect();
        let full = tree.enter(None, wide).unwrap();
        assert_eq!(
            tree.enter(Some(full), vec![Segment::name("y")]),
            Err(IndexFault::TooDeep)
        );
    }

    #[test]
    fn a_scope_past_the_files_room_is_refused() {
        let mut tree = ScopeTree::for_file(1);
        let name = "n".repeat(encoding::room_of(1));

        assert_eq!(
            tree.enter(None, vec![Segment::name(&name)]),
            Err(IndexFault::TooComplex)
        );
        assert!(tree.enter(None, vec![Segment::name("n")]).is_ok());
    }
}
