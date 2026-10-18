//! Text shown to the model: the `<style-guard>` block appended to tool results,
//! the message for an aborted stream, and the one-line pinned reminder.

use crate::rule::Severity;
use crate::scan::Finding;

pub const OPEN_TAG: &str = "<style-guard>";
pub const CLOSE_TAG: &str = "</style-guard>";
/// Prefix of the tool-error text used when a streaming call was cut short.
pub const STREAM_STOP_PREFIX: &str = "style-guard stopped this call: ";
const MAX_MESSAGE_CHARS: usize = 600;
const PIN_CHARS: usize = 300;

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    format!("{cut}…")
}

fn sev(s: Severity) -> &'static str {
    match s {
        Severity::Warn => "warn",
        Severity::Block => "block",
    }
}

fn hits_and_rules(findings: &[Finding]) -> String {
    let mut out = String::new();
    for f in findings {
        out.push_str(&format!(
            "- line {} [{}] {}: `{}`\n",
            f.line,
            sev(f.severity),
            f.rule,
            f.snippet
        ));
    }
    let mut seen: Vec<&str> = Vec::new();
    out.push_str("Rules:\n");
    for f in findings {
        if !seen.contains(&f.rule.as_str()) {
            seen.push(&f.rule);
            out.push_str(&format!(
                "- {}: {}\n",
                f.rule,
                clip(&f.message, MAX_MESSAGE_CHARS)
            ));
        }
    }
    out
}

/// Block appended to an edit/write tool result after the write succeeded.
pub fn render_after_write(path: &str, findings: &[Finding], suppressed: usize) -> String {
    let blocking = findings.iter().any(|f| f.severity == Severity::Block);
    let mut out = format!(
        "\n\n{OPEN_TAG}\n{} style issue(s) in {path} (lines you just added):\n",
        findings.len()
    );
    out.push_str(&hits_and_rules(findings));
    if suppressed > 0 {
        out.push_str(&format!("(+{suppressed} more hits not shown)\n"));
    }
    out.push_str(if blocking {
        "The file was written, but this is an ERROR: fix the lines above now, before doing anything else.\n"
    } else {
        "Fix these unless there is a strong reason not to.\n"
    });
    out.push_str(CLOSE_TAG);
    out
}

/// Tool-error text for a call that was cut short while streaming (nothing was written).
pub fn render_stream_stop(tool: &str, path: &str, findings: &[Finding]) -> String {
    let mut out = format!(
        "{STREAM_STOP_PREFIX}{tool} {path} was NOT executed because the code you were writing violates a style rule.\n{OPEN_TAG}\n"
    );
    out.push_str(&hits_and_rules(findings));
    out.push_str("Re-issue the call with the offending code rewritten.\n");
    out.push_str(CLOSE_TAG);
    out
}

/// Short single-line reminder kept across compaction (see `pinned_reminders`).
pub fn pinned_line(f: &Finding) -> String {
    let one_line = f.message.split_whitespace().collect::<Vec<_>>().join(" ");
    format!("[style-guard] {}: {}", f.rule, clip(&one_line, PIN_CHARS))
}
