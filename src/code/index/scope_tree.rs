//! The one way into an index: every resolver enters its scopes and
//! admits its definitions, with their docs, its module files, its file's
//! doc and its manifest through the scope tree, which charges each what
//! its entry costs against the room of the file ([`encoding::room_of`]),
//! so no rule of any resolver can make an index outgrow its file, and
//! refuses a path past [`MAX_NESTING`] segments.

use super::encoding::{self, Measure};
use super::file::{Definition, Doc, FileKind, IndexFault, ModuleFile};
use super::manifest::Manifest;
use super::scope::{MAX_NESTING, Scope, ScopeId};
use super::span::Span;
use super::symbol::Segment;

/// The scopes of a file as a resolver makes them, each with its depth,
/// what the file's entry has left of its room, and the span of the last
/// definition admitted.
pub(in crate::code) struct ScopeTree {
    scopes: Vec<Scope>,
    depths: Vec<usize>,
    room: usize,
    previous: Option<Span>,
}

impl ScopeTree {
    /// The tree of a file of `kind` of `bytes` bytes.
    pub(in crate::code) const fn for_file(kind: FileKind, bytes: usize) -> Self {
        Self {
            scopes: Vec::new(),
            depths: Vec::new(),
            room: encoding::room_of(kind, bytes),
            previous: None,
        }
    }

    /// The scope `name` under `parent`; [`IndexFault::TooDeep`] past
    /// [`MAX_NESTING`] segments, [`IndexFault::TooComplex`] past the room.
    pub(in crate::code) fn enter(
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

    /// Charge `definition` its cost; [`IndexFault::TooComplex`] past the
    /// room. Its span is charged as one byte when it is the last one
    /// admitted: a run of definitions of one span stays one run once the
    /// index sorts them, so the entry shares at least as many.
    pub(in crate::code) fn admit_definition(
        &mut self,
        definition: &Definition,
    ) -> Result<(), IndexFault> {
        self.charge(Measure::definition(definition, self.previous))?;
        self.previous = Some(definition.span);
        Ok(())
    }

    pub(in crate::code) fn admit_module_file(
        &mut self,
        module: &ModuleFile,
    ) -> Result<(), IndexFault> {
        self.charge(Measure::module_file(module))
    }

    pub(in crate::code) fn admit_manifest(
        &mut self,
        manifest: &Manifest,
    ) -> Result<(), IndexFault> {
        self.charge(Measure::manifest(manifest))
    }

    pub(in crate::code) fn admit_file_doc(&mut self, doc: &Doc) -> Result<(), IndexFault> {
        self.charge(Measure::file_doc(doc))
    }

    fn charge(&mut self, bytes: usize) -> Result<(), IndexFault> {
        self.room = self.room.checked_sub(bytes).ok_or(IndexFault::TooComplex)?;
        Ok(())
    }

    pub(in crate::code) fn get(&self, id: ScopeId) -> Option<&Scope> {
        self.scopes.get(id.at())
    }

    pub(in crate::code) fn into_scopes(self) -> Vec<Scope> {
        self.scopes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::index::scope::Scopes;

    #[test]
    fn a_path_is_walked_through_its_parents_and_past_the_bound_is_refused() {
        let mut tree = ScopeTree::for_file(FileKind::Rust, 64);
        let outer = tree.enter(None, vec![Segment::name("a")]).unwrap();
        let inner = tree
            .enter(Some(outer), vec![Segment::name("b"), Segment::name("c")])
            .unwrap();
        let scopes = tree.into_scopes();
        let spelled: Vec<String> = Scopes(&scopes)
            .path_from_end(inner)
            .map(ToString::to_string)
            .collect();

        assert_eq!(spelled, ["c", "b", "a"]);

        let mut tree = ScopeTree::for_file(FileKind::Rust, 4096);
        let wide: Vec<Segment> = (0..MAX_NESTING).map(|_| Segment::name("x")).collect();
        let full = tree.enter(None, wide).unwrap();
        assert_eq!(
            tree.enter(Some(full), vec![Segment::name("y")]),
            Err(IndexFault::TooDeep)
        );
    }

    #[test]
    fn a_scope_past_the_files_room_is_refused() {
        let mut tree = ScopeTree::for_file(FileKind::Rust, 2);
        let name = "n".repeat(encoding::room_of(FileKind::Rust, 2));

        assert_eq!(
            tree.enter(None, vec![Segment::name(&name)]),
            Err(IndexFault::TooComplex)
        );
        assert!(tree.enter(None, vec![Segment::name("n")]).is_ok());
    }

    #[test]
    fn a_manifest_past_the_files_room_is_refused() {
        let manifest = |path: &str| Manifest {
            package: None,
            workspace: Some(crate::code::index::Workspace {
                dependencies: Vec::new(),
                exclude: vec![path.to_owned()],
            }),
        };
        let room = encoding::room_of(FileKind::Toml, 2);

        assert_eq!(
            ScopeTree::for_file(FileKind::Toml, 2).admit_manifest(&manifest(&"p".repeat(room))),
            Err(IndexFault::TooComplex)
        );
        assert!(
            ScopeTree::for_file(FileKind::Toml, 2)
                .admit_manifest(&manifest("p"))
                .is_ok()
        );
    }
}
