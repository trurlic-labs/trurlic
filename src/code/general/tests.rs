use super::*;
use crate::code::index::{FileKind, index as index_of};
use crate::code::language::PYTHON;
use crate::code::language::python::{MAX_CONTINUATIONS, MAX_INDENTATIONS};
use crate::code::scope::Scopes;
use crate::code::span::SpanHash;
use crate::code::testing::{defined, path};

/// Each definition of `text`, a Python file, as `path kind role
/// visibility` and the text of its span.
fn statements(text: &str) -> Vec<(String, &str)> {
    let index = index_of(FileKind::Python, text.as_bytes()).unwrap();
    let scopes = Scopes {
        file: &index.scopes,
        added: &[],
    };
    index
        .definitions
        .iter()
        .map(|d| {
            let visibility = d.visibility.map_or("-".to_owned(), |v| format!("{v:?}"));
            (
                format!(
                    "{} {:?} {:?} {visibility}",
                    path(scopes, d.scope),
                    d.kind,
                    d.role
                ),
                &text[d.span.start..d.span.end],
            )
        })
        .collect()
}

/// The hash of the one definition of `text` whose path is `wanted`.
fn hash_of(text: &str, wanted: &str) -> SpanHash {
    let index = index_of(FileKind::Python, text.as_bytes()).unwrap();
    let scopes = Scopes {
        file: &index.scopes,
        added: &[],
    };
    let found: Vec<SpanHash> = index
        .definitions
        .iter()
        .filter(|d| path(scopes, d.scope) == wanted)
        .map(|d| d.hash)
        .collect();
    assert_eq!(found.len(), 1, "{wanted} in {text:?}");
    found[0]
}

#[test]
fn a_span_grows_over_its_decorators_and_a_name_to_its_statement() {
    let text = "@register\n@cache(size=2)\nclass Store:\n    limit: int = 3\n\n    @property\n    def size(self):\n        local = 1\n        self.seen = 2\n        return local\n\na = b = 1\nc, (d, [e, *f]) = 1, (2, [3, 4])\n";

    assert_eq!(
        statements(text),
        [
            (
                "Store Class Code Public".to_owned(),
                &text[..text.find("\n\na =").unwrap()]
            ),
            (
                "Store::limit Variable Code Public".to_owned(),
                "limit: int = 3"
            ),
            (
                "Store::size Method Code Public".to_owned(),
                "@property\n    def size(self):\n        local = 1\n        self.seen = 2\n        return local"
            ),
            ("a Variable Code Public".to_owned(), "a = b = 1"),
            ("b Variable Code Public".to_owned(), "a = b = 1"),
            (
                "c Variable Code Public".to_owned(),
                "c, (d, [e, *f]) = 1, (2, [3, 4])"
            ),
            (
                "d Variable Code Public".to_owned(),
                "c, (d, [e, *f]) = 1, (2, [3, 4])"
            ),
            (
                "e Variable Code Public".to_owned(),
                "c, (d, [e, *f]) = 1, (2, [3, 4])"
            ),
            (
                "f Variable Code Public".to_owned(),
                "c, (d, [e, *f]) = 1, (2, [3, 4])"
            ),
        ]
    );
}

#[test]
fn a_leading_underscore_is_private_and_a_dunder_public() {
    let text = "class _Hidden:\n    def __init__(self): pass\n    def _helper(self): pass\n    __slots__ = ()\n    __mangled = 1\n_CACHE = {}\n_ = 0\n";
    let found: Vec<String> = statements(text).into_iter().map(|(s, _)| s).collect();

    assert_eq!(
        found,
        [
            "_Hidden Class Code Private",
            "_Hidden::__init__ Method Code Public",
            "_Hidden::_helper Method Code Private",
            "_Hidden::__slots__ Variable Code Public",
            "_Hidden::__mangled Variable Code Private",
            "_CACHE Variable Code Private",
            "_ Variable Code Private",
        ]
    );
}

