use super::*;
use crate::code::BlankMacros;
use crate::code::index::FileKind;
use crate::code::language::python::MAX_INDENTATIONS;
use crate::code::scope::{ScopeTree, Scopes};
use crate::code::span::{Span, SpanHash};
use crate::code::testing::{defined, index_in, named, path};

/// Each definition of `text`, a Python file, as `path kind role
/// visibility` and the text of its span.
fn statements(text: &str) -> Vec<(String, &str)> {
    statements_of(FileKind::Python, text)
}

/// Each definition of `text`, a file of `kind`, as `path kind role
/// visibility` and the text of its span.
fn statements_of(kind: FileKind, text: &str) -> Vec<(String, &str)> {
    let index = crate::code::testing::index(kind, text.as_bytes()).unwrap();
    let scopes = Scopes(&index.scopes);
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
    hash_in(FileKind::Python, text, wanted)
}

/// The hash of the one definition of `text`, a file of `kind`, whose
/// path is `wanted`.
fn hash_in(kind: FileKind, text: &str, wanted: &str) -> SpanHash {
    let index = crate::code::testing::index(kind, text.as_bytes()).unwrap();
    let scopes = Scopes(&index.scopes);
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
    // A comment inside an `=` interpolation is no part of its output, nor
    // is one or a space inside a format specifier's nested field.
    for (one, other) in [
        ("f\"\"\"{x # c\n=}\"\"\"", "f\"\"\"{x # d\n=}\"\"\""),
        ("f\"\"\"{x= # c\n}\"\"\"", "f\"\"\"{x= \n}\"\"\""),
        ("f\"\"\"{x:>{w # c\n}}\"\"\"", "f\"\"\"{x:>{w}}\"\"\""),
        ("f'{x:>{ w }}'", "f'{x:>{w}}'"),
        ("f'{x=:>{ w }}'", "f'{x=:>{w}}'"),
    ] {
        let line = |string: &str| block(&format!("  s = {string}\n"));
        assert_eq!(line(one), line(other), "{one:?}");
    }
}

#[test]
fn a_formatters_quotes_prefix_case_and_trailing_commas_leave_the_hash() {
    for (one, other) in [
        ("s = 'a'", "s = \"a\""),
        ("s = u'a'", "s = 'a'"),
        (
            "s = R'a' + Rb'a' + F'{x!r}'",
            "s = r\"a\" + br\"a\" + f\"{x!r}\"",
        ),
        ("s = '''a'''", "s = \"\"\"a\"\"\""),
        ("f(a, b,)", "f(a, b)"),
        ("f(\n    a,\n    b,  # last\n)", "f(a, b)"),
        ("x = [1, 2,] + [3,]", "x = [1, 2] + [3]"),
        ("x = {1: 2,} | {3,}", "x = {1: 2} | {3}"),
        ("x = (1, 2,)", "x = (1, 2)"),
        ("def g(a, *b, **c,): pass", "def g(a, *b, **c): pass"),
        ("from m import (a, b,)", "from m import (a, b)"),
        ("(a, b,) = [c, d,] = e", "a, b = [c, d] = e"),
        ("x = (\n    a + b\n)", "x = a + b"),
        ("x += ((a))", "x += a"),
        ("return (a, b)", "return a, b"),
        ("del (a, b)", "del a, b"),
        ("for (a, b) in (c):\n    pass", "for a, b in c:\n    pass"),
        (
            "if (a):\n    pass\n  elif (b):\n    pass",
            "if a:\n    pass\n  elif b:\n    pass",
        ),
        ("return a or \\\n    'b'", "return a or 'b'"),
        ("assert (f(x)) is (None)", "assert f(x) is None"),
        (
            "y = -(x**2) + 1. + .5J + 0XAB",
            "y = -x**2 + 1.0 + 0.5j + 0xab",
        ),
        (
            "assert (\n    a is b\n), (\n    'm'\n)",
            "assert a is b, 'm'",
        ),
        ("s = '\\xAB\\N{em dash}'", "s = '\\xab\\N{EM DASH}'"),
        ("class C(): pass", "class C: pass"),
        ("s = r'\\\\'", "s = r\"\\\\\""),
        ("s = r'\\\"' + br'\\''", "s = r\"\\\"\" + rb\"\\'\""),
        (
            "s = rf'\\'{x}' + r'''\\\"'''",
            "s = rf\"\\'{x}\" + r\"\"\"\\\"\"\"\"",
        ),
        (
            "s = 'it\\'s' + b'\\'' + f'\\'{x}'",
            "s = \"it's\" + b\"'\" + f\"'{x}\"",
        ),
        ("s = \"\\\"\" + '\\\\\\''", "s = '\"' + \"\\\\'\""),
        ("y = 1e+5 + 1E+5j", "y = 1e5 + 1e5j"),
        ("x = a, b,", "x = (\n    a,\n    b,\n)"),
        ("return a, b,", "return a, b"),
        ("a, b, = c", "a, b = c"),
        ("x = y[a, b,]", "x = y[\n    a,\n    b,\n]"),
        (
            "def g(x: (A | B) = 1, y: C[d, e,] = 2) -> (\n    F | G\n): pass",
            "def g(x: A | B = 1, y: C[d, e] = 2) -> F | G: pass",
        ),
        ("x: (\n    A | None\n) = None", "x: A | None = None"),
        (
            "f((a if b else c), k=(d if e else f))",
            "f(a if b else c, k=d if e else f)",
        ),
        (
            "x = [(a if b else c) for d in e], {k: (a if b else c)}, ((a if b else c), d)",
            "x = [a if b else c for d in e], {k: a if b else c}, (a if b else c, d)",
        ),
        (
            "x = a if b else (c if d else e)",
            "x = a if b else c if d else e",
        ),
        ("x = lambda: (a or not b)", "x = lambda: a or not b"),
        ("from m import (\n    a,\n)", "from m import a"),
        (
            "with (\n    a() as b,\n    c(),\n):\n    pass",
            "with a() as b, c():\n    pass",
        ),
        ("x = [d for (d,) in e]", "x = [d for d, in e]"),
        ("x = yield (\n    a,\n    b,\n)", "x = yield a, b"),
        ("(a,)", "a,"),
        ("f((\n    'a'\n    'b'\n))", "f('ab')"),
    ] {
        let line = |statement: &str| body_hash(&format!("  {statement}\n"));
        assert_eq!(line(one), line(other), "{one:?}");
    }
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
fn what_a_string_or_a_comma_means_moves_the_hash() {
    for (one, other) in [
        ("  f\"doc\"\n  return 1\n", "  return 1\n"),
        ("  s = 'a  b'\n", "  s = 'a b'\n"),
        ("  s = f'{x: >4}'\n", "  s = f'{x:>4}'\n"),
        ("  s = f'{d:{s} %d}'\n", "  s = f'{d:{s}%d}'\n"),
        ("  s = rb'a'\n", "  s = b'a'\n"),
        ("  s = '#a'\n", "  s = '#b'\n"),
        ("  s = f'{x=}'\n", "  s = f'{x =}'\n"),
        ("  s = f'{x =}'\n", "  s = f'{x = }'\n"),
        // The text of an `=` interpolation is its output, as written.
        ("  s = f'{\"a\"=}'\n", "  s = f\"{'a'=}\"\n"),
        ("  s = f'{f(a,)=}'\n", "  s = f'{f(a)=}'\n"),
        ("  x = (1,)\n", "  x = (1)\n"),
        ("  x = y[1,]\n", "  x = y[1]\n"),
        ("  (a,) = b\n", "  (a) = b\n"),
        ("  return ()\n", "  return\n"),
        ("  x = (a + b) * c\n", "  x = a + b * c\n"),
        ("  f((yield a), b)\n", "  f((yield a, b))\n"),
        ("  x = (yield a), b\n", "  x = (yield a, b)\n"),
        ("  x = (-a)**2\n", "  x = -a**2\n"),
        ("  x = ('a' 'b').upper()\n", "  x = 'a' + 'b'.upper()\n"),
        // Parentheses bind a conditional expression or a yield to what
        // follows them, and a tuple in an annotation or a target.
        ("  x = (a if b else c) + d\n", "  x = a if b else c + d\n"),
        (
            "  x = (a if b else c) if d else e\n",
            "  x = a if b else c if d else e\n",
        ),
        (
            "  f = lambda: (yield a), b\n",
            "  f = lambda: (yield a, b)\n",
        ),
        ("  def g(x: (a, b)): pass\n", "  def g(x: a, b): pass\n"),
        (
            "  with (a, b) as c:\n    pass\n",
            "  with a, b as c:\n    pass\n",
        ),
        ("  x = [d for (d,) in e]\n", "  x = [d for d in e]\n"),
        ("  (a,)\n", "  a\n"),
        ("  yield (a, b), c\n", "  yield a, b, c\n"),
        ("  yield (a,)\n", "  yield a\n"),
        ("  x = 1\n", "  x = 1.0\n"),
        ("  assert (yield a), b\n", "  assert (yield a, b)\n"),
        ("  s = '\\T'\n", "  s = '\\t'\n"),
        ("  s = '\\N{x}'\n", "  s = '\\n{x}'\n"),
        ("  s = r'\\''\n", "  s = r\"\\\"\"\n"),
        // A raw string's backslash is its text; a quote escape's is not.
        ("  s = r'\\''\n", "  s = '\\''\n"),
        ("  s = rf'\\'{x}'\n", "  s = f'\\'{x}'\n"),
        ("  s = '\\''\n", "  s = '\\\\\\''\n"),
        ("  s = '\\8'\n", "  s = '\\008'\n"),
        ("  s = '\\U0001F600'\n", "  s = '\\u0001F600'\n"),
        ("  y = 1e+5\n", "  y = 1e-5\n"),
        ("  x = a, b,\n", "  x = a, (b,)\n"),
        ("  a, b, = c\n", "  a, (b,) = c\n"),
        ("  x = y[a, b,]\n", "  x = y[(a, b),]\n"),
    ] {
        assert_ne!(body_hash(one), body_hash(other), "{one:?}");
    }
}

#[test]
fn implicitly_concatenated_strings_that_start_alike_hash_as_one() {
    for (one, other) in [
        ("'a' 'b'", "'ab'"),
        ("u'a' \"b\" '' 'c'", "'abc'"),
        ("(\n    'a'  # c\n    'b'\n)", "'ab'"),
        ("'a' \\\n    'b'", "'ab'"),
        ("r'\\d' R'\\w'", "r'\\d\\w'"),
        ("b'\\x41' B\"\\'\"", "b'\\x41\\''"),
        ("'\\0' '1'", "'\\0001'"),
    ] {
        let line = |string: &str| body_hash(&format!("  s = {string}\n"));
        assert_eq!(line(one), line(other), "{one:?}");
    }
}

#[test]
fn concatenated_strings_keep_what_each_part_means() {
    for (one, other) in [
        // An octal escape ends where its part does.
        ("'\\0' '1'", "'\\01'"),
        ("'\\1' '23'", "'\\123'"),
        // A raw part's backslashes and an escape's are not one text.
        ("r'\\n' 'b'", "'\\nb'"),
        ("'\\n' r'b'", "r'\\nb'"),
        ("'\\\\' 'n'", "'\\n'"),
        // An f-string part is no text to join: its braces are fields.
        ("'{x}' f'a'", "f'{x}a'"),
        ("f'{x}' '{y}'", "f'{x}{y}'"),
    ] {
        let line = |string: &str| body_hash(&format!("  s = {string}\n"));
        assert_ne!(line(one), line(other), "{one:?}");
    }
}

#[test]
fn a_lone_carriage_return_ends_a_line_as_python_reads_it() {
    let lines = "class C:\n    def m(self):\n        return 1\n    s = '''a\nb'''\n";
    let returns = lines.replace('\n', "\r");
    let edited = returns.replace("return 1", "return 2");
    let read = |text: &str| {
        let index = crate::code::testing::index(FileKind::Python, text.as_bytes()).unwrap();
        let scopes = Scopes(&index.scopes);
        let defined: Vec<(String, Span, SpanHash)> = index
            .definitions
            .iter()
            .map(|d| (path(scopes, d.scope), d.span, d.hash))
            .collect();
        defined
    };

    assert_eq!(read(&returns), read(lines), "spans, their lines and hashes");
    assert_ne!(hash_of(&edited, "C::m"), hash_of(&returns, "C::m"));
    assert_ne!(hash_of(&edited, "C"), hash_of(&returns, "C"));
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
        assert_eq!(
            crate::code::testing::index(FileKind::Python, text.as_bytes()),
            refused
        );
    }
    let own_line = nested_ifs(600).replace("if x:\n", "if x:\n# \\\n");
    assert_eq!(
        crate::code::testing::index(FileKind::Python, own_line.as_bytes()),
        refused
    );
}

