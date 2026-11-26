//! A decision's `code_refs` of format 0.4.1 as the `anchors` of 0.5.
//!
//! A ref keeps its place: a path with an empty or `.` segment loses it, and
//! a symbol takes the spelling of the 0.5 symbol grammar, a `::`-separated
//! path of names from the outermost container, where the old spelling maps
//! to one name path by its syntax alone. 0.4.1 wrote a TOML or JSON key
//! path with `.`, a Python member as `Class.member`, and a call or a macro
//! with its `(..)` or `!`; none of those characters is part of a name of
//! its language. A spelling whose meaning the syntax does not settle is
//! kept as written and reported. A ref whose path breaks the anchor rules
//! is dropped, and a symbol 0.5 refuses or a key 0.5 does not hold is
//! dropped from it; each drop is a [`Loss`](crate::store::format::migration::Loss).

use toml::Value;

use super::super::{Fact, File, RefText};
use super::Notes;
use crate::store::model::Anchor;

/// The anchors of the `code_refs` of `file`, with what was rewritten, kept
/// or dropped on the way noted.
pub(super) fn anchors(file: &File, code_refs: Value, notes: &mut Notes<'_>) -> Vec<Value> {
    let Value::Array(refs) = code_refs else {
        notes.loss(file, "decision.code_refs", &code_refs);
        return Vec::new();
    };
    let mut anchors = Vec::with_capacity(refs.len());
    for (at, code_ref) in refs.into_iter().enumerate() {
        let field = format!("decision.code_refs[{at}]");
        let mut migrated = Migrated {
            file,
            field: &field,
            notes: &mut *notes,
        };
        if let Some(anchor) = migrated.anchor(code_ref) {
            anchors.push(anchor);
        }
    }
    anchors
}

/// One ref of `file` at `field`, and where its changes are noted.
struct Migrated<'a, 'n> {
    file: &'a File,
    field: &'a str,
    notes: &'a mut Notes<'n>,
}

impl Migrated<'_, '_> {
    /// The anchor `code_ref` becomes, as an inline table; `None` when it is
    /// dropped.
    fn anchor(&mut self, code_ref: Value) -> Option<Value> {
        let Value::Table(mut fields) = code_ref else {
            self.notes.loss(self.file, self.field, &code_ref);
            return None;
        };
        let written = match fields.remove("file") {
            Some(Value::String(written)) => written,
            other => {
                // The ref is reported whole, a `file` of another type included.
                if let Some(file) = other {
                    fields.insert("file".into(), file);
                }
                self.notes
                    .loss(self.file, self.field, &Value::Table(fields));
                return None;
            }
        };
        let symbol = fields.remove("symbol");
        self.notes.rest(self.file, self.field, fields);
        let Some(path) = one_spelling(&written) else {
            self.drop_ref(written, symbol);
            return None;
        };
        let old = RefText {
            file: written,
            symbol: symbol.as_ref().and_then(Value::as_str).map(str::to_owned),
        };
        let new = RefText {
            symbol: symbol.and_then(|symbol| self.symbol(&path, symbol)),
            file: path,
        };
        let respelled = matches!((&old.symbol, &new.symbol), (Some(old), Some(new)) if old != new);
        let value = new.to_value();
        if respelled || old.file != new.file {
            self.notes.facts.push(Fact::Normalized {
                file: self.file.to_string(),
                old,
                new,
            });
        }
        Some(value)
    }

    /// Report the ref of `file` and `symbol` dropped whole.
    fn drop_ref(&mut self, file: String, symbol: Option<Value>) {
        let mut dropped = toml::Table::new();
        dropped.insert("file".into(), file.into());
        if let Some(symbol) = symbol {
            dropped.insert("symbol".into(), symbol);
        }
        self.notes
            .loss(self.file, self.field, &Value::Table(dropped));
    }

    /// The symbol of an anchor at `path`, in the 0.5 spelling where its
    /// syntax settles one; `None` when 0.5 refuses it, which drops it from
    /// the anchor.
    fn symbol(&mut self, path: &str, symbol: Value) -> Option<String> {
        let field = format!("{}.symbol", self.field);
        let Value::String(written) = symbol else {
            self.notes.loss(self.file, &field, &symbol);
            return None;
        };
        let (spelled, kept) = match spelling(path, &written) {
            Spelling::Grammar(spelled) => (spelled, None),
            // clone: the anchor and the report each hold the symbol as written.
            Spelling::Kept(why) => (written.clone(), Some(why)),
        };
        let Ok(anchor) = Anchor::new(path.to_owned(), Some(spelled)) else {
            self.notes.loss(self.file, &field, &Value::String(written));
            return None;
        };
        if let Some(why) = kept {
            self.notes.facts.push(Fact::Unnormalized {
                file: self.file.to_string(),
                anchor: RefText {
                    file: path.to_owned(),
                    symbol: Some(written),
                },
                why,
            });
        }
        anchor.symbol().map(str::to_owned)
    }
}

/// `written` in the one spelling of an anchor path: without its empty and
/// `.` segments, which name no directory, unless it is absolute. `None`
/// when the anchor rules refuse that spelling too.
fn one_spelling(written: &str) -> Option<String> {
    if Anchor::new(written.to_owned(), None).is_ok() {
        return Some(written.to_owned());
    }
    if written.starts_with('/') {
        return None;
    }
    let path: Vec<&str> = written
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .collect();
    Anchor::new(path.join("/"), None)
        .ok()
        .map(|anchor| anchor.file().to_owned())
}