#[test]
fn a_test_function_a_test_class_and_what_they_hold_are_test_code() {
    let text = "def test_open():\n    def helper(): pass\nclass TestStore:\n    def setup_method(self): pass\nclass Case(unittest.TestCase):\n    def check(self): pass\ndef testing(): pass\nclass Tester2: pass\n";
    let found: Vec<String> = statements(text).into_iter().map(|(s, _)| s).collect();

    assert_eq!(
        found,
        [
            "test_open Function Test Public",
            "test_open::helper Function Test Public",
            "TestStore Class Test Public",
            "TestStore::setup_method Method Test Public",
            "Case Class Test Public",
            "Case::check Method Test Public",
            "testing Function Code Public",
            "Tester2 Class Test Public",
        ]
    );
}

/// The hash of `f`, a function whose body is `body`.
fn body_hash(body: &str) -> SpanHash {
    hash_of(&format!("def f():\n{body}"), "f")
}

#[test]
fn a_statement_moved_out_of_its_block_or_split_moves_the_hash() {
    for (one, other) in [
        ("  if a:\n    b\n    c\n", "  if a:\n    b\n  c\n"),
        ("  return\n  x\n", "  return x\n"),
        ("  x = 1\n  -1\n", "  x = 1 -1\n"),
    ] {
        assert_ne!(body_hash(one), body_hash(other), "{one:?}");
    }
}

#[test]
fn a_reformat_a_comment_or_a_docstring_leaves_the_hash() {
    let block = body_hash;
    assert_eq!(block("  a; b\n"), block("  a\n  b\n"));
    assert_eq!(block("  if a: b\n"), block("  if a:\n        b\n"));
    assert_eq!(
        block("  x = (1 +\n     2)  # sum\n"),
        block("  x = (1 + 2)\n")
    );
    assert_eq!(block("  x = 1 + \\\n    2\n"), block("  x = 1 + 2\n"));
    assert_eq!(
        block("  \"\"\"Doc.\"\"\"\n  return 1\n"),
        block("  '''Another doc.'''\n  return 1\n")
    );
    assert_eq!(block("  s = f'{ x }'\n"), block("  s = f'{x}'\n"));
}

#[test]
fn a_statement_that_only_starts_with_a_string_is_no_docstring() {
    let class = |body: &str| hash_of(&format!("class A:\n{body}"), "A");

    assert_ne!(body_hash("  \"a\", g()\n"), body_hash("  \"a\", h()\n"));
    assert_ne!(class("  \"a\", g()\n"), class("  \"a\", h()\n"));
    assert_eq!(
        class("  \"Doc.\"\n  x = 1\n"),
        class("  \"Other.\"\n  x = 1\n")
    );
}

#[test]
fn a_strings_spaces_and_prefix_move_the_hash() {
    for (one, other) in [
        ("  f\"doc\"\n  return 1\n", "  return 1\n"),
        ("  s = 'a  b'\n", "  s = 'a b'\n"),
        ("  s = f'{x: >4}'\n", "  s = f'{x:>4}'\n"),
        ("  s = rb'a'\n", "  s = b'a'\n"),
        ("  s = '#a'\n", "  s = '#b'\n"),
        ("  s = f'{x=}'\n", "  s = f'{x =}'\n"),
        ("  s = f'{x =}'\n", "  s = f'{x = }'\n"),
    ] {
        assert_ne!(body_hash(one), body_hash(other), "{one:?}");
    }
}

#[test]
fn a_definitions_hash_is_its_own_whatever_follows_it_or_holds_it() {
    let last = "class A:\n    def f(self):\n        pass\n";
    let followed = "class A:\n    def f(self):\n        pass\n    def g(self):\n        pass\n";
    let alone = "class A:\n    def f(self): pass\nx = 1\n";

    assert_eq!(hash_of(last, "A::f"), hash_of(followed, "A::f"));
    assert_eq!(hash_of(last, "A::f"), hash_of(alone, "A::f"));
    assert_ne!(hash_of(last, "A"), hash_of(followed, "A"));
}

#[test]
fn a_name_the_parser_inserted_as_missing_defines_nothing() {
    let text = "x = 1\ndef (): pass\n";

    assert_eq!(defined(FileKind::Python, text), [("x".to_owned(), "x = 1")]);
}

