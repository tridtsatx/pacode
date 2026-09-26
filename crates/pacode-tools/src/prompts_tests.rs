//! Convention and budget checks for the tool descriptions in `prompts/`.
//!
//! Two things are easy to break silently when a tool is added or its prose is
//! edited, and both cost the model real tokens on every step:
//! * a tool whose description never moved to `prompts/<name>.md` (it drifts back
//!   into a string literal, or is missing entirely);
//! * descriptions that grow — every word here is paid on every model step, for
//!   every session, forever.

use pacode_types::WebConfig;

use crate::builtin::builtin_tools;

/// Ceiling on the summed `estimate_tokens` of the whole built-in tool set.
///
/// This is a **regression fuse, not a budget**. The point is not to hit 12k; it
/// is that a new tool, a longer description or a fatter schema has to fail the
/// build on purpose rather than quietly tax every request. The real per-session
/// figure is this sum minus the tools a subset registry leaves out.
const TOKEN_FUSE: u32 = 12_000;

/// Definitions repeat, and the schema is serialised twice more on the way out
/// (`anthropic`/`openai_compat` re-serialise the value, `devin` renders it to a
/// string), so the estimate is a lower bound on what the prompt really costs.
#[test]
fn builtin_tool_definitions_stay_under_the_token_fuse() {
    let registry = builtin_tools(&WebConfig::default());
    let definitions = registry.definitions();
    assert!(
        definitions.len() >= 20,
        "the built-in set shrank to {} tools; is the registry still complete?",
        definitions.len()
    );

    let total: u32 = definitions.iter().map(|d| d.estimate_tokens()).sum();
    let per_tool: Vec<(&str, u32)> = definitions
        .iter()
        .map(|d| (d.name.as_str(), d.estimate_tokens()))
        .collect();
    assert!(
        total < TOKEN_FUSE,
        "built-in tool definitions now estimate {total} tokens, over the {TOKEN_FUSE} fuse: \
         {per_tool:?}"
    );
}

/// Every built-in description lives in `prompts/<name>.md` and reaches the model
/// from there. The second half of the check is the load-bearing one: a file that
/// exists but is not what the tool returns is exactly the drift the split was
/// supposed to end, so the two are compared byte for byte.
#[test]
fn every_builtin_tool_serves_its_description_from_a_file() {
    let registry = builtin_tools(&WebConfig::default());
    for name in registry.names() {
        let path = format!("{}/{name}.md", prompts_dir());
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("{name}: no prompts/{name}.md ({err})"));
        let trimmed = text.trim();
        assert!(
            !trimmed.is_empty(),
            "{name}: prompts/{name}.md is empty; a tool with no description is one \
             the model cannot choose on purpose"
        );
        assert!(
            trimmed == text.trim_end_matches('\n'),
            "{name}: prompts/{name}.md must hold only the description, with no trailing whitespace"
        );
        assert!(
            !trimmed.starts_with('#'),
            "{name}: prompts/{name}.md starts with a heading; the file is the description text, \
             not markdown to be wrapped in anything"
        );

        let tool = registry
            .get(name)
            .unwrap_or_else(|| panic!("{name} is in the registry but not retrievable from it"));
        assert_eq!(
            tool.description(),
            trimmed,
            "{name}: Tool::description does not match prompts/{name}.md — either the file is \
             stale or the tool hardcodes its prose again"
        );
    }
}

/// The crate's `src/prompts/` directory. A tool whose name is not a file name
/// (MCP `<server>__<tool>`, plugin tools) is out of scope: those descriptions
/// come from the server, not from us.
fn prompts_dir() -> String {
    format!("{}/src/prompts", env!("CARGO_MANIFEST_DIR"))
}
