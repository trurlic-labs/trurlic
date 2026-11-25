//! Makefile definitions: each target of a rule (`ci: check test`), not a
//! variable (`name := value`, `name = value`), spanning its rule line and
//! the recipe lines after it. A line ending in `\` continues on the next,
//! a `define` block holds no rule, and a recipe line opens with a tab;
//! blank and comment lines inside a recipe neither end it nor join it.

use super::index::{FileIndex, IndexFault};
use super::kinds::DefinitionKind;
use super::lexical::Lexical;

/// The index of `text`, a Makefile: its rule targets.
pub(super) fn index(text: &str) -> Result<FileIndex, IndexFault> {
    let mut lexical = Lexical::of(text);
    let mut rule: Option<(Vec<&str>, usize, usize)> = None;
    let mut in_define = false;
    for (start, line) in logical_lines(text) {
        let end = start + line.trim_end().len();
        let directive = line.trim_start();
        if in_define {
            in_define = !directive.starts_with("endef");
        } else if line.starts_with('\t') {
            if let Some((_, _, rule_end)) = &mut rule {
                *rule_end = end;
            }
        } else if directive.is_empty() || directive.starts_with('#') {
        } else {
            if let Some(closed) = rule.take() {
                define(&mut lexical, closed)?;
            }
            in_define = is_define(directive);
            rule = targets(line).map(|targets| (targets, start, end));
        }
    }
    if let Some(closed) = rule {
        define(&mut lexical, closed)?;
    }
    Ok(lexical.finish())
}

/// Define each target of a rule over bytes `start..end`, the rule hashed
/// once for all of them.
fn define(
    lexical: &mut Lexical<'_>,
    (targets, start, end): (Vec<&str>, usize, usize),
) -> Result<(), IndexFault> {
    let hash = lexical.hash(start..end)?;
    for target in targets {
        let scope = lexical.scope(None, target)?;
        lexical.define(scope, DefinitionKind::Target, start..end, hash)?;
    }
    Ok(())
}

/// Each logical line of `text` with the byte it starts at: physical lines
/// joined while one ends in `\`.
fn logical_lines(text: &str) -> Vec<(usize, &str)> {
    let mut found = Vec::new();
    let mut start = 0;
    let mut joined_from = None;
    for line in text.split_inclusive('\n') {
        let from = *joined_from.get_or_insert(start);
        start += line.len();
        if !line.trim_end_matches(['\n', '\r']).ends_with('\\') {
            found.push((from, text.get(from..start).unwrap_or_default()));
            joined_from = None;
        }
    }
    if let Some(from) = joined_from {
        found.push((from, text.get(from..).unwrap_or_default()));
    }
    found
}

/// Whether `directive` opens a `define` block, after the modifiers that
/// may precede it.
fn is_define(directive: &str) -> bool {
    let mut words = directive.split_whitespace();
    let first = words.find(|word| !matches!(*word, "override" | "export" | "private"));
    first == Some("define")
}

/// The targets of `line` when it is a rule: the words before its first
/// `:` outside a `$(...)`, when no `=` comes first and the `:` starts no
/// assignment (`:=`, `::=`, `:::=`).
fn targets(line: &str) -> Option<Vec<&str>> {
    let code = line.split('#').next().unwrap_or_default();
    let bytes = code.as_bytes();
    let mut depth = 0_usize;
    for (at, &byte) in bytes.iter().enumerate() {
        match byte {
            b'(' | b'{' => depth += 1,
            b')' | b'}' => depth = depth.saturating_sub(1),
            b'=' if depth == 0 => return None,
            b':' if depth == 0 => {
                let colons = bytes.get(at..).unwrap_or_default();
                let run = colons.iter().take_while(|&&b| b == b':').count();
                if colons.get(run) == Some(&b'=') {
                    return None;
                }
                let words: Vec<&str> = code
                    .get(..at)
                    .unwrap_or_default()
                    .split_whitespace()
                    .collect();
                return (!words.is_empty()).then_some(words);
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use crate::code::index::{FileKind, index};
    use crate::code::testing;

    const MAKEFILE: &str = "\
.PHONY: ci test

VAR := 1
OTHER = a: b
LIST ::= x
ci: check test
\t@echo done

test: build \\
      lint
\tcargo test

\t# still the recipe
\tcargo test --doc
# a comment
define TEMPLATE
fake: rule
endef
two targets: deps
$(OUT)/%.o: %.c
\tcc $<
";

    fn named<'t>(text: &'t str, symbol: &str) -> Vec<&'t str> {
        testing::named(FileKind::Make, text, symbol)
    }

    #[test]
    fn a_rule_defines_each_target_over_its_recipe() {
        assert_eq!(named(MAKEFILE, "ci"), ["ci: check test\n\t@echo done"]);
        let test = named(MAKEFILE, "test");
        assert_eq!(test.len(), 1);
        assert!(test[0].starts_with("test: build \\\n      lint"));
        assert!(test[0].ends_with("cargo test --doc"));
        assert_eq!(named(MAKEFILE, "two"), ["two targets: deps"]);
        assert_eq!(named(MAKEFILE, "targets"), ["two targets: deps"]);
        assert_eq!(named(MAKEFILE, "$(OUT)/%.o"), ["$(OUT)/%.o: %.c\n\tcc $<"]);
        assert_eq!(named(MAKEFILE, ".PHONY").len(), 1);
    }

    #[test]
    fn the_targets_of_one_rule_share_its_hash_within_the_budget() {
        let text = format!("{}: x\n\techo\n", "t ".repeat(40_000));
        let indexed = index(FileKind::Make, text.as_bytes()).unwrap();

        assert_eq!(indexed.definitions.len(), 40_000);
        assert!(
            indexed
                .definitions
                .iter()
                .all(|d| d.hash == indexed.definitions[0].hash)
        );
    }

    #[test]
    fn a_variable_and_a_define_block_define_no_target() {
        for name in ["VAR", "OTHER", "LIST", "fake", "TEMPLATE", "lint"] {
            assert!(named(MAKEFILE, name).is_empty(), "{name}");
        }
    }
}