/// What the 0.5 grammar makes of a symbol written for 0.4.1.
#[derive(Debug, PartialEq, Eq)]
enum Spelling {
    /// The symbol in the 0.5 spelling, which may be the old one.
    Grammar(String),
    /// The old spelling, kept: its syntax does not settle a name path.
    Kept(&'static str),
}

/// How a file kind spells its symbols, told by the file's name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Rust,
    Python,
    /// TOML and JSON, whose symbols are key paths.
    Keys,
    Markdown,
    /// A Makefile and every kind 0.4.1 had no rule for.
    Other,
}

fn kind(path: &str) -> Kind {
    let extension = path.rsplit_once('.').map(|(_, extension)| extension);
    match extension {
        Some("rs") => Kind::Rust,
        Some("py" | "pyi") => Kind::Python,
        Some("toml" | "json") => Kind::Keys,
        Some("md") => Kind::Markdown,
        _ => Kind::Other,
    }
}

const QUOTED_KEY: &str = "a quoted key, whose `.` may be part of a name";

const EMPTY_SEGMENT: &str = "an empty segment of a key path";

const GENERIC: &str = "a generic or an `impl`, which 0.5 writes as `Type::<Trait>::name`";

const RUST_DOT: &str = "a `.`, which no Rust path holds";

const MARKDOWN: &str = "a word of a heading, where 0.5 names the heading path";

fn spelling(path: &str, written: &str) -> Spelling {
    let symbol = written.trim();
    match kind(path) {
        Kind::Keys => keys(symbol),
        Kind::Markdown => Spelling::Kept(MARKDOWN),
        Kind::Rust => {
            let name = uncalled(symbol);
            if name.contains(['<', '>', ' ']) {
                Spelling::Kept(GENERIC)
            } else if name.contains('.') {
                Spelling::Kept(RUST_DOT)
            } else {
                Spelling::Grammar(name.to_owned())
            }
        }
        Kind::Python => Spelling::Grammar(uncalled(symbol).replace('.', "::")),
        Kind::Other => Spelling::Grammar(uncalled(symbol).to_owned()),
    }
}

/// A TOML or JSON key path: `a.b` is `a::b`.
fn keys(symbol: &str) -> Spelling {
    if symbol.contains(['"', '\'', '[', ']']) {
        return Spelling::Kept(QUOTED_KEY);
    }
    if symbol.contains("::") || !symbol.contains('.') {
        return Spelling::Grammar(symbol.to_owned());
    }
    let segments: Vec<&str> = symbol.split('.').collect();
    if segments.iter().any(|segment| segment.is_empty()) {
        return Spelling::Kept(EMPTY_SEGMENT);
    }
    Spelling::Grammar(segments.join("::"))
}

/// `symbol` without the `(..)` of a call or the `!` of a macro.
fn uncalled(symbol: &str) -> &str {
    let name = match symbol.find('(') {
        Some(open) if symbol.ends_with(')') => symbol.get(..open).unwrap_or(symbol),
        _ => symbol,
    };
    name.trim_end().trim_end_matches('!')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_old_spelling_its_syntax_settles_takes_the_grammar() {
        for (path, old, new) in [
            (
                "Cargo.toml",
                "workspace.lints.clippy",
                "workspace::lints::clippy",
            ),
            ("final.json", "layers.order", "layers::order"),
            ("Cargo.toml", "package", "package"),
            ("py/mod.py", "Point.seed", "Point::seed"),
            ("py/mod.pyi", "Phase", "Phase"),
            ("src/a.rs", "Store::load_state()", "Store::load_state"),
            ("src/a.rs", "identifier!", "identifier"),
            ("src/a.rs", "format!(..)", "format"),
            ("src/a.rs", " padded ", "padded"),
            ("Makefile", "ci-test", "ci-test"),
        ] {
            assert_eq!(spelling(path, old), Spelling::Grammar(new.into()), "{old}");
        }
    }

    #[test]
    fn a_path_takes_its_one_spelling_unless_no_spelling_is_below_the_root() {
        for (written, path) in [
            ("src/a.rs", Some("src/a.rs")),
            ("./src//a.rs", Some("src/a.rs")),
            ("src/./store/", Some("src/store")),
            ("/etc/passwd", None),
            ("//etc/passwd", None),
            ("./../a.rs", None),
            ("src\\a.rs", None),
            ("./", None),
        ] {
            assert_eq!(one_spelling(written).as_deref(), path, "{written}");
        }
    }

    /// A ref whose `file` is no string is dropped and reported whole, its
    /// `file` included.
    #[test]
    fn a_ref_with_a_file_of_another_type_is_reported_whole() {
        let code_refs: Value = toml::from_str::<toml::Table>("refs = [{ file = 7, line = 3 }]")
            .unwrap()
            .remove("refs")
            .unwrap();
        let (mut facts, mut losses) = (Vec::new(), Vec::new());
        let mut notes = Notes {
            facts: &mut facts,
            losses: &mut losses,
        };

        let kept = anchors(&File::Decision("d".into()), code_refs, &mut notes);

        assert_eq!(kept, []);
        let reported: Vec<(&str, &str)> = losses
            .iter()
            .map(|loss| (loss.field.as_str(), loss.value.as_str()))
            .collect();
        assert_eq!(
            reported,
            [("decision.code_refs[0]", "{ file = 7, line = 3 }")]
        );
    }

    #[test]
    fn an_old_spelling_its_syntax_does_not_settle_is_kept() {
        for (path, old, why) in [
            ("Cargo.toml", "tool.\"a.b\"", QUOTED_KEY),
            ("Cargo.toml", "a..b", EMPTY_SEGMENT),
            ("src/a.rs", "Display<Config>", GENERIC),
            ("src/a.rs", "impl Store", GENERIC),
            ("src/a.rs", "self.field", RUST_DOT),
            ("README.md", "sweep", MARKDOWN),
        ] {
            assert_eq!(spelling(path, old), Spelling::Kept(why), "{old}");
        }
    }
}
