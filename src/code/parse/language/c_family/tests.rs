use std::borrow::Cow;

use super::*;

/// The sets of a tree whose headers are `headers`, each its path and its
/// text.
fn tree_at(headers: &[(&str, &str)]) -> BlankMacros {
    let defines: Vec<(&str, Defines)> = headers
        .iter()
        .map(|(path, text)| (*path, lexed(text.as_bytes()).into_defines()))
        .collect();
    BlankMacros::of(defines.iter().map(|(path, defines)| (*path, defines)))
}

/// The sets of a tree whose headers are `headers`, none in a test place.
fn tree(headers: &[&str]) -> BlankMacros {
    let named: Vec<String> = (0..headers.len())
        .map(|n| format!("include/{n}.h"))
        .collect();
    let paths: Vec<(&str, &str)> = named
        .iter()
        .map(String::as_str)
        .zip(headers.iter().copied())
        .collect();
    tree_at(&paths)
}

/// What `text`, a source file outside test places, blanks in a tree whose
/// sets are `tree`.
fn file(tree: &BlankMacros, text: &str) -> FileMacros {
    tree.of_file("src/f.cc", &lexed(text.as_bytes()))
}

/// `text` blanked in a tree whose headers are `headers`.
fn blanked(headers: &[&str], text: &str) -> String {
    blank(text, &file(&tree(headers), text)).text.into_owned()
}

/// The names `set` blanks: written alone, then called.
fn names(set: &FileMacros) -> (Vec<&str>, Vec<&str>) {
    (
        set.names.iter().map(String::as_str).collect(),
        set.calls.iter().map(String::as_str).collect(),
    )
}

#[test]
fn a_macro_is_blanked_when_every_definition_in_the_tree_is_an_attribute() {
    let header = "#if defined(_WIN32)\n#define API __declspec(dllexport)\n#else\n#define API __attribute__((visibility(\"default\")))\n#endif\n\
                  #define CONSTEXPR constexpr\n#define EMPTY\n#define NODISCARD [[nodiscard]]\n\
                  #define PRINTF(f, a) __attribute__((format(printf, f, a)))\n\
                  #define GUARDED(x) ANNOTATE(guarded_by(x))\n#define ANNOTATE(x) __attribute__((x))\n#define ANNOTATE(x)\n\
                  #define EXPORT API CONSTEXPR\n";
    let other = "#define CAPACITY 40\n#define HALF(n) ((n) / 2)\n#define SAME(x) x\n#define STUB(x)\n\
                 #define CALLED_ALONE ANNOTATE\n#define OUTSIDE SYSTEM_API\n#define CONSTEXPR 1\n";
    let all = "API CONSTEXPR EMPTY NODISCARD PRINTF GUARDED ANNOTATE EXPORT CAPACITY HALF SAME STUB CALLED_ALONE OUTSIDE";
    // `CONSTEXPR` is also `1`, so neither it nor `EXPORT`, which names it,
    // is blanked.
    let set = file(&tree(&[header, other]), all);

    assert_eq!(
        names(&set),
        (
            vec!["API", "EMPTY", "NODISCARD"],
            vec!["ANNOTATE", "GUARDED", "PRINTF"]
        )
    );
}

#[test]
fn a_namespace_macro_is_blanked_when_the_tree_defines_an_opening() {
    let opening = "#define BEGIN namespace shelf { inline namespace v1 {\n#define END } }\n#define C_BEGIN extern \"C\" {\n";
    let closing_alone = "#define LOOP_END }\n";
    let all = "BEGIN END C_BEGIN LOOP_END";

    assert_eq!(
        names(&file(&tree(&[opening, closing_alone]), all)).0,
        ["BEGIN", "C_BEGIN", "END", "LOOP_END"]
    );
    assert!(file(&tree(&[closing_alone]), all).names.is_empty());
}

#[test]
fn a_cycle_or_a_name_no_file_defines_blanks_nothing() {
    let set = file(
        &tree(&["#define A B\n#define B A\n#define C D_OUTSIDE\n#define E A\n"]),
        "A B C E",
    );

    assert!(set.names.is_empty() && set.calls.is_empty());
}

