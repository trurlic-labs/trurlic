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
