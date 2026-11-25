use super::*;

/// The blank set of a tree whose files are `files`.
fn blank_set(files: &[&str]) -> BlankMacros {
    BlankMacros::of(files.iter().map(|text| defines(text.as_bytes())))
}

/// The names `set` blanks: written alone, then called.
fn names(set: &BlankMacros) -> (Vec<&str>, Vec<&str>) {
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
    // `CONSTEXPR` is also `1`, so neither it nor `EXPORT`, which names it,
    // is blanked.
    let set = blank_set(&[header, other]);

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

    assert_eq!(
        names(&blank_set(&[opening, closing_alone])).0,
        ["BEGIN", "C_BEGIN", "END", "LOOP_END"]
    );
    assert!(blank_set(&[closing_alone]).names.is_empty());
}

#[test]
fn a_cycle_or_a_name_no_file_defines_blanks_nothing() {
    let set = blank_set(&["#define A B\n#define B A\n#define C D_OUTSIDE\n#define E A\n"]);

    assert!(set.names.is_empty() && set.calls.is_empty());
}

#[test]
fn a_body_is_read_up_to_its_bound_of_tokens() {
    let body = |tokens: usize| format!("#define AT {}\n", "inline ".repeat(tokens));

    assert_eq!(names(&blank_set(&[&body(MAX_MACRO_BODY_TOKENS)])).0, ["AT"]);
    assert!(
        blank_set(&[&body(MAX_MACRO_BODY_TOKENS + 1)])
            .names
            .is_empty()
    );
}

#[test]
fn a_tree_defined_attribute_is_blanked_where_code_writes_it_alone() {
    let set = blank_set(&[
        "#define API __attribute__((used))\n#define PRINTF(f, a) __attribute__((format(printf, f, a)))\n",
    ]);
    let text = "#ifdef API\n#define OTHER API\n#endif\n/* API */ API int f(const char *s, ...) PRINTF(1, 2);\nconst char *t = \"API\", *r = R\"x(API)x\";\nint PRINTF;\n";
    let blanked = blank(text, &set);

    assert_eq!(blanked.len(), text.len());
    assert_eq!(
        blanked,
        "#ifdef API\n#define OTHER API\n#endif\n/* API */     int f(const char *s, ...)             ;\nconst char *t = \"API\", *r = R\"x(API)x\";\nint PRINTF;\n"
    );
    assert!(matches!(blank("int f();\n", &set), Cow::Borrowed(_)));
}

#[test]
fn a_call_is_blanked_within_its_bound_of_argument_bytes() {
    let set = blank_set(&["#define ATTR(x) __attribute__((x))\n"]);
    let call = |bytes: usize| format!("int f() ATTR({});\n", "a".repeat(bytes - 2));

    let within = call(MAX_BLANK_ARGUMENT_BYTES);
    assert!(!blank(&within, &set).contains("ATTR"));
    let past = call(MAX_BLANK_ARGUMENT_BYTES + 1);
    assert_eq!(blank(&past, &set), past);
}

#[test]
fn the_comments_of_a_continued_define_are_blanked_and_no_others() {
    let text = "#define SWAP(a, b) do { /* swap */ \\\n  t = a; \\\n } while (0) // last\n#define ONE 1 /* one */\n/* kept */\n";

    assert_eq!(
        blank(text, &BlankMacros::default()),
        "#define SWAP(a, b) do {            \\\n  t = a; \\\n } while (0) // last\n#define ONE 1 /* one */\n/* kept */\n"
    );
}

#[test]
fn hostile_defines_are_read_in_linear_work() {
    let count = 100_000;
    let chain: String = (0..count)
        .map(|n| format!("#define M{n} M{}\n", n + 1))
        .chain([format!("#define M{count}\n")])
        .collect();
    let cycle: String = (0..count)
        .map(|n| format!("#define C{n} C{}\n", (n + 1) % count))
        .collect();
    let long_body = format!("#define LONG {}\n", "inline ".repeat(1 << 17));
    let calls = format!(
        "#define ATTR(x) __attribute__((x))\n{}",
        "ATTR(".repeat(1 << 18)
    );

    assert_eq!(blank_set(&[&chain]).names.len(), count + 1);
    assert!(blank_set(&[&cycle]).names.is_empty());
    assert!(blank_set(&[&long_body]).names.is_empty());
    let set = blank_set(&[&calls]);
    assert_eq!(blank(&calls, &set).len(), calls.len());
}

#[test]
fn a_set_is_known_by_its_names_and_their_forms() {
    let alone = blank_set(&["#define A\n"]);
    let called = blank_set(&["#define A(x) __attribute__((x))\n"]);

    assert_ne!(alone.identity(), called.identity());
    assert_eq!(
        alone.identity(),
        blank_set(&["#define A\n#define A\n"]).identity()
    );
    assert_ne!(alone.identity(), BlankMacros::of([]).identity());
}