#[test]
fn indentations_past_the_scanners_bound_are_refused_before_the_parse() {
    assert!(
        crate::code::testing::index(
            FileKind::Python,
            nested_ifs(MAX_INDENTATIONS - 1).as_bytes()
        )
        .is_ok()
    );
    for depth in [MAX_INDENTATIONS + 1, 600] {
        assert_eq!(
            crate::code::testing::index(FileKind::Python, nested_ifs(depth).as_bytes()),
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
        ("continued lines in an error", "\\\nx".repeat(mib / 3)),
        ("unclosed strings", "\"x\n".repeat(mib / 3)),
    ];
    for (case, text) in cases {
        assert!(text.len() <= mib, "{case}");
        match crate::code::testing::index(FileKind::Python, text.as_bytes()) {
            Ok(_) | Err(IndexFault::TooComplex) => {}
            Err(other) => panic!("{case}: {other}"),
        }
    }
    let chain = "a = ".repeat(10_000) + "1\n";
    assert_eq!(defined(FileKind::Python, &chain).len(), 10_000);
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

/// The path of each definition `source`, a Python query of a test, places
/// in `text`, in the sweep's order.
fn placed_with(source: &str, text: &str) -> Vec<String> {
    let rules = rules(source);
    let tree = syntax::parse(text, &rules.grammar).unwrap();
    let found = collect(&rules, &tree, text).unwrap();
    let mut tree_of_scopes = ScopeTree::for_file(FileKind::Python, text.len());
    let placed = place::place(&found, &mut tree_of_scopes).unwrap();
    let file = tree_of_scopes.into_scopes();
    let scopes = Scopes(&file);
    placed.iter().map(|it| path(scopes, it.scope)).collect()
}

#[test]
fn of_two_within_marks_of_one_node_the_one_visibility_orders_first_holds() {
    let function =
        "((function_definition name: (identifier) @name) @definition (#set! kind \"function\"))";
    let within =
        |visibility: Visibility| format!("(module) @visibility.within.{}", visibility.name());

    // The order of `Visibility::ALL`, a cache entry's codes, is the order
    // the rule reads: a private mark holds over any other.
    for (first, one) in Visibility::ALL.iter().enumerate() {
        for (second, other) in Visibility::ALL.iter().enumerate() {
            let source = format!("{function}\n{}\n{}", within(*one), within(*other));
            let rules = rules(&source);
            let text = "def f(): pass\n";
            let tree = syntax::parse(text, &rules.grammar).unwrap();
            let found = collect(&rules, &tree, text).unwrap();
            let placed = place::place(
                &found,
                &mut ScopeTree::for_file(FileKind::Python, text.len()),
            )
            .unwrap();
            let expected = Visibility::ALL[first.min(second)];

            assert_eq!(placed[0].what.visibility, Some(expected), "{source}");
        }
    }
    assert_eq!(Visibility::ALL[0], Visibility::Private);
}

#[test]
fn two_definitions_of_one_node_are_siblings() {
    let both = "((function_definition name: (identifier) @name) @definition (#set! kind \"function\"))\n\
                ((function_definition name: (identifier) @name) @definition (#set! kind \"class\"))";

    assert_eq!(
        placed_with(both, "def f():\n    def g(): pass\n"),
        ["f", "f", "f::g", "f::g"]
    );
}

#[test]
fn of_two_definitions_from_one_byte_the_wider_holds_the_other() {
    let calls = "((call function: [(identifier) @name (call function: (identifier) @name)]) @definition (#set! kind \"function\"))";

    assert_eq!(placed_with(calls, "f(1)(2)\n"), ["f", "f::f"]);
}

#[test]
fn a_definition_its_container_drops_leaves_the_unnamed_scope_of_its_node() {
    let dropped = "((function_definition name: (identifier) @name) @definition (#set! kind \"function\") (#set! within \"class\"))\n\
                   ((class_definition name: (identifier) @name) @definition (#set! kind \"class\"))\n\
                   ((assignment left: (identifier) @name) @definition (#set! kind \"variable\"))\n\
                   (function_definition) @scope.unnamed";

    assert_eq!(
        placed_with(dropped, "def f():\n    x = 1\n"),
        Vec::<String>::new()
    );
}

#[test]
fn a_node_captured_as_no_scope_holds_what_it_holds_where_it_stands() {
    let none = "((assignment left: (identifier) @name) @definition (#set! kind \"variable\"))\n\
                (function_definition) @scope.unnamed\n\
                (module (function_definition) @scope.none)";

    assert_eq!(
        placed_with(none, "def f():\n    x = 1\n    def g():\n        y = 2\n"),
        ["x"]
    );
}

#[test]
fn a_match_limit_that_is_hit_is_a_fault_not_a_dropped_match() {
    // Every pair of a module's statements is a match in progress: 129
    // statements stay within the limit, and 130 pass it.
    let pairs = "(module (_) @hash.group (_) @hash.group)";

    assert!(collect_with(pairs, &"a\n".repeat(129)).is_ok());
    assert_eq!(
        collect_with(pairs, &"a\n".repeat(130)),
        Err(IndexFault::TooComplex)
    );
}

#[test]
fn the_text_a_predicate_reads_is_held_to_16_bytes_a_byte_of_the_file() {
    // Every block's whole text is read once per block it is in: twenty
    // blocks deep, a payload is read twenty times. A comment the query
    // reads nothing of dilutes that: one byte shorter it is past the
    // bound, and with that byte it is within.
    let whole_blocks = "((block) @_b (#match? @_b \"never\"))";
    let payload = format!("\"{}\"", "x".repeat(100_000));
    let deep = nested_ifs(20).replace("f\"{z}\"", &payload);
    let text = |comment: usize| format!("# {}\n{deep}", "p".repeat(comment));

    assert!(collect_with(whole_blocks, &text(24_905)).is_ok());
    assert_eq!(
        collect_with(whole_blocks, &text(24_904)),
        Err(IndexFault::TooComplex)
    );
}

/// The fixture corpus of each language the general resolver reads, and a
/// file per language of comments, continued lines and strings that open
/// like a comment.
fn general_corpus() -> Vec<(String, FileKind, String)> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/code");
    let mut found = vec![
        (
            "lexemes.py".to_owned(),
            FileKind::Python,
            "x = 1 + \\\n    2  # sum\ny = '#not' f'{x: >4} #' \\\n  \"\"\"# doc\"\"\"\n#!z\n"
                .to_owned(),
        ),
        (
            "lexemes.tsx".to_owned(),
            FileKind::Tsx,
            "#!/usr/bin/env node\nconst t = `//a ${b /* c */} /*d*/`; // e\nconst r = /\\/\\*x/g;\n<!-- f\nconst j = <p> a  // b </p>;\n"
                .to_owned(),
        ),
        (
            "lexemes.cc".to_owned(),
            FileKind::Cpp,
            "#define M(a) ((a) + \\\n  1) // m\nint x = 1 + \\\n  2; /* c */\nauto s = R\"x(/* no */)x\" \"//\";\n"
                .to_owned(),
        ),
    ];
    let mut pending = vec![root];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if let Some(kind) =
                FileKind::of(path.to_str().unwrap()).filter(|kind| kind.language().is_some())
            {
                let text = std::fs::read_to_string(&path).unwrap();
                found.push((path.display().to_string(), kind, text));
            }
        }
    }
    found
}

