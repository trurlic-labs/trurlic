//! What a page of `map` shows of each child, declared once for the output
//! schema and the JSON. What one kind of node alone has is an object under
//! the kind's own key, so a child's kind states which keys it holds.

use crate::sources::{
    Child, ChildKind, CrateFacts, DecisionCounts, Definitions, Dependencies, Dependency, Doc,
    EndpointKind, FileFacts, FilePlace, ImportEdge, Imports, Lines, ManifestState, ModuleFacts,
};
use crate::tools::spec::output::{Bool, Count, Items, Nested, Str, Tag};
use crate::tools::spec::{Output, Shown};

impl Output for Child {
    fn declare(fields: &mut Shown<'_, Self>) {
        let address = "Its address: `node` set to it lists the nodes it holds.";
        fields.required("address", address, &Str, |child| &child.address);
        let kind = Tag {
            all: &ChildKind::ALL,
            name: ChildKind::as_str,
        };
        let about = "`crate`, with the key `crate`; `outside_crates`: what lies outside every \
                     crate, from the code root, first of the top level when it is there; \
                     `module`: a Rust module, its own file or files and the modules below it, \
                     with the key `module`; `directory`: files in no Rust module; `file`, with \
                     the key `file`.";
        fields.required("kind", about, &kind, |child| &child.kind);
        let name = "The last segment of its path: of a crate's or a root module's directory, \
                    `.` for the code root, a module's own name, a directory's or a file's; \
                    `outside_crates` for what lies outside every crate.";
        fields.required("name", name, &Str, |child| &child.name);
        declare_sums(fields);
        let own = "Present on a child of kind `crate` alone.";
        let kind = Nested::<CrateFacts>::new();
        fields.optional("crate", own, &kind, |child| child.crate_facts.as_ref());
        let own = "Present on a child of kind `module` alone.";
        let kind = Nested::<ModuleFacts>::new();
        fields.optional("module", own, &kind, |child| child.module.as_ref());
        let own = "Present on a child of kind `file` alone.";
        let kind = Nested::<FileFacts>::new();
        fields.optional("file", own, &kind, |child| child.file.as_ref());
    }
}

/// The fields of a [`Child`] that sum what lies in and under it.
fn declare_sums(fields: &mut Shown<'_, Child>) {
    let lines = "The lines of the walked files at or below it.";
    let kind = Nested::<Lines>::new();
    fields.required("lines", lines, &kind, |child| &child.lines);
    let definitions = "The definitions the index holds of the walked files at or below it.";
    let kind = Nested::<Definitions>::new();
    fields.required("definitions", definitions, &kind, |child| {
        &child.definitions
    });
    let decisions = "The decisions anchored in it, each once: one of whose anchors names a \
                     walked file at or below it, or a directory whose walked files all are.";
    let kind = Nested::<DecisionCounts>::new();
    fields.required("decisions", decisions, &kind, |child| &child.decisions);
    let unresolved = "The Rust imports of the files at or below it that resolve to no file, \
                      re-exports included.";
    fields.required("unresolved_imports", unresolved, &Count, |child| {
        &child.unresolved_imports
    });
}

impl Output for CrateFacts {
    fn declare(fields: &mut Shown<'_, Self>) {
        let manifest = Tag {
            all: &ManifestState::ALL,
            name: ManifestState::as_str,
        };
        let about = "`read`, or `unread` when its manifest has no index (a syntax error, a file \
                     past its size), so its package and dependencies are unknown.";
        fields.required("manifest", about, &manifest, |facts| &facts.manifest);
        let package = "Its package name, when its manifest is read and names one.";
        fields.optional("package", package, &Str, |facts| facts.package.as_deref());
        let dependencies = "Its path and workspace dependencies, by the table of its manifest \
                            that states them, when its manifest is read.";
        let kind = Nested::<Dependencies>::new();
        fields.optional("dependencies", dependencies, &kind, |facts| {
            facts.dependencies.as_ref()
        });
    }
}

impl Output for Dependencies {
    fn declare(fields: &mut Shown<'_, Self>) {
        let kind = Items {
            item: Nested::<Dependency>::new(),
        };
        fields.required("normal", "`[dependencies]`.", &kind, |tables| {
            &tables.normal
        });
        fields.required("dev", "`[dev-dependencies]`.", &kind, |tables| &tables.dev);
        fields.required("build", "`[build-dependencies]`.", &kind, |tables| {
            &tables.build
        });
        let omitted = "The dependencies the page left out of the tables, every one, when the \
                       crate passes a page with them.";
        fields.optional("omitted", omitted, &Count, |tables| {
            (tables.omitted > 0).then_some(&tables.omitted)
        });
    }
}

impl Output for Dependency {
    fn declare(fields: &mut Shown<'_, Self>) {
        let endpoint = Tag {
            all: &EndpointKind::ALL,
            name: EndpointKind::as_str,
        };
        let about = "`crate`: a crate of the tree; `unwalked`: a path whose directory holds no \
                     walked manifest; `no_workspace_root`: a workspace dependency of a crate \
                     with no workspace root the walked manifests show; `not_in_workspace`: a \
                     workspace dependency its root does not name.";
        fields.required("endpoint", about, &endpoint, |dependency| {
            &dependency.endpoint
        });
        let value = "For `crate` the crate's address, a child of the top level; for \
                     `unwalked` the path as the manifest writes it; for the other two the \
                     dependency's name.";
        fields.required("value", value, &Str, |dependency| &dependency.value);
    }
}