/// A file whose blocks nest `depth` deep, each one space deeper, whose
/// innermost line holds an f-string: the input that, past some 380
/// widths, makes tree-sitter-python's scanner abort the process.
fn nested_ifs(depth: usize) -> String {
    let mut text = String::new();
    for level in 0..depth {
        text.push_str(&format!("{}if x:\n", " ".repeat(level)));
    }
    text.push_str(&format!("{}y = f\"{{z}}\"\n", " ".repeat(depth)));
    text
}

/// [`nested_ifs`], each `if` line ending in a comment that ends in a
/// backslash, after a newline or a CR and a newline.
fn commented_ifs(depth: usize, line_break: &str) -> String {
    nested_ifs(depth).replace("if x:\n", &format!("if x:  # \\{line_break}"))
}

#[test]
fn a_comment_ending_in_a_backslash_continues_no_width_the_bound_counts() {
    let refused = Err(IndexFault::TooManyIndentations {
        max: MAX_INDENTATIONS,
    });

    for line_break in ["\n", "\r\n"] {
        let text = commented_ifs(600, line_break);
        assert_eq!(index_of(FileKind::Python, text.as_bytes()), refused);
    }
    let own_line = nested_ifs(600).replace("if x:\n", "if x:\n# \\\n");
    assert_eq!(index_of(FileKind::Python, own_line.as_bytes()), refused);
}

#[test]
fn indentations_past_the_scanners_bound_are_refused_before_the_parse() {
    assert!(
        index_of(
            FileKind::Python,
            nested_ifs(MAX_INDENTATIONS - 1).as_bytes()
        )
        .is_ok()
    );
    for depth in [MAX_INDENTATIONS + 1, 600] {
        assert_eq!(
            index_of(FileKind::Python, nested_ifs(depth).as_bytes()),
            Err(IndexFault::TooManyIndentations {
                max: MAX_INDENTATIONS
            })
        );
    }
}

#[test]
fn hostile_python_is_refused_or_read_in_linear_work() {
    let mib = 1024 * 1024;
    let cases: [(&str, String); 5] = [
        ("unclosed brackets", "(".repeat(mib)),
        (
            "nested lists",
            format!("x = {}", "[".repeat(mib / 2) + &"]".repeat(mib / 2 - 8)),
        ),
        ("chained assignments", "a = ".repeat(mib / 4 - 1) + "1\n"),
        (
            "continued lines in an error",
            continued(MAX_CONTINUATIONS).repeat(mib / continued(MAX_CONTINUATIONS).len()),
        ),
        ("unclosed strings", "\"x\n".repeat(mib / 3)),
    ];
    for (case, text) in cases {
        assert!(text.len() <= mib, "{case}");
        match index_of(FileKind::Python, text.as_bytes()) {
            Ok(_) | Err(IndexFault::TooComplex) => {}
            Err(other) => panic!("{case}: {other}"),
        }
    }
    let chain = "a = ".repeat(10_000) + "1\n";
    assert_eq!(defined(FileKind::Python, &chain).len(), 10_000);
}

/// A logical line of `count` line continuations, each followed by a name
/// the line before it does not expect: a run of them in a parse error.
fn continued(count: usize) -> String {
    format!("{}\n", "\\\nx".repeat(count))
}

#[test]
fn a_run_of_continuations_past_the_bound_is_refused_before_the_parse() {
    let refused = Err(IndexFault::TooManyContinuations {
        max: MAX_CONTINUATIONS,
    });
    // Unbounded, runs of 256 take seconds a MiB, and one run of a MiB an
    // hour, in work no budget of the parse counts.
    let hostile = continued(256).repeat((1 << 20) / continued(256).len());

    assert_eq!(index_of(FileKind::Python, hostile.as_bytes()), refused);
    assert_eq!(
        index_of(
            FileKind::Python,
            continued(MAX_CONTINUATIONS + 1).as_bytes()
        ),
        refused
    );
    assert!(index_of(FileKind::Python, continued(MAX_CONTINUATIONS).as_bytes()).is_ok());
}

