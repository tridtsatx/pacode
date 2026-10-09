//! B19: where `/export` writes, and what it writes.

use std::path::Path;

use pacode_types::state::ToastLevel;
use pacode_types::{AgentId, TranscriptItem, TranscriptKind};

use super::{markdown, resolve_path};
use crate::state::transcript::Transcript;

#[test]
fn default_and_tilde_paths_land_in_home_and_relative_ones_in_the_session_cwd() {
    let home = Path::new("/home/u");
    let cwd = Path::new("/work/proj");
    assert_eq!(
        resolve_path("", "ses_1", cwd, Some(home)),
        home.join("pacode-ses_1.md")
    );
    assert_eq!(
        resolve_path("~/notes/x.md", "ses_1", cwd, Some(home)),
        home.join("notes/x.md")
    );
    assert_eq!(
        resolve_path("out.md", "ses_1", cwd, Some(home)),
        cwd.join("out.md")
    );
    assert_eq!(
        resolve_path("/tmp/abs.md", "ses_1", cwd, Some(home)),
        Path::new("/tmp/abs.md")
    );
    // No $HOME: the session cwd stands in.
    assert_eq!(
        resolve_path("", "ses_1", cwd, None),
        cwd.join("pacode-ses_1.md")
    );
}

#[test]
fn reasoning_notices_and_tool_output_are_exported() {
    let mut t = Transcript::new(64);
    let items = [
        TranscriptKind::Reasoning {
            text: "think".into(),
            complete: true,
        },
        TranscriptKind::Notice {
            level: ToastLevel::Warn,
            text: "careful".into(),
        },
        TranscriptKind::ToolCall {
            call_id: pacode_types::CallId::new("call_1"),
            name: "bash".into(),
            title: "bash ls".into(),
            intent: None,
            status: pacode_types::ToolStatus::Ok,
            preview: "file.rs".into(),
            diff: None,
            duration_ms: None,
            task: None,
        },
    ];
    t.reset(
        items
            .into_iter()
            .enumerate()
            .map(|(i, kind)| TranscriptItem {
                seq: i as u64 + 1,
                agent: AgentId::main(),
                ts_ms: 1,
                kind,
            })
            .collect(),
        false,
    );
    let md = markdown(t.cells.iter());
    assert!(md.contains("> think"), "{md}");
    assert!(md.contains("*Warning: careful*"), "{md}");
    assert!(md.contains("```\nfile.rs\n```"), "{md}");
}