#[test]
fn every_byte_but_comments_and_whitespace_is_in_one_token() {
    for (file, kind, text) in general_corpus() {
        let language = kind.language().unwrap();
        let rules = language.rules().unwrap();
        let tree = syntax::parse(&text, &rules.grammar).unwrap();
        let mut covered = vec![false; text.len()];
        syntax::tokens(tree.root_node(), &text, &language.lexicon, |token| {
            let (from, to) = match token {
                syntax::Token::Node(node) => (node.start_byte(), node.end_byte()),
                syntax::Token::Gap { start, end } | syntax::Token::Run { start, end } => {
                    (start, end)
                }
                syntax::Token::Hole { .. } | syntax::Token::Enter(_) | syntax::Token::Leave(_) => {
                    (0, 0)
                }
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
                    matches!(
                        node.kind(),
                        "comment" | "html_comment" | "line_continuation"
                    ),
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
                covered[at]
                    || extras[at]
                    || c.is_whitespace()
                    || language.lexicon.continuation(&text[at..]) > 0,
                "{file}: byte {at} ({c:?}) is in no token"
            );
        }
    }
}

/// Each definition of `text`, a TypeScript file, as [`statements_of`]
/// writes it, without its span.
fn typescript(text: &str) -> Vec<String> {
    statements_of(FileKind::TypeScript, text)
        .into_iter()
        .map(|(statement, _)| statement)
        .collect()
}

#[test]
fn an_exclusive_pattern_claims_its_node_from_the_others() {
    // A name bound to an arrow function is a function alone, not also
    // the variable every other binding is.
    assert_eq!(
        typescript("const f = () => 1;\nconst g = 2;\n"),
        ["f Function Code Private", "g Variable Code Private"]
    );
}

#[test]
fn each_declarator_of_a_statement_holds_what_its_own_node_holds() {
    for (text, expected) in [
        (
            "const a = 1, f = () => 2;\n",
            &["a Variable Code Private", "f Function Code Private"][..],
        ),
        (
            "export const f = () => { function inner() {} }, a = 1;\n",
            &[
                "f Function Code Public",
                "a Variable Code Public",
                "f::inner Function Code Private",
            ][..],
        ),
        (
            "const g = () => { const h = () => 1; }, k = () => {};\n",
            &[
                "g Function Code Private",
                "k Function Code Private",
                "g::h Function Code Private",
            ][..],
        ),
    ] {
        assert_eq!(typescript(text), expected, "{text}");
    }
}

#[test]
fn a_span_takes_in_the_decorators_beside_it() {
    let text = "class A {\n  @first() // why\n  @second\n  m() {}\n  n() {}\n}\n";
    let spans: Vec<(String, &str)> = statements_of(FileKind::TypeScript, text);

    assert_eq!(
        spans[1],
        (
            "A::m Method Code Public".to_owned(),
            "@first() // why\n  @second\n  m() {}"
        )
    );
    assert_eq!(spans[2].1, "n() {}");
    let decorated = |decorator: &str| {
        hash_in(
            FileKind::TypeScript,
            &format!("class A {{\n  {decorator}\n  m() {{}}\n}}\n"),
            "A::m",
        )
    };
    assert_ne!(decorated("@first()"), decorated("@second()"));
}

#[test]
fn a_name_splits_at_its_separator_and_drops_a_string_literals_quotes() {
    assert_eq!(
        typescript(
            "namespace A . /* inner */ B { function f() {} }\ndeclare module \"pkg.name\" {}\nenum E { \"a b\" }\n"
        ),
        [
            "A::B Module Code Private",
            "A::B::f Function Code Private",
            "pkg.name Module Code Public",
            "E Enum Code Private",
            "E::a b Variant Code Public",
        ]
    );
    assert_eq!(
        typescript("class K { [\"computed\"]() {} 'quoted name'() {} [Symbol.iterator]() {} }\n"),
        [
            "K Class Code Private",
            "K::computed Method Code Public",
            "K::quoted name Method Code Public",
            "K::[Symbol.iterator] Method Code Public",
        ]
    );
}

/// The words of the `#any-of?` predicate on `capture` in TypeScript's
/// query: the runners' names or their members.
fn runner_words(capture: &str) -> Vec<&'static str> {
    let query = include_str!("../language/typescript.scm");
    let needle = format!("(#any-of? {capture} ");
    let start = query.find(&needle).unwrap() + needle.len();
    let line = &query[start..start + query[start..].find(')').unwrap()];
    line.split_whitespace()
        .map(|word| word.trim_matches('"'))
        .collect()
}