impl Output for ModuleFacts {
    fn declare(fields: &mut Shown<'_, Self>) {
        let doc = "The first paragraph of its own file's doc the index holds, when it has one \
                   own file and no other.";
        let kind = Nested::<Doc>::new();
        fields.optional("doc", doc, &kind, |module| module.doc.as_ref());
        let kind = Nested::<Imports>::new();
        fields.required("imports", IMPORTS, &kind, |module| &module.imports);
    }
}

/// What the `imports` of a module or a file hold.
const IMPORTS: &str = "The Rust imports of the files below it that resolve to a file: those \
                       into the other children of the level, listed, and the dependency \
                       imports into files outside the level, counted.";

impl Output for FileFacts {
    fn declare(fields: &mut Shown<'_, Self>) {
        let place = Tag {
            all: &FilePlace::ALL,
            name: FilePlace::as_str,
        };
        let about = "`in_module`: a Rust file at its module; `no_module`: a file of a language \
                     with no modules, or a manifest; `unreached`: a Rust file no target's mod \
                     declarations reach; `unresolved_several`: one reached at two or more \
                     module paths; `unresolved_in_item`: one a mod inside a function or \
                     another item declares; `unresolved_two_files`: one declared where both \
                     name.rs and name/mod.rs are.";
        fields.required("place", about, &place, |file| &file.place);
        let doc = "The first paragraph of its doc the index holds: a Rust file's.";
        let kind = Nested::<Doc>::new();
        fields.optional("doc", doc, &kind, |file| file.doc.as_ref());
        let kind = Nested::<Imports>::new();
        fields.required("imports", IMPORTS, &kind, |file| &file.imports);
    }
}

impl Output for Imports {
    fn declare(fields: &mut Shown<'_, Self>) {
        let siblings = "Into the files below the other children of the level, by the order \
                        of the child they go to.";
        let kind = Items {
            item: Nested::<ImportEdge>::new(),
        };
        fields.required("siblings", siblings, &kind, |imports| &imports.siblings);
        let outside = "The dependency imports into files outside the level, counted and not \
                       listed; re-exports are left out.";
        fields.required("outside_level", outside, &Count, |imports| {
            &imports.outside_level
        });
        let omitted = "The edges the page left out of `siblings`, every one, when the child \
                       passes a page with them.";
        fields.optional("omitted", omitted, &Count, |imports| {
            (imports.omitted > 0).then_some(&imports.omitted)
        });
    }
}

impl Output for ImportEdge {
    fn declare(fields: &mut Shown<'_, Self>) {
        let to = "The address of the child they go to.";
        fields.required("to", to, &Str, |import| &import.to);
        let count = "The dependency imports: every import but a re-export. 0 for an edge of \
                     re-exports alone, which is listed.";
        fields.required("count", count, &Count, |import| &import.count);
        let reexports = "The re-exports: `use`s a visibility opens, which are no dependency.";
        fields.required("reexports", reexports, &Count, |import| &import.reexports);
        let test_only = "Whether every one of them, dependency or re-export, is test code.";
        fields.required("test_only", test_only, &Bool, |import| &import.test_only);
    }
}

impl Output for Doc {
    fn declare(fields: &mut Shown<'_, Self>) {
        fields.required("text", "The paragraph, on one line.", &Str, |doc| &doc.text);
        let cut = "Whether the paragraph runs on past `text`, the bytes of it the index \
                   holds.";
        fields.required("cut", cut, &Bool, |doc| &doc.cut);
    }
}

impl Output for Lines {
    fn declare(fields: &mut Shown<'_, Self>) {
        let counted = "The lines of the files whose bytes were read.";
        fields.required("counted", counted, &Count, |lines| &lines.counted);
        let unread = "The files none of whose bytes were read (past the size a source file \
                      may have, unreadable), whose lines `counted` leaves out.";
        fields.required("unread_files", unread, &Count, |lines| &lines.unread_files);
    }
}

impl Output for Definitions {
    fn declare(fields: &mut Shown<'_, Self>) {
        let all = "Every definition, test code included.";
        fields.required("all", all, &Count, |definitions| &definitions.all);
        let non_test = "The definitions neither marked test code nor in a file that is.";
        fields.required("non_test", non_test, &Count, |definitions| {
            &definitions.non_test
        });
        let unindexed = "The files with no index (unread, not UTF-8, too large or too complex \
                         to index), whose definitions both counts leave out.";
        fields.required("unindexed_files", unindexed, &Count, |definitions| {
            &definitions.unindexed_files
        });
    }
}

impl Output for DecisionCounts {
    fn declare(fields: &mut Shown<'_, Self>) {
        fields.required("proposed", "Proposed decisions.", &Count, |counts| {
            &counts.proposed
        });
        fields.required("accepted", "Accepted decisions.", &Count, |counts| {
            &counts.accepted
        });
    }
}
