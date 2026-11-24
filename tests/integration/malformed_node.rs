//! A file of the graph that cannot be read as its type: every surface that
//! loads the graph refuses it with an error that starts with the file's
//! path, then says what is wrong. One test per shape a hand edit can break.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::harness::{McpClient, Project, decision};

/// A project with the one file `file` under `.trurlic/` written as `text`,
/// and that file's path as an error names it.
fn project_with(file: &str, text: &[u8]) -> (Project, PathBuf) {
    let project = Project::init();
    project.write("components/auth.toml", "description = \"Auth\"\n");
    fs::write(project.node(file), text).unwrap();
    let path = fs::canonicalize(project.node(file)).unwrap();
    (project, path)
}

/// `check` and `status` fail naming `path` first, then `detail`.
fn assert_refused(project: &Project, path: &Path, detail: &str) {
    for command in ["check", "status"] {
        let stderr = project.run_refused(&[command]);
        let named = format!("error: {}: ", path.display());
        assert!(
            stderr.starts_with(&named),
            "{command}: {named:?} not first in {stderr:?}"
        );
        assert!(
            stderr.contains(detail),
            "{command}: {detail:?} not in {stderr:?}"
        );
    }
}

/// A test that `file` holding `text` is refused with `detail`.
macro_rules! refused {
    ($test:ident, $file:expr, $text:expr, $detail:expr) => {
        #[test]
        fn $test() {
            let text: String = $text;
            let (project, path) = project_with($file, text.as_bytes());
            assert_refused(&project, &path, $detail);
        }
    };
}

fn sign(extra: &str) -> String {
    decision("auth", "Sign tokens", extra)
}

refused!(
    not_toml,
    "decisions/sign.toml",
    sign("unclosed = [\n"),
    "invalid TOML"
);
refused!(
    an_unknown_key,
    "decisions/sign.toml",
    sign("weight = 3\n"),
    "unknown field `weight`"
);
refused!(
    a_name_key,
    "decisions/sign.toml",
    sign("name = \"sign\"\n"),
    "unknown field `name`"
);
refused!(
    a_missing_choice,
    "decisions/sign.toml",
    sign("").replace("choice = \"Sign tokens\"\n", ""),
    "missing field `choice`"
);
refused!(
    tags_that_are_no_list,
    "decisions/sign.toml",
    sign("tags = \"security\"\n"),
    "invalid type"
);
refused!(
    an_unknown_status,
    "decisions/sign.toml",
    sign("").replace("\"accepted\"", "\"approved\""),
    "unknown variant `approved`"
);
refused!(
    accepted_via_on_a_proposed_decision,
    "decisions/sign.toml",
    sign("accepted_via = \"cli\"\n").replace("\"accepted\"", "\"proposed\""),
    "`accepted_via` is set, but `status` is \"proposed\""
);
refused!(
    an_unknown_accepted_via,
    "decisions/sign.toml",
    sign("accepted_via = \"email\"\n"),
    "unknown variant `email`"
);
refused!(
    accepted_text_on_an_accepted_decision,
    "decisions/sign.toml",
    sign("\n[accepted_text]\nchoice = \"Old\"\nreason = \"The text the owner accepted\"\n"),
    "`accepted_text` is set, but `status` is \"accepted\""
);
refused!(
    a_created_that_is_no_timestamp,
    "decisions/sign.toml",
    sign("").replace("2025-06-01T10:00:00Z", "yesterday"),
    "RFC 3339"
);
refused!(
    an_absolute_anchor,
    "decisions/sign.toml",
    sign("anchors = [{ file = \"/etc/passwd\" }]\n"),
    "must be relative to the code root"
);
refused!(
    an_anchor_that_climbs_out,
    "decisions/sign.toml",
    sign("anchors = [{ file = \"src/../../x.rs\" }]\n"),
    "`..` segment"
);
refused!(
    an_anchor_with_a_backslash,
    "decisions/sign.toml",
    sign("anchors = [{ file = 'src\\auth.rs' }]\n"),
    "separates segments with `/`"
);
refused!(
    an_anchor_with_an_unknown_key,
    "decisions/sign.toml",
    sign("anchors = [{ file = \"src/auth.rs\", line = 3 }]\n"),
    "unknown field `line`"
);
refused!(
    a_component_with_a_name_key,
    "components/auth.toml",
    "name = \"auth\"\ndescription = \"Auth\"\n".to_owned(),
    "unknown field `name`"
);
refused!(
    connects_to_that_is_no_list,
    "components/auth.toml",
    "description = \"Auth\"\nconnects_to = \"store\"\n".to_owned(),
    "invalid type"
);
refused!(
    a_project_without_placement,
    "project.toml",
    format!(
        "format_version = \"{}\"\nname = \"x\"\ndescription = \"\"\n",
        trurlic::store::FORMAT_VERSION
    ),
    "missing field `placement`"
);
refused!(
    an_unknown_placement,
    "project.toml",
    format!(
        "format_version = \"{}\"\nname = \"x\"\ndescription = \"\"\nplacement = \"cloud\"\n",
        trurlic::store::FORMAT_VERSION
    ),
    "unknown variant `cloud`"
);

/// One byte over the cap, and no TOML at all: only the size can refuse it.
#[test]
fn a_node_file_over_one_mib() {
    let mut text = sign("").into_bytes();
    text.resize(1024 * 1024, b'\n');
    text.push(b'[');
    let (project, path) = project_with("decisions/sign.toml", &text);
    assert_refused(
        &project,
        &path,
        "larger than the 1048576 bytes a node file may hold",
    );
}

/// The file breaks while the server runs: the next call loads it and
/// names it.
#[test]
fn a_tool_call_names_the_malformed_file() {
    let (project, path) = project_with("decisions/sign.toml", sign("").as_bytes());
    let (mut client, _) = McpClient::connect(&project);
    client.call_tool("get_context", json!({ "component": "auth" }));

    project.write("decisions/sign.toml", &sign("unclosed = [\n"));

    let message = client.call_tool_error("get_context", json!({ "component": "auth" }));
    assert!(
        message.starts_with(&format!("{}: invalid TOML", path.display())),
        "{message}"
    );
}