#[test]
fn a_test_call_is_named_by_its_strings_as_written() {
    let text = "describe('Store', () => {\n  it(`opens ${kind}`, () => {});\n  test.each([1])(\"a::b %s\", () => {});\n  it.skip('later', () => {});\n  it(name, () => {});\n  it('', () => {});\n});\n";

    assert_eq!(
        typescript(text),
        [
            "Store Test Test -",
            "Store::opens ${kind} Test Test -",
            "Store::a::b %s Test Test -",
            "Store::later Test Test -",
        ]
    );
    let runners = runner_words("@_runner");
    let members = runner_words("@_member");
    assert!(runners.contains(&"describe") && members.contains(&"only"));
    for runner in &runners {
        assert_eq!(
            typescript(&format!("{runner}('t', () => {{}});\n")),
            ["t Test Test -"],
            "{runner}"
        );
    }
    for member in &members {
        assert_eq!(
            typescript(&format!("describe.{member}('t', () => {{}});\n")),
            ["t Test Test -"],
            "{member}"
        );
    }
}

#[test]
fn a_member_chain_of_a_runner_is_a_test_when_a_function_follows_its_string() {
    let text = "describe.for([1])('grp', () => { it('a', () => {}); });\ndescribe.runIf(x)('run', () => {});\ntest.describe.serial('ser', () => {});\nit.concurrent.only('both', async () => {}, 10);\ntest.fail('details', { tag: '@x' }, async () => {});\ntest.extend({})('fixture', function () {});\ncontext.only('ctx', () => { it('in', () => {}); });\ntest.describe.parallel.only('deep', () => { test('in', () => {}); });\ntest.concurrent.only.each([1])('%i', () => {});\nit.only('named', testFn);\ntest('top', async (t) => { await t.test('sub', async () => {}); });\n";

    assert_eq!(
        typescript(text),
        [
            "grp Test Test -",
            "grp::a Test Test -",
            "run Test Test -",
            "ser Test Test -",
            "both Test Test -",
            "details Test Test -",
            "fixture Test Test -",
            "ctx Test Test -",
            "ctx::in Test Test -",
            "deep Test Test -",
            "deep::in Test Test -",
            "%i Test Test -",
            "named Test Test -",
            "top Test Test -",
        ]
    );
}

#[test]
fn a_call_of_a_runners_name_without_a_function_after_its_string_or_of_another_member_is_no_test() {
    let text = "function draw(context, test, suite) {\n  context.clip('evenodd');\n  test.set('Content-Type', 'text/plain');\n  context.waitForEvent('page');\n  context.request.get('/a', { headers });\n  context.route('/b', async (route) => {});\n  suite.add('RegExp#test', function () {});\n  suite.on('cycle', function (event) {});\n  describe.bind(null)('w', () => {});\n  test('abc');\n  const ok = test('value');\n  if (test('x')) {}\n  it('pending');\n  test('x', {});\n  rows.only('x', () => {});\n  rows.each(table)('x', () => {});\n  rows.describe.only('x', () => {});\n  rows.only.skip.each(t)('x', () => {});\n}\n";

    assert_eq!(typescript(text), ["draw Function Code Private"]);
}

#[test]
fn an_export_states_what_its_module_makes_public() {
    let text = "export function a() {}\nfunction b() {}\nfunction c() {}\nfunction d() {}\nfunction e() {}\nexport { b, c as see };\nexport { d } from './d';\nexport default e;\nexport namespace N { function hidden() {} export function shown() {} }\n";

    assert_eq!(
        typescript(text),
        [
            "a Function Code Public",
            "b Function Code Public",
            "c Function Code Public",
            "d Function Code Private",
            "e Function Code Public",
            "N Module Code Public",
            "N::hidden Function Code Private",
            "N::shown Function Code Public",
        ]
    );
    assert_eq!(
        statements_of(
            FileKind::Tsx,
            "function f() {}\nfunction g() {}\nmodule.exports = { f, other: g };\n"
        )
        .into_iter()
        .map(|(statement, _)| statement)
        .collect::<Vec<_>>(),
        ["f Function Code Public", "g Function Code Public"]
    );
    assert_eq!(
        typescript("function f() {}\nexport = f;\n"),
        ["f Function Code Public"]
    );
    assert_eq!(
        typescript("namespace N { function f() {} }\nfunction f() {}\nexport { f };\n"),
        [
            "N Module Code Private",
            "N::f Function Code Private",
            "f Function Code Public",
        ]
    );
}

#[test]
fn an_ambient_module_or_namespace_exports_what_it_holds_at_any_depth() {
    let text = "declare module \"foo\" { function f(): void; interface I { a: number } }\ndeclare namespace NS { function g(): void; namespace B { function h(): void; } }\ndeclare global { interface Window { x: 1 } }\ndeclare function local(): void;\n";

    assert_eq!(
        typescript(text),
        [
            "foo Module Code Public",
            "foo::f Function Code Public",
            "foo::I Interface Code Public",
            "foo::I::a Field Code Public",
            "NS Module Code Private",
            "NS::g Function Code Public",
            "NS::B Module Code Public",
            "NS::B::h Function Code Public",
            "Window Interface Code Public",
            "Window::x Field Code Public",
            "local Function Code Private",
        ]
    );
}

#[test]
fn an_ambient_module_with_an_export_form_exports_only_what_it_marks() {
    let text = "declare module \"*.svg\" { const content: string; function helper(): void; export default content; }\ndeclare module \"lib\" { function f(): void; export function g(): void; export {}; }\ndeclare module \"m\" { class C { private p: number; q: number } }\n";

    assert_eq!(
        typescript(text),
        [
            "*.svg Module Code Public",
            "*.svg::content Variable Code Public",
            "*.svg::helper Function Code Private",
            "lib Module Code Public",
            "lib::f Function Code Private",
            "lib::g Function Code Public",
            "m Module Code Public",
            "m::C Class Code Public",
            "m::C::p Field Code Private",
            "m::C::q Field Code Public",
        ]
    );
    // Each checked against tsc: an export declaration or assignment makes
    // the rest private, `export default class {}` and `export as
    // namespace` do not, and a name an export list or `export =` names is
    // exported.
    for (body, expected) in [
        (
            "export default class {}",
            &["m::f Function Code Public", "m::default Class Code Public"][..],
        ),
        ("export * from \"n\";", &["m::f Function Code Private"][..]),
        (
            "export * as ns from \"n\";",
            &["m::f Function Code Private"][..],
        ),
        ("export = g;", &["m::f Function Code Private"][..]),
        ("export = f;", &["m::f Function Code Public"][..]),
        (
            "export default N.g;",
            &[
                "m::f Function Code Private",
                "m::default Variable Code Public",
            ][..],
        ),
        ("export { f };", &["m::f Function Code Public"][..]),
        ("export { g };", &["m::f Function Code Private"][..]),
    ] {
        let text = format!("declare module \"m\" {{ function f(): void; {body} }}\n");
        assert_eq!(&typescript(&text)[1..], expected, "{body}");
    }
    assert_eq!(
        typescript(
            "declare namespace N { function f(): void; function g(): void; export { f }; }\n"
        ),
        [
            "N Module Code Private",
            "N::f Function Code Public",
            "N::g Function Code Private",
        ]
    );
}

