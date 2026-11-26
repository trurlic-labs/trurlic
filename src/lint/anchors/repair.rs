//! The repair of a stale anchor: the anchor it would hold had it followed
//! its code, when the calling tree and git's history show the code itself
//! there. A wrong repair hides the staleness it mends, and
//! `repair_anchors` writes every repair `check` shows unattended, so a
//! repair is given only on evidence of the code itself, never of a name;
//! an anchor without such evidence stays stale, as it is.
//!
//! Only a stale anchor is repaired: a planned one names what is not
//! written yet, and a pending one's code is on a branch. A file anchor
//! whose file is gone follows that file where git's history shows its
//! blob moved, unchanged, as the shell follows it ([`Renames`]), into a
//! file that defines an item: a file of comments or nothing alone is no
//! code to follow, and a file git pairs with it by similarity alone, as a
//! shared header pairs two, is no move of it. A symbol anchor on a gone
//! file is judged on the file its file went to, by the code itself, as
//! below.
//!
//! Any other symbol anchor on a Rust file whose file is gone or defines
//! nothing its symbol names is searched for where its code may have moved
//! ([`CodeFacts::namesakes`]): never when its file still defines its last
//! segment, since the code is then there under another spelling, and only
//! as the whole path of an item written in another file of the same target
//! by place, never a re-export, a module declaration or an item a macro
//! invocation produces. A file of another language is not searched, since
//! no crate or target bounds where its code may go. The one such file is a
//! [`Search`] candidate, repaired when git's history shows the move
//! ([`Shown`]): where the anchor's file last lost an item of the symbol,
//! the candidate gained definitions of the span hash the anchor's had, of
//! an item it did not write before; and the candidate held an item of the
//! kinds it gained at every commit that changed it since, and holds one
//! now. A namesake that stood beside the code, one written after the code
//! went, and one a later commit deleted or replaced by another kind of item
//! are so no move, while an edit of the moved item since keeps its repair.
//! With no git, or any git failure, no anchor is repaired.

use std::collections::{BTreeMap, BTreeSet};

use crate::code::{Code, CodeFacts, DefinitionKind, Entry, Namesakes, Resolution};
use crate::store::Anchor;

use super::{Asked, Here};

/// A stale anchor's repair: the anchor it is replaced by, and the code that
/// anchor names in the tree it was judged in, which the write holds it to
/// again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Repair {
    pub(crate) to: Anchor,
    pub(crate) code: Code,
}

/// Where git's history says each gone file of a stale anchor is now, and
/// the facts of those paths.
#[derive(Debug)]
pub(crate) struct Renames {
    pub(crate) to: BTreeMap<String, String>,
    pub(crate) facts: CodeFacts,
}

impl Renames {
    /// No gone file followed.
    pub(crate) fn none() -> Self {
        Self {
            to: BTreeMap::new(),
            facts: CodeFacts::new(BTreeMap::new(), BTreeSet::new()),
        }
    }
}

/// Files of git's history, each read alone, by the name git read it by and
/// its path.
#[derive(Debug, Default)]
pub(crate) struct Prior {
    files: BTreeMap<(String, String), CodeFacts>,
}

impl Prior {
    /// Each of `files` held its entry: a name git read it by, a path and
    /// what was there.
    pub(crate) fn extend(&mut self, files: impl IntoIterator<Item = (String, String, Entry)>) {
        for (name, path, entry) in files {
            // clone: the facts key their one entry by the path the map keys too.
            let facts = CodeFacts::new(BTreeMap::from([(path.clone(), entry)]), BTreeSet::new());
            self.files.insert((name, path), facts);
        }
    }

    /// The facts of `anchor`'s file as `name` held it, when it was read.
    fn facts(&self, name: &str, anchor: &Anchor) -> Option<&CodeFacts> {
        let key = (name.to_owned(), anchor.file().to_owned());
        self.files.get(&key)
    }

    /// What `anchor` resolved to in `name`; `FileMissing` for a file not
    /// read.
    fn resolve(&self, name: &str, anchor: &Anchor) -> Resolution {
        self.facts(name, anchor)
            .map_or(Resolution::FileMissing, |facts| facts.resolve(anchor))
    }

    /// Whether `name` wrote an item whose whole path `anchor`'s symbol is:
    /// no re-export, no module declaration.
    fn wrote(&self, name: &str, anchor: &Anchor) -> bool {
        self.facts(name, anchor)
            .is_some_and(|facts| facts.writes_whole(anchor))
    }

    /// The kinds of the definitions of `name` whose whole path `anchor`'s
    /// symbol is: none for a file not read.
    fn kinds(&self, name: &str, anchor: &Anchor) -> BTreeSet<DefinitionKind> {
        self.facts(name, anchor)
            .map_or_else(BTreeSet::new, |facts| facts.kinds(anchor))
    }
}

