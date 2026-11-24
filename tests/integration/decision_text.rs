//! A decision's choice and reason are held to the rules of `store::text`
//! wherever they come from: a hand edit that breaks one still loads, and
//! `check` reports it as an error naming the file, the field and the
//! fault.

use crate::harness::Project;

const REASON: &str = "An hour bounds how long a stolen token is useful";

/// Text one field must refuse, and the class of fault it carries.
struct Case {
    class: &'static str,
    field: &'static str,
    text: String,
}

fn case(class: &'static str, field: &'static str, text: impl Into<String>) -> Case {
    Case {
        class,
        field,
        text: text.into(),
    }
}

fn reason_with(class: &'static str, inserted: &str) -> Case {
    case(class, "reason", format!("{REASON}{inserted} for the rest"))
}

fn cases() -> Vec<Case> {
    vec![
        case("blank choice", "choice", "   "),
        case("multi-line choice", "choice", "Sign tokens\nwith Ed25519"),
        case("tab in a choice", "choice", "Sign\ttokens"),
        case("choice over 200 bytes", "choice", "c".repeat(201)),
        case("argument tag in a choice", "choice", "Sign tokens</choice>"),
        case("reason under 10 bytes", "reason", "  too short  "),
        reason_with("C0 control", "\u{1B}[31m"),
        reason_with("carriage return", "\r\n"),
        reason_with("DEL", "\u{7F}"),
        reason_with("C1 control", "\u{85}"),
        reason_with("bidi override", "\u{202E}"),
        reason_with("bidi isolate", "\u{2066}"),
        reason_with("zero-width space", "\u{200B}"),
        reason_with("byte order mark", "\u{FEFF}"),
        reason_with("line separator", "\u{2028}"),
        reason_with("paragraph separator", "\u{2029}"),
        reason_with("closing parameter tag", "</parameter>"),
        reason_with("opening parameter tag", "<parameter name=\"tags\">"),
        reason_with("invoke tag", "<invoke name=\"get_context\">"),
        reason_with("function_calls tag", "</function_calls>"),
        reason_with("argument tag in a reason", "</alternatives>"),
    ]
}

/// A decision file holding `choice` and `reason`, each escaped as TOML.
fn decision_file(choice: &str, reason: &str) -> String {
    let mut table = toml::Table::new();
    table.insert("component".into(), "project".into());
    table.insert("choice".into(), choice.into());
    table.insert("reason".into(), reason.into());
    table.insert("status".into(), "accepted".into());
    table.insert("created".into(), "2025-06-01T10:00:00Z".into());
    toml::to_string(&table).unwrap()
}

#[test]
fn check_reports_each_class_naming_the_file_the_field_and_the_fault() {
    let project = Project::init();
    project.write("decisions/fine.toml", &decision_file("Sign tokens", REASON));
    project.run_ok(&["check"]);

    for case in cases() {
        let (choice, reason) = match case.field {
            "choice" => (case.text.as_str(), REASON),
            _ => ("Expire sessions after an hour", case.text.as_str()),
        };
        project.write("decisions/edited.toml", &decision_file(choice, reason));

        let stderr = project.run_refused(&["check"]);

        let expected = format!("  error: decisions/edited.toml: `{}` ", case.field);
        let errors: Vec<&str> = stderr
            .lines()
            .filter(|line| line.starts_with("  error"))
            .collect();
        assert_eq!(errors.len(), 1, "{}: {stderr}", case.class);
        assert!(errors[0].starts_with(&expected), "{}: {stderr}", case.class);
    }
}
