//! `trurlic map --json`: one level of the code tree, over a project built
//! for it whose every value below was derived by hand from its files and
//! decisions. A level and its refusals are JSON on stdout; the command
//! prints no other form.

use serde_json::Value;

use crate::golden::assert_golden;
use crate::harness::{Project, anchored, trurlic};

/// The largest source file the index reads, past which a file's bytes are
/// left unread.
const MAX_SOURCE_BYTES: usize = 4 * 1024 * 1024;

/// A shared graph over a workspace of three crates: `app` at the root, a
/// `core` crate, and one whose manifest does not parse. `app` holds a
/// directory module, one-file modules, an inline module with a file below
/// it, a test-only module, a module whose file is not UTF-8, an import
/// that resolves to nothing, and a Python directory with a file past the
/// size the index reads. Seven decisions are anchored on them, one
/// outside the walk.
fn project() -> Project {
    let project = Project::shared();
    for (path, text) in [
        (
            "Cargo.toml",
            "[workspace]\n[package]\nname = \"app\"\n[dependencies]\n\
             core = { path = \"crates/core\" }\n[dev-dependencies]\nfar = { path = \"../far\" }\n",
        ),
        (
            "src/lib.rs",
            "//! The app.\npub mod store;\nmod util;\nmod broken;\nmod inline {\n    mod deep;\n}\n\
             #[cfg(test)]\nmod tests;\n",
        ),
        (
            "src/store/mod.rs",
            "//! Where things are kept.\npub mod disk;\npub fn keep() {}\n",
        ),
        (
            "src/store/disk.rs",
            "use crate::util::helper;\npub fn write() {\n    helper();\n}\nfn lost() {\n    \
             crate::missing::thing();\n}\n",
        ),
        ("src/util.rs", "//! Small helpers.\npub fn helper() {}\n"),
        (
            "src/inline/deep.rs",
            "pub fn deep() {\n    crate::store::keep();\n}\n",
        ),
        (
            "src/tests.rs",
            "use crate::store::keep;\n#[test]\nfn kept() {\n    keep();\n}\n",
        ),
        ("tools/check.py", "def main():\n    pass\n"),
        ("crates/core/Cargo.toml", "[package]\nname = \"core\"\n"),
        (
            "crates/core/src/lib.rs",
            "//! The core.\npub fn core() {}\n",
        ),
        ("crates/old/Cargo.toml", "[package\n"),
    ] {
        project.source(path, text);
    }
    let broken = project.path().join("src/broken.rs");
    std::fs::write(broken, b"pub fn x() {}\n\xff\n").unwrap();
    let huge = project.path().join("tools/huge.py");
    std::fs::write(huge, vec![b'#'; MAX_SOURCE_BYTES + 1]).unwrap();
    for (name, anchors, status) in [
        (
            "keep-store",
            &["src/store/mod.rs::keep", "src/store/disk.rs"][..],
            "accepted",
        ),
        ("help-util", &["src/util.rs::helper"], "proposed"),
        ("check-tools", &["tools"], "proposed"),
        (
            "share-core",
            &["crates/core/src/lib.rs::core", "src/util.rs"],
            "accepted",
        ),
        ("write-disk", &["src/store/disk.rs::nothing"], "accepted"),
        ("outside", &["README.md"], "accepted"),
        ("all-of-src", &["src"], "proposed"),
    ] {
        anchored(&project, name, anchors, status);
    }
    project
}

