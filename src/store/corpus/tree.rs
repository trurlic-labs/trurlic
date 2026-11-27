//! The source tree a corpus writes: one crate, `Cargo.toml` and
//! `src/lib.rs`, with a module per component, its directory holding a
//! `mod.rs` and [`FILES_PER_COMPONENT`] files that anchors name. Each such
//! file opens with a `//!` paragraph and `use` lines that name functions
//! of other files, then holds one function per [`SYMBOL_WORDS`] entry and
//! up to [`MAX_EXTRA_FUNCTIONS`] more, each under a `///` paragraph. The
//! tree's draws come from a stream of their own ([`SOURCE_STREAM`]), so
//! the tree changes no byte of the graph.

use std::collections::BTreeSet;
use std::fs;
use std::ops::RangeInclusive;
use std::path::Path;

use crate::{Error, Result};

use super::seeded::SplitMix64;
use super::words::{MODULE_WORDS, SYMBOL_WORDS};

pub(super) const FILES_PER_COMPONENT: usize = 64;

/// Mixed into the corpus's seed for the tree's draws, a stream apart from
/// the graph's.
const SOURCE_STREAM: u64 = 0x736f_7572_6365_7321;

const MANIFEST: &str = "[package]\nname = \"corpus\"\nversion = \"0.1.0\"\nedition = \"2024\"\n";

/// Sentences of a file's `//!` paragraph.
const FILE_DOC_SENTENCES: RangeInclusive<usize> = 1..=4;

/// Sentences of a function's `///` paragraph.
const FUNCTION_DOC_SENTENCES: RangeInclusive<usize> = 1..=3;

const MAX_EXTRA_FUNCTIONS: usize = 12;

/// `use` lines a file draws; two draws of one function are one line.
const USES: RangeInclusive<usize> = 1..=3;

/// The widest line of a doc comment, unless one word is wider.
const DOC_WIDTH: usize = 80;

/// The path of the file `file` of `component`, from the project's root.
pub(super) fn source_path(component: &str, file: usize) -> String {
    format!("src/{component}/{}.rs", module_stem(file))
}

/// File stems: [`MODULE_WORDS`] with a round suffix.
pub(super) fn module_stem(file: usize) -> String {
    let word = MODULE_WORDS[file % MODULE_WORDS.len()];
    format!("{word}_{}", file / MODULE_WORDS.len())
}

/// Write the crate of `components` in `project_dir`, drawn from `seed`.
pub(super) fn write_tree(project_dir: &Path, components: &[String], seed: u64) -> Result<()> {
    let mut rng = SplitMix64::new(seed ^ SOURCE_STREAM);
    write(&project_dir.join("Cargo.toml"), MANIFEST)?;
    let src = project_dir.join("src");
    let mut lib = doc_comment("//!", &rng.sentences(&FILE_DOC_SENTENCES));
    lib.push('\n');
    for component in components {
        lib.push_str(&declaration(component));
        let dir = src.join(component);
        fs::create_dir_all(&dir).map_err(Error::io(&dir))?;
        let mut root = doc_comment("//!", &rng.sentences(&FILE_DOC_SENTENCES));
        root.push('\n');
        for file in 0..FILES_PER_COMPONENT {
            root.push_str(&format!("pub mod {};\n", module_stem(file)));
        }
        write(&dir.join("mod.rs"), &root)?;
    }
    write(&src.join("lib.rs"), &lib)?;
    for (index, component) in components.iter().enumerate() {
        for file in 0..FILES_PER_COMPONENT {
            let text = source_file(&mut rng, components, index * FILES_PER_COMPONENT + file);
            write(&project_dir.join(source_path(component, file)), &text)?;
        }
    }
    Ok(())
}

fn write(path: &Path, text: &str) -> Result<()> {
    fs::write(path, text).map_err(Error::io(path))
}

/// The module a component is, an identifier: a suffixed component's
/// hyphen is an underscore.
fn module_name(component: &str) -> String {
    component.replace('-', "_")
}

/// The `mod` item of `component` in `src/lib.rs`: a `#[path]` names the
/// directory of a component whose name is no identifier.
fn declaration(component: &str) -> String {
    let name = module_name(component);
    match name == component {
        true => format!("pub mod {name};\n"),
        false => format!("#[path = \"{component}/mod.rs\"]\npub mod {name};\n"),
    }
}

/// The text of the source file at `place` among every component's files,
/// in component order.
fn source_file(rng: &mut SplitMix64, components: &[String], place: usize) -> String {
    let file = place % FILES_PER_COMPONENT;
    let stem = module_stem(file);
    let mut text = doc_comment("//!", &rng.sentences(&FILE_DOC_SENTENCES));
    text.push('\n');
    let files = components.len() * FILES_PER_COMPONENT;
    let uses: BTreeSet<String> = (0..rng.within(&USES))
        .map(|_| {
            let other = (place + 1 + rng.below(files - 1)) % files;
            let module = module_name(&components[other / FILES_PER_COMPONENT]);
            let stem = module_stem(other % FILES_PER_COMPONENT);
            let prefix = rng.pick(&SYMBOL_WORDS);
            format!("use crate::{module}::{stem}::{prefix}_{stem};\n")
        })
        .collect();
    text.extend(uses);
    let extra = (1..=rng.below(MAX_EXTRA_FUNCTIONS + 1))
        .map(|n| format!("{}_{stem}_{n}", SYMBOL_WORDS[n % SYMBOL_WORDS.len()]));
    let names = SYMBOL_WORDS
        .iter()
        .map(|prefix| format!("{prefix}_{stem}"))
        .chain(extra);
    for (factor, name) in (3..).zip(names) {
        text.push('\n');
        text.push_str(&doc_comment("///", &rng.sentences(&FUNCTION_DOC_SENTENCES)));
        text.push_str(&format!(
            "pub fn {name}(input: usize) -> usize {{\n    input.wrapping_mul({factor}) ^ {file}\n}}\n"
        ));
    }
    text
}

/// `text` as comment lines that open with `marker`, broken between words
/// so that no line is wider than [`DOC_WIDTH`] unless one word is.
fn doc_comment(marker: &str, text: &str) -> String {
    let mut comment = String::new();
    let mut line = marker.to_owned();
    for word in text.split(' ') {
        if line.len() > marker.len() && line.len() + 1 + word.len() > DOC_WIDTH {
            comment.push_str(&line);
            comment.push('\n');
            line = marker.to_owned();
        }
        line.push(' ');
        line.push_str(word);
    }
    comment.push_str(&line);
    comment.push('\n');
    comment
}