#[test]
fn a_body_is_read_up_to_its_bound_of_tokens() {
    let body = |tokens: usize| format!("#define AT {}\n", "inline ".repeat(tokens));

    assert_eq!(
        names(&file(&tree(&[&body(MAX_MACRO_BODY_TOKENS)]), "AT")).0,
        ["AT"]
    );
    assert!(
        file(&tree(&[&body(MAX_MACRO_BODY_TOKENS + 1)]), "AT")
            .names
            .is_empty()
    );
}

#[test]
fn a_directive_is_a_define_only_spelled_so() {
    let set = file(
        &BlankMacros::default(),
        "#defineA\n# define C\n#define D(x)\nA C D(1)",
    );

    assert_eq!(names(&set), (vec!["C"], vec![]));
}

#[test]
fn an_attribute_list_ends_at_its_double_bracket_and_a_parameter_is_no_name() {
    let header =
        "#define DEPRECATED [[deprecated(a[1])]] static\n#define x\n#define WRAP(x) static x\n";

    assert_eq!(
        names(&file(&tree(&[header]), "DEPRECATED WRAP")),
        (vec!["DEPRECATED"], vec![])
    );
}

#[test]
fn a_source_files_define_blanks_in_that_file_alone() {
    let local = "#define DONE\nDONE int f(void);\n";
    let other = "enum { PORT, DONE };\n";
    let headers = tree(&[]);

    assert_eq!(names(&file(&headers, local)).0, ["DONE"]);
    assert!(file(&headers, other).names.is_empty());
}

#[test]
fn a_files_own_define_of_a_header_name_is_judged_with_the_headers() {
    let headers = tree(&["#define API\n#define EXPORT API\n"]);
    let redefined = "#undef API\n#define API 1\nAPI EXPORT int f(void);\n";

    assert_eq!(names(&file(&headers, redefined)).0, ["EXPORT"]);
    assert_eq!(names(&file(&headers, "API EXPORT")).0, ["API", "EXPORT"]);
}

#[test]
fn a_files_own_define_naming_a_header_name_it_redefines_is_judged_by_its_own() {
    let headers = tree(&["#define API\n"]);
    let text = "#undef API\n#define API 1\n#define LOCAL API\nLOCAL int f(void);\n";

    assert!(file(&headers, text).names.is_empty());
}

#[test]
fn a_files_own_closing_brace_is_blanked_when_a_header_opens_a_namespace() {
    let headers = tree(&["#define BEGIN namespace shelf {\n"]);

    assert_eq!(names(&file(&headers, "#define END }\nEND")).0, ["END"]);
    assert!(file(&tree(&[]), "#define END }\nEND").names.is_empty());
}

#[test]
fn a_file_blanks_only_the_names_it_writes() {
    let text = "API int f(void);\n";
    let one = tree(&["#define API\n"]);
    let two = tree(&["#define API\n#define UNUSED\n"]);

    assert_eq!(file(&one, text), file(&two, text));
    assert_ne!(
        file(&one, text).identity(),
        file(&tree(&[]), text).identity()
    );
}

#[test]
fn a_set_is_known_by_its_names_and_their_forms() {
    let alone = file(&tree(&["#define A\n"]), "A");
    let called = file(&tree(&["#define A(x) __attribute__((x))\n"]), "A");
    let static_one = file(&tree(&["#define ALWAYS_STATIC static\n"]), "ALWAYS_STATIC");
    let plain = file(&tree(&["#define ALWAYS_STATIC inline\n"]), "ALWAYS_STATIC");

    assert_ne!(alone.identity(), called.identity());
    assert_eq!(
        alone.identity(),
        file(&tree(&["#define A\n#define A\n"]), "A").identity()
    );
    assert_ne!(alone.identity(), FileMacros::default().identity());
    assert_ne!(static_one.identity(), plain.identity());
}

#[test]
fn a_tree_defined_attribute_is_blanked_where_code_writes_it_alone() {
    let header = "#define API __attribute__((used))\n#define PRINTF(f, a) __attribute__((format(printf, f, a)))\n";
    let text = "#ifdef API\n#define OTHER API\n#endif\n/* API */ API int f(const char *s, ...) PRINTF(1, 2);\nconst char *t = \"API\", *r = R\"x(API)x\";\nint PRINTF;\n";
    let blanked = blanked(&[header], text);

    assert_eq!(
        blanked,
        "#ifdef API\n#define OTHER API\n#endif\n/* API */     int f(const char *s, ...)             ;\nconst char *t = \"API\", *r = R\"x(API)x\";\nint PRINTF;\n"
    );
    assert!(matches!(
        blank("int f();\n", &FileMacros::default()).text,
        Cow::Borrowed(_)
    ));
}