#[test]
fn a_declaration_files_top_exports_what_typescript_reads_as_exported() {
    let declarations = |text: &str| {
        statements_of(FileKind::TypeScriptDeclarations, text)
            .into_iter()
            .map(|(statement, _)| statement)
            .collect::<Vec<_>>()
    };
    // Each checked against tsc: a script's declarations are global, a
    // module's exported unless it holds an export declaration or
    // assignment.
    assert_eq!(
        declarations("declare function f(): void;\ninterface I { a: string }\n"),
        [
            "f Function Code Public",
            "I Interface Code Public",
            "I::a Field Code Public"
        ]
    );
    assert_eq!(
        declarations(
            "import { X } from \"./x\";\ndeclare function f(): void;\nexport declare function g(): void;\nexport default class {}\n"
        ),
        [
            "f Function Code Public",
            "g Function Code Public",
            "default Class Code Public"
        ]
    );
    assert_eq!(
        declarations(
            "declare function f(): void;\ndeclare function g(): void;\nexport declare function h(): void;\nexport { f };\n"
        ),
        [
            "f Function Code Public",
            "g Function Code Private",
            "h Function Code Public"
        ]
    );
    for form in [
        "export {};",
        "export * from \"./x\";",
        "export * as x from \"./x\";",
        "export = g;",
        "export default g;",
        "export default N.g;",
    ] {
        assert_eq!(
            declarations(&format!("declare function f(): void;\n{form}\n"))[0],
            "f Function Code Private",
            "{form}"
        );
    }
    assert_eq!(
        typescript("declare function f(): void;\nexport declare function g(): void;\n"),
        ["f Function Code Private", "g Function Code Public"]
    );
}

#[test]
fn an_unnamed_default_export_is_named_default() {
    for (text, expected) in [
        (
            "export default class { m() {} }\n",
            &["default Class Code Public", "default::m Method Code Public"][..],
        ),
        (
            "export default function () {}\n",
            &["default Function Code Public"][..],
        ),
        (
            "export default { m() {}, n: () => 1, v: 2 };\n",
            &[
                "default Variable Code Public",
                "default::m Method Code Public",
                "default::n Method Code Public",
            ][..],
        ),
        (
            "export default defineConfig({});\n",
            &["default Variable Code Public"][..],
        ),
        (
            "const x = 1;\nexport default x;\n",
            &["x Variable Code Public"][..],
        ),
        (
            "export default 'text';\n",
            &["default Variable Code Public"][..],
        ),
        ("export default 42;\n", &[][..]),
        (
            "module.exports = function createApp() { function inner() {} };\n",
            &[
                "createApp Function Code Public",
                "createApp::inner Function Code Private",
            ][..],
        ),
        (
            "module.exports = function* () {};\n",
            &["default Function Code Public"][..],
        ),
        (
            "module.exports = function* made() {};\n",
            &["made Function Code Public"][..],
        ),
    ] {
        assert_eq!(typescript(text), expected, "{text}");
    }
}

#[test]
fn an_object_literals_methods_are_defined_only_under_a_top_level_const() {
    let text = "const api = { get() {} };\nlet loose = { get() {} };\nfunction f() { const local = { get() {} }; }\nconst outer = (() => { const inner = { get() {} }; return inner; })();\n";

    assert_eq!(
        typescript(text),
        [
            "api Variable Code Private",
            "api::get Method Code Public",
            "loose Variable Code Private",
            "f Function Code Private",
            "outer Variable Code Private",
        ]
    );
}

#[test]
fn what_an_unnamed_function_holds_is_under_the_definition_around_it_or_nothing() {
    for (text, expected) in [
        (
            "module.exports = (compiler) => { const { a } = compiler; let css = false; function helper() {} };\n",
            &[
                "default Function Code Public",
                "default::helper Function Code Private",
            ][..],
        ),
        (
            "jest.mock('a', () => { const original = 1; });\njest.mock('b', () => { const original = 2; });\n",
            &[][..],
        ),
        (
            "module.exports = { create(context) { const x = 1; function f() {} } };\n",
            &[][..],
        ),
        (
            "namespace N { run(() => { const x = 1; }); }\n",
            &["N Module Code Private"][..],
        ),
        (
            "describe('a', () => { const x = 1; function f() {} });\n",
            &["a Test Test -", "a::f Function Test Private"][..],
        ),
        (
            "namespace N { run(function () { const x = 1; }); run(function* () { const y = 1; }); }\n",
            &["N Module Code Private"][..],
        ),
        (
            "app.get('/', (req) => { const a = 1; function h() {} });\n",
            &[][..],
        ),
    ] {
        assert_eq!(typescript(text), expected, "{text}");
    }
}

#[test]
fn a_wrapper_of_the_whole_file_is_the_module() {
    let wrapped = |shape: &str| {
        let text = shape
            .replace('F', "function () { function api() {} var x = 1; }")
            .replace('A', "() => { function api() {} var x = 1; }");
        typescript(&format!("{text}\n"))
    };
    // Each shape a statement of the file's top calls a wrapper in, F a
    // function and A an arrow function; then each that passes a factory.
    for shape in [
        "(F)();",
        "(A)();",
        "(F).call(this);",
        "(A).apply(this);",
        "(F());",
        "((A)());",
        "(F.call(this));",
        "((A).call(this));",
        "!F();",
        "void F();",
        "!(A)();",
        "!F.call(this);",
        "!(F).apply(this);",
        "(function (r, f) {})(this, F);",
        "(function (r, f) {}).call(this, this, A);",
        "(function (r, f) {})(this, (F));",
        "(function (r, f) {}(this, (A)));",
        "((function (r, f) {})(this, F));",
        "(function (r, f) {}.call(this, this, F));",
        "!function (r, f) {}(this, F);",
        "!(function (r, f) {})(this, A);",
        "!function (r, f) {}.call(this, this, (F));",
        "define(['a'], F);",
        "define(A);",
    ] {
        assert_eq!(
            wrapped(shape),
            ["api Function Code Private", "x Variable Code Private"],
            "{shape}"
        );
    }
    for shape in [
        "module.exports = (F)();",
        "module.exports = (A).call(this);",
        "module.exports = (F());",
        "module.exports = (F.call(this));",
        "exports = module.exports = (F)();",
    ] {
        assert_eq!(
            wrapped(shape),
            [
                "default Variable Code Public",
                "default::api Function Code Private"
            ],
            "{shape}"
        );
    }
    for shape in ["module.exports = F;", "exports = module.exports = A;"] {
        assert_eq!(
            wrapped(shape),
            [
                "default Function Code Public",
                "default::api Function Code Private"
            ],
            "{shape}"
        );
    }
    for shape in [
        "(F).toString();",
        "x = (F)();",
        "(F)() + 1;",
        "if (x) (F)();",
    ] {
        assert_eq!(wrapped(shape), Vec::<String>::new(), "{shape}");
    }
    assert_eq!(
        typescript("function f() { (function () { function deep() {} })(); }\n"),
        ["f Function Code Private", "f::deep Function Code Private"]
    );
}

#[test]
fn a_class_expression_is_a_class_where_a_name_holds_it() {
    for (text, expected) in [
        (
            "module.exports = class Compiler { run() {} };\n",
            &[
                "Compiler Class Code Public",
                "Compiler::run Method Code Public",
            ][..],
        ),
        (
            "X.Template = class XTemplate { apply() {} };\n",
            &[
                "XTemplate Class Code Private",
                "XTemplate::apply Method Code Public",
            ][..],
        ),
        (
            "const C = class D { m() {} };\n",
            &["C Class Code Private", "C::m Method Code Public"][..],
        ),
        (
            "module.exports = class { n() {} };\n",
            &["default Class Code Public", "default::n Method Code Public"][..],
        ),
        (
            "exports = module.exports = class { n() {} };\n",
            &["default Class Code Public", "default::n Method Code Public"][..],
        ),
        ("register(class Local { n() {} });\n", &[][..]),
        (
            "function o() { return class V { m() {} f = 1; }; }\n",
            &["o Function Code Private"][..],
        ),
        (
            "export const mixin = (B) => class extends B { m() {} };\n",
            &["mixin Function Code Public"][..],
        ),
        (
            "function outer() { let X; X ??= class V { m() {} f = 1; }; }\n",
            &[
                "outer Function Code Private",
                "outer::V Class Code Private",
                "outer::V::m Method Code Public",
                "outer::V::f Field Code Public",
            ][..],
        ),
    ] {
        assert_eq!(typescript(text), expected, "{text}");
    }
}