/// `trurlic map --json` with `args` after it, its JSON.
fn map(project: &Project, args: &[&str]) -> Value {
    let output = trurlic(project.path())
        .args(["map", "--json"])
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

/// The decisions of `level`'s holder, proposed and accepted, and those in
/// no child; then each child, one a line: its address, kind and name, its
/// lines and unread files, its definitions, non-test ones and unindexed
/// files, its decisions, its unresolved imports, then what its kind has.
fn spelled(level: &Value) -> Vec<String> {
    let counts = |counts: &Value| format!("{}/{}", counts["proposed"], counts["accepted"]);
    let holder = format!(
        "holder {} in no child {}",
        counts(&level["decisions"]),
        counts(&level["decisions_in_no_child"])
    );
    let children = level["children"].as_array().unwrap().iter().map(|child| {
        let (lines, definitions) = (&child["lines"], &child["definitions"]);
        let kind = child["kind"].as_str().unwrap();
        format!(
            "{} {kind} {}: lines {}+{} definitions {}/{}+{} decisions {} unresolved {} {}",
            child["address"].as_str().unwrap(),
            child["name"].as_str().unwrap(),
            lines["counted"],
            lines["unread_files"],
            definitions["all"],
            definitions["non_test"],
            definitions["unindexed_files"],
            counts(&child["decisions"]),
            child["unresolved_imports"],
            child.get(kind).map_or_else(String::new, Value::to_string),
        )
    });
    std::iter::once(holder).chain(children).collect()
}

/// The level of `app`'s crate, which the golden does not hold.
#[test]
fn a_level_holds_the_hand_checked_sums_decisions_and_what_each_kind_has() {
    let project = project();

    let app = map(&project, &["crate:."]);

    assert_eq!(
        spelled(&app),
        [
            "holder 3/3 in no child 0/0",
            "module:@src module src: lines 31+0 definitions 13/11+1 decisions 2/3 unresolved 1 \
             {\"doc\":{\"cut\":false,\"text\":\"The app.\"},\
             \"imports\":{\"outside_level\":0,\"siblings\":[]}}",
            "directory:tools directory tools: lines 2+1 definitions 1/1+1 decisions 1/0 \
             unresolved 0 ",
            "file:Cargo.toml file Cargo.toml: lines 7+0 definitions 9/9+0 decisions 0/0 \
             unresolved 0 {\"imports\":{\"outside_level\":0,\"siblings\":[]},\
             \"place\":\"no_module\"}",
        ]
    );
}

#[test]
fn a_page_starts_at_the_child_from_names() {
    let project = project();

    let tools = map(
        &project,
        &["directory:tools", "--from", "file:tools/huge.py"],
    );

    assert_eq!(tools["total"], 2);
    let children = tools["children"].as_array().unwrap();
    let listed: Vec<&str> = children
        .iter()
        .map(|child| child["address"].as_str().unwrap())
        .collect();
    assert_eq!(listed, ["file:tools/huge.py"]);
    assert_eq!(tools.get("next"), None);
}

#[test]
fn two_levels_match_golden() {
    let project = project();
    let levels = [map(&project, &[]), map(&project, &["module:@src"])];

    let rendered = serde_json::to_string_pretty(&levels).unwrap() + "\n";

    assert_golden("map_levels.json", &rendered);
}

#[test]
fn an_address_of_no_node_or_no_child_is_refused_as_json_naming_it() {
    let project = project();
    for (args, field, address) in [
        (&["module:gone@src"][..], "node", "module:gone@src"),
        (
            &["crate:.", "--from", "file:src/lib.rs"],
            "from",
            "file:src/lib.rs",
        ),
    ] {
        let output = trurlic(project.path())
            .args(["map", "--json"])
            .args(args)
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(1), "{args:?}");
        let refusal: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(refusal["error"]["kind"], "invalid_argument");
        assert_eq!(refusal["error"]["field"], field);
        let message = refusal["error"]["message"].as_str().unwrap();
        assert!(message.contains(&format!("`{address}`")), "{message}");
    }
}

#[test]
fn map_without_json_is_a_usage_error_naming_json() {
    let project = project();

    let output = trurlic(project.path()).arg("map").output().unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("--json"), "{stderr}");
}

/// A shared graph over one crate whose `src/shapes.rs` holds a struct with
/// a field, inherent impls of it and of a second struct, cfg twins and a
/// test, and whose `src/other.rs` holds a trait impl and an inherent impl
/// of the first struct and a second struct of the second's name. Ten
/// decisions are anchored on the first file: on the file, on its
/// directory, on the struct, on its field, two on one method, on the
/// twins, on a symbol it does not define and on one it defines at two
/// paths, and one on both the file and a method; another on the second
/// file.
fn shapes() -> Project {
    let project = Project::shared();
    for (path, text) in [
        ("Cargo.toml", "[package]\nname = \"shapes\"\n"),
        ("src/lib.rs", "mod shapes;\nmod other;\n"),
        (
            "src/shapes.rs",
            "//! Shapes.\npub struct Square {\n    pub side: u32,\n}\n\nimpl Square {\n    \
             pub fn area(&self) -> u32 {\n        self.side * self.side\n    }\n}\n\n\
             pub struct Circle;\n\nimpl Circle {\n    pub fn area(&self) -> u32 {\n        3\n    \
             }\n}\n\n#[cfg(unix)]\npub fn twin() {}\n#[cfg(not(unix))]\npub fn twin() {}\n\n\
             #[test]\nfn checks() {}\n",
        ),
        (
            "src/other.rs",
            "use crate::shapes::Square;\n\nimpl std::fmt::Display for Square {\n    fn fmt(&self, \
             f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {\n        write!(f, \
             \"square\")\n    }\n}\n\nimpl Square {\n    pub fn more(&self) {}\n\n    pub fn \
             most(&self) {}\n}\n\npub struct Circle;\n\nimpl Circle {\n    pub fn square(&self) \
             {}\n}\n\npub fn squared() {}\n\npub fn square_root_of_a_number() {}\n",
        ),
    ] {
        project.source(path, text);
    }
    for (name, anchors, status) in [
        ("a-on-file", &["src/shapes.rs"][..], "accepted"),
        ("b-square", &["src/shapes.rs::Square"], "accepted"),
        ("c-side", &["src/shapes.rs::Square::side"], "proposed"),
        ("d-area-one", &["src/shapes.rs::Square::area"], "accepted"),
        ("e-area-two", &["src/shapes.rs::Square::area"], "proposed"),
        ("f-twin", &["src/shapes.rs::twin"], "accepted"),
        ("g-missing", &["src/shapes.rs::missing"], "proposed"),
        ("h-ambiguous", &["src/shapes.rs::area"], "accepted"),
        ("i-src", &["src"], "proposed"),
        ("j-other", &["src/other.rs::squared"], "accepted"),
        (
            "k-both",
            &["src/shapes.rs", "src/shapes.rs::Square::area"],
            "accepted",
        ),
    ] {
        anchored(&project, name, anchors, status);
    }
    project
}

