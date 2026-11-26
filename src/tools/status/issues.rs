//! The `issues` section of `status`: the graph's validation issues, sorted.

use crate::store::{Issue, IssueKind};
use crate::tools::spec::output::{Str, Tag};
use crate::tools::spec::{Output, Shown};

pub(crate) struct IssueItem {
    kind: IssueKind,
    pub(crate) message: String,
}

pub(super) fn issue_item(issue: Issue) -> IssueItem {
    IssueItem {
        kind: issue.kind,
        message: issue.message,
    }
}

impl Output for IssueItem {
    fn declare(fields: &mut Shown<'_, Self>) {
        let kind = Tag {
            all: &IssueKind::ALL,
            name: IssueKind::as_str,
        };
        fields.required("kind", "The issue's kind.", &kind, |item| &item.kind);
        fields.required(
            "message",
            "The file at fault, then the fault.",
            &Str,
            |item| &item.message,
        );
    }
}