/// The hash of `f`, a TypeScript function whose body is `body`.
fn typescript_body(body: &str) -> SpanHash {
    body_in(FileKind::TypeScript, body)
}

/// The hash of `f`, a function of a file of `kind` whose body is `body`.
fn body_in(kind: FileKind, body: &str) -> SpanHash {
    hash_in(kind, &format!("function f() {{\n{body}}}\n"), "f")
}

#[test]
fn a_statement_split_by_a_line_break_moves_a_typescript_hash() {
    for (one, other) in [
        ("  return\n  x\n", "  return x\n"),
        ("  a\n  ++b\n", "  a++\n  b\n"),
        (
            "  switch (a) {\n    case 1: b()\n      return\n      c\n  }\n",
            "  switch (a) {\n    case 1: b()\n      return c\n  }\n",
        ),
        (
            "  switch (a) {\n    default: b()\n      return\n      c\n  }\n",
            "  switch (a) {\n    default: b()\n      return c\n  }\n",
        ),
    ] {
        assert_ne!(typescript_body(one), typescript_body(other), "{one:?}");
    }
}

#[test]
fn a_reformat_a_comment_or_a_semicolon_leaves_a_typescript_hash() {
    let block = typescript_body;
    assert_eq!(block("  a; b;\n"), block("  a\n  b\n"));
    assert_eq!(
        block("  const x = 1 +\n    2; // sum\n"),
        block("  const x = 1 + 2\n")
    );
    assert_eq!(block("  /* c */ return a\n"), block("  return a;\n"));
    assert_eq!(
        block("  const t = `a ${ b /* c */ } c`;\n"),
        block("  const t = `a ${b} c`;\n")
    );
    let members = |body: &str| hash_in(FileKind::TypeScript, &format!("interface I {body}\n"), "I");
    assert_eq!(
        members("{ a: string; b(): void }"),
        members("{\n  a: string,\n  b(): void\n}")
    );
    assert_eq!(
        members("{ a: string; b(): void }"),
        members("{\n  a: string\n  b(): void\n}")
    );
    assert_eq!(jsx("<div>{/* c */}</div>"), jsx("<div></div>"));
    assert_eq!(
        jsx("<div>{/* a */}{/* b */}</div>"),
        jsx("<div>{/* a */}</div>")
    );
    assert_ne!(jsx("<p>{a}</p>"), jsx("<p>a</p>"));
    assert_eq!(jsx("<div>{}</div>"), jsx("<div></div>"));
    assert_eq!(jsx("<p>a{/* c */}b</p>"), jsx("<p>ab</p>"));
    // Semicolon-free code guards a statement that starts with `[`, `(` or
    // a template with a `;`, an empty statement of its own after a `}`.
    assert_eq!(
        block("  switch (a) {\n    case 1: ;[b] = c\n    default: ;(d)()\n  }\n"),
        block("  switch (a) {\n    case 1: [b] = c;\n    default: (d)();\n  }\n")
    );
    for (guarded, ended) in [
        (
            "  if (a) {}\n  ;[1].map(g)\n",
            "  if (a) {}\n  [1].map(g);\n",
        ),
        ("  ;({ a } = b)\n", "  ({ a } = b);\n"),
        ("  ;`a`.at(0)\n", "  `a`.at(0);\n"),
    ] {
        assert_eq!(block(guarded), block(ended), "{guarded:?}");
    }
}

/// Pairs of a body as prettier writes it under two values of one option,
/// each pair from its output.
const PRETTIER_OPTIONS: [(&str, &str); 18] = [
    ("  a = \"x\" + 'y'\n", "  a = 'x' + \"y\"\n"),
    ("  import(\"m\")\n", "  import('m')\n"),
    ("  a = \"it's \\\"x\\\"\"\n", "  a = 'it\\'s \"x\"'\n"),
    ("  a = \"\\\\'\"\n", "  a = '\\\\\\''\n"),
    (
        "  let t: [...A[], B]\n",
        "  let t: [\n    ...A[],\n    B,\n  ]\n",
    ),
    (
        "  let t: [a: A, b?: B]\n",
        "  let t: [\n    a: A,\n    b?: B,\n  ]\n",
    ),
    ("  f(a, b)\n", "  f(\n    a,\n    b,\n  )\n"),
    (
        "  const o = { a, b }\n",
        "  const o = {\n    a,\n    b,\n  }\n",
    ),
    (
        "  const l = [a, b]\n",
        "  const l = [\n    a,\n    b,\n  ]\n",
    ),
    (
        "  const [a, b] = l\n",
        "  const [\n    a,\n    b,\n  ] = l\n",
    ),
    (
        "  const { a, b } = o\n",
        "  const {\n    a,\n    b,\n  } = o\n",
    ),
    ("  g = (a, b) => a\n", "  g = (\n    a,\n    b,\n  ) => a\n"),
    (
        "  g = new M<A, B>()\n",
        "  g = new M<\n    A,\n    B,\n  >()\n",
    ),
    ("  let t: [A, B]\n", "  let t: [\n    A,\n    B,\n  ]\n"),
    ("  enum E { A, B }\n", "  enum E {\n    A,\n    B,\n  }\n"),
    ("  g = (x) => x\n", "  g = x => x\n"),
    ("  g = async (x) => x\n", "  g = async x => x\n"),
    ("  h((x) => x)\n", "  h(x => x)\n"),
];

#[test]
fn prettiers_quotes_trailing_commas_and_arrow_parentheses_leave_a_typescript_hash() {
    for (one, other) in PRETTIER_OPTIONS {
        assert_eq!(typescript_body(one), typescript_body(other), "{one:?}");
    }
    // An import or an export list is a statement of a module's body,
    // which an ambient module's hash holds.
    let module = |text: &str| hash_in(FileKind::TypeScript, text, "m");
    assert_eq!(
        module("declare module \"m\" {\n  import { a, b } from \"n\";\n  export { a, b };\n}\n"),
        module(
            "declare module 'm' {\n  import {\n    a,\n    b,\n  } from 'n';\n  export {\n    a,\n    b,\n  };\n}\n"
        )
    );
    assert_eq!(jsx("<a b=\"c\" />"), jsx("<a b='c' />"));
    for element in ["a", "{ a }", "[a]", "a?", "this: T", "a: T = 1"] {
        let parameters =
            |comma: &str| typescript_body(&format!("  g = function (b, {element}{comma}) {{}}\n"));
        assert_eq!(parameters(""), parameters(","), "{element}");
    }
    for element in ["a: b", "a", "a = 1"] {
        let pattern =
            |comma: &str| typescript_body(&format!("  ({{ c, {element}{comma} }} = o)\n"));
        assert_eq!(pattern(""), pattern(","), "{element}");
    }
    for element in [
        "a",
        "a.b",
        "a[0]",
        "a!",
        "{ a }",
        "[a]",
        "a = 1",
        "undefined",
    ] {
        let pattern = |comma: &str| typescript_body(&format!("  [c, {element}{comma}] = l\n"));
        assert_eq!(pattern(""), pattern(","), "{element}");
    }
}

/// Pairs of a body as prettier writes it at two line widths.
const PRETTIER_WIDTHS: [(&str, &str); 10] = [
    (
        "  let t: [A | B, C]\n",
        "  let t: [\n    (\n      | A\n      | B\n    ),\n    C,\n  ]\n",
    ),
    (
        "  type R<T, U> = T extends U ? 0 : (U extends T ? 1 : 0)\n",
        "  type R<T, U> = T extends U\n    ? 0\n    : U extends T\n      ? 1\n      : 0\n",
    ),
    (
        "  type S<T, U> = T extends U ? (U extends T ? 1 : 0) : 0\n",
        "  type S<T, U> = T extends U\n    ? U extends T\n      ? 1\n      : 0\n    : 0\n",
    ),
    ("  return a && b\n", "  return (\n    a &&\n    b\n  )\n"),
    ("  throw a || b\n", "  throw (\n    a ||\n    b\n  )\n"),
    (
        "  g = (x) => (x ? a : b)\n",
        "  g = (x) =>\n    x ? a : b\n",
    ),
    ("  g = c ? d : e\n", "  g = c\n    ? (d)\n    : (e)\n"),
    (
        "  type V = \"a\" | \"b\"\n",
        "  type V =\n    | \"a\"\n    | \"b\"\n",
    ),
    ("  type W = A & B\n", "  type W =\n    & A\n    & B\n"),
    (
        "  let u: A | undefined\n",
        "  let u:\n    | A\n    | undefined\n",
    ),
];