#[test]
fn a_static_macro_of_any_length_marks_the_code_after_it_private() {
    let header = "#ifdef TESTING\n#define STATIC\n#define SI\n#else\n#define STATIC static\n#define SI static inline\n#endif\n#define local static\n#define API\n";
    let text = "STATIC int a;\nSI /* c */ int b(void);\nlocal API int c;\nint d; local\n#if X\n#endif\nint e;\n";
    let blanked = blank(text, &file(&tree(&[header]), text));
    let after = |code: &str| text.find(code).unwrap();

    assert_eq!(
        blanked.text,
        "       int a;\n   /* c */ int b(void);\n          int c;\nint d;      \n#if X\n#endif\nint e;\n"
    );
    assert_eq!(
        blanked.private,
        [
            after("int a"),
            after("int b"),
            after("int c"),
            after("int e")
        ]
    );
}

#[test]
fn a_call_is_blanked_within_its_bound_of_argument_bytes() {
    let header = "#define ATTR(x) __attribute__((x))\n";
    let call = |bytes: usize| format!("int f() ATTR({});\n", "a".repeat(bytes - 2));

    let within = call(MAX_BLANK_ARGUMENT_BYTES);
    assert!(!blanked(&[header], &within).contains("ATTR"));
    let past = call(MAX_BLANK_ARGUMENT_BYTES + 1);
    assert_eq!(blanked(&[header], &past), past);
}

#[test]
fn a_calls_arguments_end_at_their_parenthesis_past_literals_comments_and_lines() {
    let header = "#define ATTR(x) __attribute__((x))\n";
    let text = "int f() ATTR(\")\" /* ) */ ')',\n  (x));\n";

    assert_eq!(
        blanked(&[header], text),
        format!("int f() {}\n{};\n", " ".repeat(21), " ".repeat(6))
    );
}

#[test]
fn a_call_is_left_as_written_when_no_parenthesis_follows_its_name() {
    let header = "#define ATTR(x) __attribute__((x))\n";
    for text in [
        "int ATTR;\n",
        "int f() ATTR\n(x);\n",
        "int f() ATTR /* c */ (x);\n",
    ] {
        assert_eq!(blanked(&[header], text), text);
    }
}

#[test]
fn a_call_a_statement_ends_before_its_parenthesis_blanks_only_the_names_in_it() {
    let header = "#define ATTR(x) __attribute__((x))\n#define API\n";
    for stop in [";", "{", "}", "\n#if X\n"] {
        let text = format!("int f() ATTR(API x{stop} int g(void));\n");
        let blanked = blanked(&[header], &text);

        assert!(
            blanked.contains("ATTR(") && blanked.contains("g(void)"),
            "{text}"
        );
        assert!(!blanked.contains("API"), "{text}");
    }
}

#[test]
fn the_comments_inside_a_define_are_blanked_line_breaks_and_all() {
    let text = "#define SWAP(a, b) do { /* swap the\n two */ \\\n  t = a; \\\n } while (0) // last\n#define ONE 1 /* one */\n/* kept */\n";
    let inside = "#define M(a) ((a) /* c */ + 1)\n#define LIMIT(a) ((a) + \\\n    /* slack */ 1)\n";

    assert_eq!(
        blank(text, &FileMacros::default()).text,
        format!(
            "#define SWAP(a, b) do {{ {} \\\n  t = a; \\\n }} while (0) // last\n#define ONE 1 /* one */\n/* kept */\n",
            " ".repeat(19)
        )
    );
    assert_eq!(
        blank(inside, &FileMacros::default()).text,
        "#define M(a) ((a)         + 1)\n#define LIMIT(a) ((a) + \\\n                1)\n"
    );
}

#[test]
fn a_header_is_named_by_its_extension() {
    assert!(["a.h", "b/c.hh", "d.hpp"].into_iter().all(is_header));
    assert!(
        !["a.c", "b.cc", "c.cpp", "d.cxx", "e.hpp.c"]
            .into_iter()
            .any(is_header)
    );
}