/// The repair of each of `asked`'s stale file anchors whose gone file git's
/// history shows at another path now, `renames`, when that file defines an
/// item.
pub(crate) fn followed<'s>(
    asked: &Asked<'s>,
    here: &Here<'s, '_>,
    renames: &Renames,
) -> BTreeMap<&'s Anchor, Repair> {
    asked
        .stale
        .iter()
        .copied()
        .filter(|&anchor| {
            let gone = here.resolutions.get(anchor) == Some(&Resolution::FileMissing);
            anchor.symbol().is_none() && gone
        })
        .filter_map(|anchor| {
            let to = renames.to.get(anchor.file())?;
            if !renames.facts.defines_anything(to) {
                return None;
            }
            // clone: the repair owns its path.
            let to = Anchor::new(to.clone(), None).ok()?;
            let code = renames.facts.code(&to)?;
            Some((anchor, Repair { to, code }))
        })
        .collect()
}

/// A stale symbol anchor the search found one file for: its symbol on that
/// file is the repair when git's history shows the move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Search<'s> {
    pub(crate) anchor: &'s Anchor,
    pub(crate) there: Anchor,
}

/// The stale symbol anchors whose file is gone or defines nothing their
/// symbol names, each with the file its code may be in now: the one
/// `renames` shows its gone file at, else the one its search finds.
pub(crate) fn searched<'s>(
    asked: &Asked<'s>,
    here: &Here<'s, '_>,
    renames: &Renames,
) -> Vec<Search<'s>> {
    let missing = asked.stale.iter().copied().filter(|&anchor| {
        let missing = matches!(
            here.resolutions.get(anchor),
            Some(Resolution::SymbolMissing | Resolution::FileMissing)
        );
        anchor.symbol().is_some() && missing
    });
    let (carried, missing): (Vec<&'s Anchor>, Vec<&'s Anchor>) =
        missing.partition(|anchor| renames.to.contains_key(anchor.file()));
    let found = here.facts.namesakes(missing).into_iter().filter_map(
        |(anchor, Namesakes { at_home, elsewhere })| {
            let [target] = <[String; 1]>::try_from(elsewhere).ok()?;
            (!at_home).then_some((anchor, target))
        },
    );
    let carried = carried.into_iter().filter_map(|anchor| {
        // clone: the search owns the path its symbol is looked for on.
        Some((anchor, renames.to.get(anchor.file())?.clone()))
    });
    carried
        .chain(found)
        .filter_map(|(anchor, target)| {
            let there = Anchor::new(target, anchor.symbol().map(str::to_owned)).ok()?;
            Some(Search { anchor, there })
        })
        .collect()
}

/// Of `versions`, the anchor's file before each of its newest
/// changes, newest first, as names of `prior`: the place of the first that
/// wrote an item of the anchor's symbol, which the change after it lost.
pub(crate) fn lost<'v>(
    search: &Search<'_>,
    versions: impl IntoIterator<Item = &'v str>,
    prior: &Prior,
) -> Option<usize> {
    versions
        .into_iter()
        .position(|version| prior.wrote(version, search.anchor))
}

/// What git's history shows of a search's candidate, as names of
/// [`Prior`], around the change where the anchor's file lost the code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Shown {
    /// The anchor's file before the loss.
    pub(crate) lost: String,
    /// The candidate before the loss; `None` where no file was there.
    pub(crate) before: Option<String>,
    /// The candidate after the loss; `None` for a loss in the work
    /// tree, whose file is the tree's.
    pub(crate) after: Option<String>,
    /// The candidate after each commit that changed it since the loss.
    pub(crate) since: Vec<String>,
}

/// The repair of `search` when git's history shows the move, judged by
/// `shown` and what the tree holds now, `here`: the candidate wrote no item
/// of the symbol before the loss and, after it, held definitions with the
/// span hash the anchor's had before it; and it held definitions of the
/// same kinds after every change since and holds them now.
pub(crate) fn moved(
    search: &Search<'_>,
    shown: &Shown,
    here: &CodeFacts,
    prior: &Prior,
) -> Option<Repair> {
    let Search { anchor, there } = search;
    let held_before = shown
        .before
        .as_ref()
        .is_some_and(|before| prior.wrote(before, there));
    let (after, kinds) = match &shown.after {
        Some(after) => (prior.resolve(after, there), prior.kinds(after, there)),
        None => (here.resolve(there), here.kinds(there)),
    };
    let same = prior.resolve(&shown.lost, anchor).same_code(&after);
    let kept = shown.since.iter().all(|at| prior.kinds(at, there) == kinds);
    let now = here.kinds(there) == kinds;
    if held_before || !same || !kept || !now || kinds.is_empty() {
        return None;
    }
    let code = here.code(there)?;
    Some(Repair {
        // clone: the repair owns its anchor.
        to: there.clone(),
        code,
    })
}