/// Each item of `items`, a list of decisions, as `name status`.
fn decided(items: &Value) -> Vec<String> {
    let items = items.as_array().unwrap().iter();
    let spelled = items.map(|item| {
        let (name, status) = (&item["decision"], &item["status"]);
        format!("{} {}", name.as_str().unwrap(), status.as_str().unwrap())
    });
    spelled.collect()
}

/// The ranges of `src/shapes.rs`, derived by hand: the struct covers lines
/// 2 to 4 and its field line 3, so the field's line splits the struct's
/// range; the two decisions on `Square::area` cover its lines 7 to 9
/// together; the twins' spans, 20 to 21 and 22 to 23, are one set's, so
/// they are one range. Lines no span covers are ranges of no decision. The
/// file and directory anchors are on the file, apart, a decision with one
/// of them in no range whatever its symbol covers. The missing symbol of a
/// decision never accepted is planned, and the ambiguous one covers
/// nothing.
#[test]
fn a_frame_cuts_its_lines_where_the_spans_of_its_anchors_start_and_end() {
    let project = shapes();

    let frame = map(&project, &["--frame", "file:src/shapes.rs"]);

    let ranges = frame["ranges"].as_array().unwrap().iter().map(|range| {
        let lines = format!("{}-{}", range["first_line"], range["last_line"]);
        format!("{lines} {:?}", decided(&range["decisions"]))
    });
    assert_eq!(
        ranges.collect::<Vec<String>>(),
        [
            "1-1 []",
            "2-2 [\"b-square accepted\"]",
            "3-3 [\"b-square accepted\", \"c-side proposed\"]",
            "4-4 [\"b-square accepted\"]",
            "5-6 []",
            "7-9 [\"d-area-one accepted\", \"e-area-two proposed\"]",
            "10-19 []",
            "20-23 [\"f-twin accepted\"]",
            "24-26 []",
        ]
    );
    assert_eq!(frame["line_count"], 26);
    assert_eq!(
        decided(&frame["on_file"]),
        ["a-on-file accepted", "i-src proposed", "k-both accepted"]
    );
    let unspanned = frame["unspanned_anchors"].as_array().unwrap().iter();
    let unspanned = unspanned.map(|anchor| {
        let decision = anchor["decision"].as_str().unwrap();
        let kind = anchor["kind"].as_str().unwrap();
        format!("{decision} {} {kind}", anchor["anchor"].as_str().unwrap())
    });
    assert_eq!(
        unspanned.collect::<Vec<String>>(),
        [
            "g-missing src/shapes.rs::missing planned",
            "h-ambiguous src/shapes.rs::area ambiguous"
        ]
    );
}

/// The impls of `Square` that `src/other.rs` holds: its `Display` impl,
/// lines 3 to 7, and an inherent impl whose members run from line 10 to
/// line 12; `Circle` is a struct of both files, so its impls are no one's.
#[test]
fn a_type_lists_the_impls_of_its_name_in_other_files_unless_another_type_has_it() {
    let project = shapes();

    let frame = map(&project, &["--frame", "file:src/shapes.rs"]);

    let definitions = frame["definitions"].as_array().unwrap().iter();
    let types = definitions
        .filter(|definition| definition["kind"] == "struct")
        .map(|definition| format!("{} {}", definition["path"], definition["impls"]));
    assert_eq!(
        types.collect::<Vec<String>>(),
        [
            "[\"Square\"] {\"elsewhere\":[{\"file\":\"file:src/other.rs\",\"first_line\":3,\
             \"last_line\":7,\"path\":[\"Square\",\"<Display>\"]},{\"file\":\
             \"file:src/other.rs\",\"first_line\":10,\"last_line\":12,\"path\":[\"Square\"]}],\
             \"types_named\":1}",
            "[\"Circle\"] {\"types_named\":2}",
        ]
    );
}