#[test]
fn hostile_defines_are_read_in_linear_work() {
    let count = 100_000;
    let chain: String = (0..count)
        .map(|n| format!("#define M{n} M{}\n", n + 1))
        .chain([format!("#define M{count}\n")])
        .collect();
    let star: String = (0..count)
        .map(|n| format!("#define S{n} HUB\n"))
        .chain(["#define HUB\n".to_owned()])
        .collect();
    let cycle: String = (0..count)
        .map(|n| format!("#define C{n} C{}\n", (n + 1) % count))
        .collect();
    let long_body = format!("#define LONG {}\n", "inline ".repeat(1 << 17));
    let calls = format!(
        "#define ATTR(x) __attribute__((x))\n{}",
        "ATTR(".repeat(1 << 18)
    );

    for set in [&chain, &star, &cycle] {
        assert!(tree(&[set]).steps() <= 5 * (count + 1), "{}", &set[..20]);
    }
    assert!(tree(&[&long_body]).steps() == 0);
    assert_eq!(blanked(&[&calls], &calls).len(), calls.len());
}

#[test]
fn a_files_own_defines_are_judged_in_work_its_own_bound() {
    let wide: String = (0..MAX_MACRO_BODY_TOKENS)
        .map(|n| format!("a{n} "))
        .collect();
    let headers: Vec<String> = (0..1000)
        .flat_map(|n| {
            [
                format!("#define DEBUG {wide}\n"),
                format!("#define WIDE w{n}\n"),
            ]
        })
        .collect();
    let headers: Vec<&str> = headers.iter().map(String::as_str).collect();
    let set = tree(&headers);
    let own = lexed(b"#define DEBUG\n#define WIDE\nDEBUG WIDE int f(void);\n");

    let steps: usize = (0..1000).map(|_| set.file_steps("src/f.c", &own)).sum();

    // Each file's two names, each needing at most the bound's names; the
    // thousand headers of each name cost nothing per file.
    assert!(steps <= 1000 * 4 * (2 + MAX_MACRO_BODY_TOKENS), "{steps}");
}

#[test]
fn a_define_of_a_word_the_grammars_read_is_no_macro() {
    let header = "#ifndef STDC\n#define const\n#define inline\n#endif\n#define __attribute__(x)\n#define restrict\n#define _Atomic\n#define API\n";
    let set = file(
        &tree(&[header]),
        "const inline restrict _Atomic API __attribute__((x)) int f;",
    );

    assert_eq!(names(&set), (vec!["API"], vec![]));
}

#[test]
fn a_header_in_a_test_place_reaches_test_code_alone() {
    let set = tree_at(&[
        ("tests/fake.h", "#define DONE\n#define FAKE_API\n"),
        ("include/api.h", "#define API\n"),
    ]);
    let text = lexed(b"API FAKE_API DONE");
    let all = vec!["API", "DONE", "FAKE_API"];

    assert_eq!(names(&set.of_file("src/state.c", &text)).0, ["API"]);
    assert_eq!(names(&set.of_file("tests/state_check.c", &text)).0, all);
    assert_eq!(names(&set.of_file("src/state_test.cc", &text)).0, all);
}

#[test]
fn a_lexical_pass_reads_back_from_its_bytes_within_its_room() {
    let text = "#define API __attribute__((used))\n#define BEGIN namespace a {\n#define END }\n#define ST static\n#define N 1\nAPI int f(void); a b c\n";
    let lexed = lexed(text.as_bytes());
    let bytes = encode_lexed(&lexed);

    assert_eq!(decode_lexed(&bytes, text.len()), Some(lexed));
    assert!(bytes.len() <= max_lexed_bytes(text.len()));
    assert_eq!(decode_lexed(&bytes[..bytes.len() - 1], text.len()), None);
    assert_eq!(decode_lexed(&bytes, 0), None);
}

#[test]
fn a_dense_file_fits_its_lexical_room() {
    let names: String = (0..20_000).map(|n| format!("a{n} ")).collect();
    let defines = "#define A\n".repeat(10_000);
    let bodies = format!("#define B {}\n", "a ".repeat(MAX_MACRO_BODY_TOKENS));
    for text in [names, defines, bodies.repeat(1000)] {
        let lexed = lexed(text.as_bytes());

        assert_eq!(decode_lexed(&encode_lexed(&lexed), text.len()), Some(lexed));
    }
}

#[test]
fn a_lexical_pass_holds_each_name_written_once_in_order() {
    let lexed = lexed(b"b a b /* c */ \"d\" a\n#define E F\n");

    assert_eq!(lexed.spelled().collect::<Vec<_>>(), ["a", "b"]);
}