#[test]
fn the_parentheses_and_operators_a_line_width_adds_leave_a_typescript_hash() {
    for (one, other) in PRETTIER_WIDTHS {
        assert_eq!(typescript_body(one), typescript_body(other), "{one:?}");
    }
    let tsx = |body: &str| body_in(FileKind::Tsx, body);
    for (one, other) in [
        (
            "  return <div>a</div>\n",
            "  return (\n    <div>a</div>\n  )\n",
        ),
        ("  const v = <p />\n", "  const v = (\n    <p />\n  )\n"),
        (
            "  const v = <p>a</p>\n",
            "  const v = (\n    <p>a</p>\n  )\n",
        ),
        ("  g = () => <p />\n", "  g = () => (\n    <p />\n  )\n"),
        ("  g = c && <p />\n", "  g = c && (\n    <p />\n  )\n"),
        (
            "  g = c ? <p /> : <q />\n",
            "  g = c ? (\n    <p />\n  ) : (\n    <q />\n  )\n",
        ),
    ] {
        assert_eq!(tsx(one), tsx(other), "{one:?}");
    }
}

/// Pairs of bodies that a token left out of the hash elsewhere tells
/// apart: each means something else, or one of them is no program.
const MEANINGS: [(&str, &str); 23] = [
    ("  let t: (A | B)[]\n", "  let t: A | B[]\n"),
    ("  let t: [(A | B)?]\n", "  let t: [A | B?]\n"),
    ("  a = \"a\\\\'\"\n", "  a = \"a'\"\n"),
    ("  g = () => ({})\n", "  g = () => {}\n"),
    ("  g = c ? d : (a, b)\n", "  g = c ? d : a, b\n"),
    ("  return (\n    a\n  )\n", "  return\n  a\n"),
    ("  a\n  ;(b)\n", "  a\n  (b)\n"),
    ("  a\n  ;[b]\n", "  a\n  [b]\n"),
    ("  if (a);\n  b\n", "  if (a)\n  b\n"),
    ("  g = (a + b) * c\n", "  g = a + b * c\n"),
    ("  g = (c ? d : e).f\n", "  g = c ? d : e.f\n"),
    ("  s = '\\\\\\''\n", "  s = \"'\"\n"),
    ("  s = `a`\n", "  s = \"a\"\n"),
    ("  l = [a, , ]\n", "  l = [a, ]\n"),
    ("  l = [,]\n", "  l = []\n"),
    ("  l = [a, /* c */ ,]\n", "  l = [a,]\n"),
    ("  const [a, , ] = l\n", "  const [a, ] = l\n"),
    ("  const [...a, ] = l\n", "  const [...a] = l\n"),
    ("  const { ...a, } = o\n", "  const { ...a } = o\n"),
    ("  g = (...a, ) => a\n", "  g = (...a) => a\n"),
    ("  (x: T) => x\n", "  x: T => x\n"),
    ("  (x): T => x\n", "  x: T => x\n"),
    ("  g = (x = y) => 1\n", "  g = x = y => 1\n"),
];

#[test]
fn a_token_the_hash_leaves_out_elsewhere_moves_it_where_it_means_something() {
    for (one, other) in MEANINGS {
        assert_ne!(typescript_body(one), typescript_body(other), "{one:?}");
    }
    let tsx = |body: &str| body_in(FileKind::Tsx, body);
    assert_ne!(
        tsx("  g = <p>\n    {\" \"}\n  </p>\n"),
        tsx("  g = <p>\n     \n  </p>\n")
    );
}

#[test]
fn a_literals_text_moves_a_typescript_hash() {
    for (one, other) in [
        ("  s = 'a  b'\n", "  s = 'a b'\n"),
        ("  s = `a  b`\n", "  s = `a b`\n"),
        ("  s = `${a}b`\n", "  s = `${a} b`\n"),
        ("  r = /a b/\n", "  r = /a  b/\n"),
        ("  r = /a/g\n", "  r = /a/i\n"),
        ("  s = '//a'\n", "  s = '//b'\n"),
        ("  s = `//a`\n", "  s = `//b`\n"),
    ] {
        assert_ne!(typescript_body(one), typescript_body(other), "{one:?}");
    }
}

#[test]
fn a_definition_inside_a_template_strings_interpolation_is_found() {
    assert_eq!(
        typescript("const t = `${(() => { function inner() {} })()}`;\n"),
        ["t Variable Code Private", "t::inner Function Code Private"]
    );
}

/// The hash of `View`, a TSX function that returns `element`.
fn jsx(element: &str) -> SpanHash {
    hash_in(
        FileKind::Tsx,
        &format!("function View() {{\n  return {element};\n}}\n"),
        "View",
    )
}

#[test]
fn jsx_text_is_hashed_as_jsx_renders_it() {
    for (one, other) in [
        ("<p>\n    a b\n  </p>", "<p>a b</p>"),
        ("<p>a  \n    b</p>", "<p>a b</p>"),
        (
            "<p><b>Hi</b>{\" \"}\n  there friend</p>",
            "<p><b>Hi</b> there friend</p>",
        ),
        ("<p>a{' '}b</p>", "<p>a b</p>"),
        ("<p>a{\"  \"}b</p>", "<p>a  b</p>"),
        (
            "<p>Copyright &copy;\n  2024</p>",
            "<p>Copyright &copy; 2024</p>",
        ),
        ("<p>&amp;\n  &amp;</p>", "<p>&amp; &amp;</p>"),
        ("<p>&amp;  \n&amp;</p>", "<p>&amp; &amp;</p>"),
        ("<p>  \n&amp;</p>", "<p>&amp;</p>"),
    ] {
        assert_eq!(jsx(one), jsx(other), "{one:?}");
    }
    for (one, other) in [
        ("<p>a   b</p>", "<p>a b</p>"),
        ("<p>a\tb</p>", "<p>a b</p>"),
        ("<pre>  a</pre>", "<pre> a</pre>"),
        ("<p>a b</p>", "<p>a c</p>"),
        ("<p>a b</p>", "<p>ab</p>"),
        ("<p><b>Hi</b> there</p>", "<p><b>Hi</b>there</p>"),
        ("<p>&amp;\n&amp;</p>", "<p>&amp;&amp;</p>"),
        ("<p>a\n{/* c */}b</p>", "<p>a b</p>"),
    ] {
        assert_ne!(jsx(one), jsx(other), "{one:?}");
    }
}

#[test]
fn hostile_typescript_is_refused_or_read_in_linear_work() {
    let mib = 1024 * 1024;
    let cases: [(&str, String); 9] = [
        // Each keeps an arrow's and a conditional's readings alive.
        (
            "conditional arrows",
            format!("x={}", "a?(b):c=>".repeat((mib - 2) / 9)),
        ),
        (
            "spaced conditional arrows",
            format!("x={}", "a ? (b) : c => ".repeat((mib - 2) / 15)),
        ),
        ("unclosed brackets", "(".repeat(mib)),
        ("unclosed template strings", "`${".repeat(mib / 3)),
        (
            "nested generics",
            format!("let x: {}", "A<".repeat(mib / 2 - 8)),
        ),
        (
            "nested elements",
            format!("x = {}{}", "<a>".repeat(mib / 7), "</a>".repeat(mib / 7)),
        ),
        ("generic calls", "x<y>(z".repeat(mib / 6)),
        (
            "lines automatic semicolons end",
            "return\nx\n".repeat(mib / 9),
        ),
        ("unclosed regular expressions", "/[/".repeat(mib / 3)),
    ];
    for (case, text) in cases {
        assert!(text.len() <= mib, "{case}");
        for kind in [FileKind::TypeScript, FileKind::Tsx] {
            match crate::code::testing::index(kind, text.as_bytes()) {
                Ok(_) | Err(IndexFault::TooComplex) => {}
                Err(other) => panic!("{case}: {other}"),
            }
        }
    }
}