/// The hits of `square`, ranked by hand: the two names that are the query
/// in any case, `Square` before `Circle::square` by its shorter path; the
/// two names it starts, the longer `square_root_of_a_number` still before
/// every path it is a substring of; those, the shorter first, the four of
/// 12 bytes by their bytes.
#[test]
fn a_search_ranks_an_exact_name_then_a_prefix_then_a_substring_the_shorter_path_first() {
    let project = shapes();

    let found = map(&project, &["--search", "square"]);

    let hits = found["hits"].as_array().unwrap().iter().map(|hit| {
        let definition = &hit["definition"];
        let (matched, file) = (hit["match"].as_str().unwrap(), &definition["file"]);
        let lines = &definition["first_line"];
        format!(
            "{matched} {} {} {lines}",
            file.as_str().unwrap(),
            definition["path"]
        )
    });
    assert_eq!(
        hits.collect::<Vec<String>>(),
        [
            "exact_name file:src/shapes.rs [\"Square\"] 2",
            "exact_name file:src/other.rs [\"Circle\",\"square\"] 18",
            "name_prefix file:src/other.rs [\"squared\"] 21",
            "name_prefix file:src/other.rs [\"square_root_of_a_number\"] 23",
            "path_substring file:src/shapes.rs [\"Square\",\"area\"] 7",
            "path_substring file:src/other.rs [\"Square\",\"more\"] 10",
            "path_substring file:src/other.rs [\"Square\",\"most\"] 12",
            "path_substring file:src/shapes.rs [\"Square\",\"side\"] 3",
            "path_substring file:src/other.rs [\"Square\",\"<Display>\"] 3",
            "path_substring file:src/other.rs [\"Square\",\"<Display>\",\"fmt\"] 4",
        ]
    );
    assert_eq!(found["total"], 10);
}

#[test]
fn a_frame_and_a_search_match_golden() {
    let project = shapes();
    let units = [
        map(&project, &["--frame", "file:src/other.rs"]),
        map(&project, &["--search", "CIRCLE"]),
    ];

    let rendered = serde_json::to_string_pretty(&units).unwrap() + "\n";

    assert_golden("map_frame_and_search.json", &rendered);
}

/// The fixture's file that is not UTF-8 was read, so its line count is
/// known and one range spans its lines; it lists no definition.
#[test]
fn a_frame_of_a_file_the_index_refuses_says_why_and_lists_no_definition() {
    let project = project();

    let frame = map(&project, &["--frame", "file:src/broken.rs"]);

    let spelled = format!(
        "{} {} lines {} ranges {} definitions {}",
        frame["address"].as_str().unwrap(),
        frame["index_state"].as_str().unwrap(),
        frame["line_count"],
        frame["ranges"],
        frame["definitions"],
    );
    assert_eq!(
        spelled,
        "file:src/broken.rs not_utf8 lines 2 ranges [{\"decisions\":[],\"first_line\":1,\
         \"last_line\":2}] definitions []"
    );
    assert!(frame["cause"].is_string());
}

#[test]
fn a_frame_of_no_file_a_position_past_the_end_and_a_query_out_of_bounds_are_refused_as_json() {
    let project = shapes();
    let long = "a".repeat(257);
    for (args, field, named) in [
        (&["--frame", "crate:."][..], "file", "`crate:.`"),
        (
            &["--frame", "src/shapes.rs"],
            "file",
            "`file:src/shapes.rs`",
        ),
        (
            &["--frame", "file:src/shapes.rs", "--from-position", "999"],
            "from_position",
            "999",
        ),
        (&["--search", ""], "query", "is empty"),
        (&["--search", long.as_str()], "query", "257 bytes"),
    ] {
        let output = trurlic(project.path())
            .args(["map", "--json"])
            .args(args)
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(1), "{args:?}");
        let refusal: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(refusal["error"]["kind"], "invalid_argument", "{args:?}");
        assert_eq!(refusal["error"]["field"], field, "{args:?}");
        let message = refusal["error"]["message"].as_str().unwrap();
        assert!(message.contains(named), "{message}");
    }
}

#[test]
fn two_units_or_a_start_of_another_unit_s_kind_is_a_usage_error() {
    let project = shapes();
    for (args, flag) in [
        (&["crate:.", "--frame", "file:src/shapes.rs"][..], "--frame"),
        (
            &["--frame", "file:src/shapes.rs", "--search", "x"],
            "--search",
        ),
        (
            &["--frame", "file:src/shapes.rs", "--from", "file:src/lib.rs"],
            "--from <ADDRESS>",
        ),
        (&["crate:.", "--from-position", "1"], "--frame"),
        (
            &["--search", "x", "--from-position", "next"],
            "--from-position",
        ),
    ] {
        let output = trurlic(project.path())
            .args(["map", "--json"])
            .args(args)
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains(flag), "{stderr}");
    }
}