#[test]
fn a_lexer_that_rescans_within_its_chunk_is_stopped_within_its_budget() {
    // Each line continuation makes the scanner read ahead within the
    // chunk the lexer holds; at 1,024 bytes a chunk this went uncounted.
    let hostile = format!("{}x\n", "\\\n".repeat(500)).repeat(1_000);

    assert_eq!(
        syntax::parse(&hostile, &tree_sitter_python::LANGUAGE.into()).err(),
        Some(IndexFault::TooComplex)
    );
}

/// The rules of `source`, a Python query of a test.
fn rules(source: &str) -> Rules {
    Rules::compile(tree_sitter_python::LANGUAGE.into(), source).unwrap()
}

fn collect_with(source: &str, text: &str) -> Result<usize, IndexFault> {
    let rules = rules(source);
    let tree = syntax::parse(text, &rules.grammar).unwrap();
    collect(&rules, &tree, text).map(|found| found.candidates.len())
}

#[test]
fn a_match_limit_that_is_hit_is_a_fault_not_a_dropped_match() {
    // Every pair of a module's statements is a match in progress.
    let pairs = "(module (_) @hash.group (_) @hash.group)";
    let few = "a\n".repeat(10);
    let many = "a\n".repeat(MATCH_LIMIT as usize * 2);

    assert!(collect_with(pairs, &few).is_ok());
    assert_eq!(collect_with(pairs, &many), Err(IndexFault::TooComplex));
}

#[test]
fn the_text_a_predicate_reads_is_charged_to_the_budget() {
    // Every block's whole text is read once per block it is in.
    let whole_blocks = "((block) @_b (#match? @_b \"never\"))";
    let payload = format!("\"{}\"", "x".repeat(1 << 19));
    let deep = nested_ifs(MAX_INDENTATIONS - 1).replace("f\"{z}\"", &payload);
    let shallow = nested_ifs(4).replace("f\"{z}\"", &payload);

    assert!(collect_with(whole_blocks, &shallow).is_ok());
    assert_eq!(
        collect_with(whole_blocks, &deep),
        Err(IndexFault::TooComplex)
    );
}

/// The Python fixture corpus, and a file of comments, continued lines and
/// strings that open like a comment.
fn python_corpus() -> Vec<(String, String)> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/code");
    let mut found = vec![(
        "lexemes".to_owned(),
        "x = 1 + \\\n    2  # sum\ny = '#not' f'{x: >4} #' \\\n  \"\"\"# doc\"\"\"\n#!z\n"
            .to_owned(),
    )];
    let mut pending = vec![root];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if FileKind::of(path.to_str().unwrap()) == Some(FileKind::Python) {
                let text = std::fs::read_to_string(&path).unwrap();
                found.push((path.display().to_string(), text));
            }
        }
    }
    found
}

#[test]
fn every_python_byte_but_extras_and_whitespace_is_in_one_token() {
    for (file, text) in python_corpus() {
        let tree = syntax::parse(&text, &tree_sitter_python::LANGUAGE.into()).unwrap();
        let mut covered = vec![false; text.len()];
        syntax::tokens(tree.root_node(), &text, &PYTHON.lexicon, |token| {
            let (from, to) = match token {
                syntax::Token::Node(node) => (node.start_byte(), node.end_byte()),
                syntax::Token::Gap { start, end } => (start, end),
                syntax::Token::Enter(_) | syntax::Token::Leave(_) => (0, 0),
            };
            for byte in &mut covered[from..to] {
                assert!(!*byte, "{file}: byte {from} in two tokens");
                *byte = true;
            }
            std::ops::ControlFlow::Continue(())
        });
        let mut extras = vec![false; text.len()];
        let mut cursor = tree.walk();
        'walk: loop {
            let node = cursor.node();
            if syntax::is_comment(node) {
                assert!(
                    matches!(node.kind(), "comment" | "line_continuation"),
                    "{file}: an extra {}",
                    node.kind()
                );
                extras[node.byte_range()].fill(true);
            } else if cursor.goto_first_child() {
                continue;
            }
            while !cursor.goto_next_sibling() {
                if !cursor.goto_parent() {
                    break 'walk;
                }
            }
        }
        for (at, c) in text.char_indices() {
            assert!(
                covered[at] || extras[at] || c.is_whitespace(),
                "{file}: byte {at} ({c:?}) is in no token"
            );
        }
    }
}