/// Each definition of `text`, a C++ file, as [`statements_of`] writes it,
/// without its span.
fn cpp(text: &str) -> Vec<String> {
    statements_of(FileKind::Cpp, text)
        .into_iter()
        .map(|(statement, _)| statement)
        .collect()
}

#[test]
fn a_name_captured_on_several_nodes_is_their_segments_in_order() {
    assert_eq!(
        cpp("TEST(Shelf, Opens) {}\nTEST_F(Shelf, Closes) {}\nCHECK(Shelf, Fills) {}\n"),
        ["Shelf::Opens Test Test -", "Shelf::Closes Test Test -"]
    );
}

#[test]
fn a_skipped_kind_is_no_part_of_a_name() {
    assert_eq!(
        cpp(
            "template <> class Box<int> {};\ntemplate <typename T> T Box<T>::get() {}\nA::operator bool() const {}\n"
        ),
        [
            "Box Class Code Public",
            "Box::get Function Code Public",
            "A::operator bool Function Code Public",
        ]
    );
}

#[test]
fn a_space_stands_in_a_name_only_between_two_words() {
    let text = "bool A::operator == (A o) {}\nA::operator  bool() {}\nA::~ A() {}\n";

    assert_eq!(
        cpp(text),
        [
            "A::operator== Function Code Public",
            "A::operator bool Function Code Public",
            "A::~A Function Code Public",
        ]
    );
    for symbol in ["A::operator==", "A::operator bool", "A::~A()"] {
        assert_eq!(named(FileKind::Cpp, text, symbol).len(), 1, "{symbol}");
    }
}

#[test]
fn a_label_states_the_visibility_of_what_follows_it() {
    let text = "class A {\n  int a;\n public:\n  static int b() {}\n#if X\n  int c;\n#endif\n protected:\n  struct B { int d; };\n};\n\
                struct C { int e; private: int f; };\nnamespace { int g; }\nstatic int h;\nint i;\n";

    assert_eq!(
        cpp(text),
        [
            "A Class Code Public",
            "A::a Field Code Private",
            "A::b Method Code Public",
            "A::c Field Code Public",
            "A::B Struct Code Protected",
            "A::B::d Field Code Public",
            "C Struct Code Public",
            "C::e Field Code Public",
            "C::f Field Code Private",
            "g Variable Code Private",
            "h Variable Code Private",
            "i Variable Code Public",
        ]
    );
}

#[test]
fn a_label_in_a_preprocessor_arm_holds_past_it_and_none_holds_in_a_body() {
    let text = "class A {\n#ifdef T\n public:\n  int a;\n#else\n protected:\n#endif\n  int b;\n\
                private:\n  union { int c; };\n  struct { int d; } e;\n  void f() { struct G { int h; }; }\n};\n";

    assert_eq!(
        cpp(text),
        [
            "A Class Code Public",
            "A::a Field Code Public",
            "A::b Field Code Protected",
            "A::c Field Code Private",
            "A::e Field Code Private",
            "A::e::d Field Code Public",
            "A::f Method Code Private",
            "A::f::G Struct Code Public",
            "A::f::G::h Field Code Public",
        ]
    );
}

#[test]
fn a_macros_body_is_hashed_by_its_tokens_whatever_their_spacing() {
    let macro_hash = |text: &str| hash_in(FileKind::C, text, "M");
    let one = macro_hash("#define M(a) ((a) + 1)\n");

    for same in [
        "#define M(a) ((a)+1)\n",
        "#define M(a) ( ( a ) + 1 )\n",
        "#define M(a)   ((a)  + \\\n    1)\n",
        "#define M(a) ((a) /* one */ + \\\r\n 1) // one\n",
        "#define M(a) ((a) /* one */ + 1)\n",
        "#define M(a) ((a) + \\\n /* one */ 1)\n",
    ] {
        assert_eq!(macro_hash(same), one, "{same:?}");
    }
    for other in [
        "#define M(a) ((a) + 2)\n",
        "#define M(a) ((a) ++ 1)\n",
        "#define M(a) ((a) /* one */ - 7)\n",
    ] {
        assert_ne!(macro_hash(other), one, "{other:?}");
    }
    // A punctuator of two bytes is one token, two of one byte are two.
    for (one, two) in [
        ("((a) && (b))", "((a) & &(b))"),
        ("a->b", "a- >b"),
        ("a##b", "a# #b"),
        ("a++b", "a+ +b"),
    ] {
        let define = |body: &str| macro_hash(&format!("#define M(a, b) {body}\n"));
        assert_ne!(define(one), define(two), "{one:?}");
    }
}

#[test]
fn a_line_continuation_inside_a_token_joins_it_and_between_tokens_is_blank() {
    let f = |text: &str| hash_in(FileKind::C, text, "f");
    let m = |text: &str| hash_in(FileKind::C, text, "M");

    assert_ne!(m("#define M a\\\nb\n"), m("#define M a b\n"));
    assert_eq!(m("#define M a\\\nb\n"), m("#define M ab\n"));
    assert_ne!(
        f("int f(void) { return a\\\nb; }\n"),
        f("int f(void) { return a b; }\n")
    );
    for line_break in ["\n", "\r\n"] {
        for continued in [" + \\", " +\\"] {
            assert_eq!(
                f("int f(void) { return 1 + 2; }\n"),
                f(&format!(
                    "int f(void) {{ return 1{continued}{line_break}2; }}\n"
                ))
            );
        }
    }
}

#[test]
fn a_tree_defined_attribute_macro_is_blanked_before_the_parse() {
    let text = "class API Book { int pages; };\nNS_BEGIN\nint f();\nNS_END\n";
    let header = crate::code::lexed(
        b"#define API __attribute__((visibility(\"default\")))\n#define NS_BEGIN namespace shelf {\n#define NS_END }\n",
    );
    let read = |tree: &BlankMacros| {
        let index = index_in("shelf.cc", FileKind::Cpp, text.as_bytes(), tree).unwrap();
        let scopes = Scopes(&index.scopes);
        index
            .definitions
            .iter()
            .map(|d| path(scopes, d.scope))
            .collect::<Vec<_>>()
    };

    assert_eq!(
        read(&BlankMacros::of([("api.h", &header.into_defines())])),
        ["Book", "Book::pages", "f"]
    );
    assert!(!read(&BlankMacros::default()).contains(&"Book".to_owned()));
}

#[test]
fn a_blanked_static_macro_makes_what_it_marks_private_as_static_does() {
    let text = "#define local static\n#define SI static inline\n#define API\n\
                local int f(void) { return 0; }\nSI int g(void) { return 1; }\n\
                local API char buffer[4];\nint h(void);\nint (*local_fn)(void);\n\
                struct tm local_tm;\nlocal struct s { int a; } v;\n";
    let visibility: Vec<String> = statements_of(FileKind::C, text)
        .into_iter()
        .map(|(statement, _)| statement)
        .filter(|statement| !statement.contains(" Macro "))
        .collect();

    assert_eq!(
        visibility,
        [
            "f Function Code Private",
            "g Function Code Private",
            "buffer Variable Code Private",
            "h Function Code Public",
            "local_fn Variable Code Public",
            "local_tm Variable Code Public",
            "s Struct Code Private",
            "v Variable Code Private",
            "s::a Field Code Public",
        ]
    );
}

#[test]
fn a_spans_lines_are_counted_in_the_bytes_as_written() {
    let text = "#define SWAP(a, b) do { /* swap the\n two values,\n three lines */ \\\n  t = a; } while (0)\nint after(void) { return 0; }\n";
    let index = crate::code::testing::index(FileKind::C, text.as_bytes()).unwrap();
    let scopes = Scopes(&index.scopes);
    let lines: Vec<(String, usize, usize)> = index
        .definitions
        .iter()
        .map(|d| (path(scopes, d.scope), d.span.first_line, d.span.last_line))
        .collect();

    assert_eq!(
        lines,
        [("SWAP".to_owned(), 1, 4), ("after".to_owned(), 5, 5)]
    );
}
