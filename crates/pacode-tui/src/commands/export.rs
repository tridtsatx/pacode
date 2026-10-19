//! `/export [path]`: the loaded transcript as markdown.
//!
//! No path → `~/pacode-<session>.md`. `~` expands to `$HOME`; a relative path
//! is resolved against the session's cwd, not the client process's.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use pacode_types::state::ToastLevel;
use pacode_types::{AgentMessageDirection, AgentReply, TranscriptKind};

use crate::keys::Action;
use crate::state::AppState;
use crate::state::transcript::{BackgroundKind, BackgroundOutcome, Cell, CellKind};

#[cfg(test)]
#[path = "export_tests.rs"]
mod export_tests;

pub(super) fn run(state: &mut AppState, arg: &str) -> Vec<Action> {
    let session = state
        .meta
        .as_ref()
        .map(|m| m.id.to_string())
        .unwrap_or_else(|| "current".to_string());
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let path = resolve_path(arg, &session, &state.cwd(), home.as_deref());
    let md = markdown(state.transcript.cells.iter());
    let items = state
        .transcript
        .cells
        .iter()
        .filter(|c| !matches!(c.kind, CellKind::Gap))
        .count();
    let path_display = path.display();
    match std::fs::write(&path, md) {
        Ok(()) if state.transcript.has_more_history => state.push_notice_with_level(
            ToastLevel::Success,
            format!(
                "Exported {items} loaded items to {path_display}; older history was not \
                 loaded (scroll up to load it, then export again)"
            ),
        ),
        Ok(()) => state.push_notice_with_level(
            ToastLevel::Success,
            format!("Exported {items} items to {path_display}"),
        ),
        Err(e) => state.push_notice_with_level(
            ToastLevel::Error,
            format!("Export to {path_display} failed: {e}"),
        ),
    }
    vec![]
}

/// Where `/export <arg>` writes. `home` is `None` when `$HOME` is unset; the
/// session cwd stands in for it then.
fn resolve_path(arg: &str, session: &str, cwd: &Path, home: Option<&Path>) -> PathBuf {
    let home = home.unwrap_or(cwd);
    if arg.is_empty() || arg == "~" {
        return home.join(format!("pacode-{session}.md"));
    }
    if let Some(rest) = arg.strip_prefix("~/") {
        return home.join(rest);
    }
    cwd.join(arg)
}

fn markdown<'a>(cells: impl Iterator<Item = &'a Cell>) -> String {
    let mut md = String::new();
    for cell in cells {
        // `write!` into a `String` cannot fail.
        let _ = match &cell.kind {
            CellKind::Gap => Ok(()),
            CellKind::BackgroundResult(result) => {
                let kind = match result.kind {
                    BackgroundKind::Task => "Task",
                    BackgroundKind::Agent => "Agent",
                };
                let outcome = match result.outcome {
                    BackgroundOutcome::Completed => "completed",
                    BackgroundOutcome::Failed => "failed",
                    BackgroundOutcome::Killed => "killed",
                };
                writeln!(md, "*{kind} {outcome}: {}*\n", result.label)
            }
            CellKind::Item(kind) => item(&mut md, kind),
        };
    }
    md
}

fn item(md: &mut String, kind: &TranscriptKind) -> std::fmt::Result {
    match kind {
        TranscriptKind::User { text } => write!(md, "### User\n\n{text}\n\n"),
        TranscriptKind::Images { images } => write!(md, "*{} image(s)*\n\n", images.len()),
        TranscriptKind::Assistant { text, .. } => write!(md, "### Assistant\n\n{text}\n\n"),
        TranscriptKind::Reasoning { text, .. } => {
            md.push_str("### Reasoning\n\n");
            for line in text.lines() {
                writeln!(md, "> {line}")?;
            }
            md.push('\n');
            Ok(())
        }
        TranscriptKind::ToolCall {
            name,
            title,
            preview,
            ..
        } => {
            write!(md, "*Tool: {name} - {title}*\n\n")?;
            fenced(md, preview)
        }
        TranscriptKind::Notice { level, text } => {
            let level = match level {
                ToastLevel::Info => "Info",
                ToastLevel::Success => "Success",
                ToastLevel::Warn => "Warning",
                ToastLevel::Error => "Error",
            };
            write!(md, "*{level}: {text}*\n\n")
        }
        TranscriptKind::Permission(req) => write!(md, "*Permission: {}*\n\n", req.title),
        TranscriptKind::Question { question, answer } => {
            write!(md, "### Question\n\n{}\n\n", question.question)?;
            match answer {
                Some(answer) => write!(md, "**Answer:** {}\n\n", answer.render(question)),
                None => Ok(()),
            }
        }
        TranscriptKind::BashCommand {
            command,
            output,
            exit_code,
            truncated,
        } => {
            let code = exit_code
                .map(|c| format!(" (exit {c})"))
                .unwrap_or_default();
            write!(md, "### Bash: {command}{code}\n\n")?;
            fenced(md, output)?;
            if *truncated {
                md.push_str("*(output truncated)*\n\n");
            }
            Ok(())
        }
        TranscriptKind::AgentMessage {
            subagent_name,
            direction,
            text,
            reply,
            ..
        } => {
            match direction {
                AgentMessageDirection::ToSubagent => {
                    write!(md, "*To {subagent_name}:*\n\n{text}\n\n")?
                }
                AgentMessageDirection::Question => {
                    write!(md, "*{subagent_name} asks:*\n\n{text}\n\n")?
                }
            }
            match reply {
                Some(AgentReply::Answered { text }) => write!(md, "**Answer:** {text}\n\n"),
                Some(AgentReply::Unanswered) => md.write_str("*No answer.*\n\n"),
                Some(AgentReply::Failed) => {
                    md.write_str("*No answer: the orchestrator's turn failed.*\n\n")
                }
                Some(AgentReply::Cancelled) => md.write_str("*Cancelled.*\n\n"),
                None => Ok(()),
            }
        }
    }
}

fn fenced(md: &mut String, body: &str) -> std::fmt::Result {
    if body.is_empty() {
        return Ok(());
    }
    write!(md, "```\n{}\n```\n\n", body.trim_end_matches('\n'))
}
