//! Prompt text for the LLM summary stage of compaction. Kept in its own file so the
//! mandatory section list is reviewed in one place.

/// System prompt for the summarisation call.
pub const SUMMARY_SYSTEM: &str = "You are a precise summarizer of a coding-agent session. \
You never answer or continue the conversation you are given; you only produce the \
summary document described in the final instruction. Preserve identifiers, file paths, \
command lines and error strings exactly as written, never paraphrase them.";

/// Mandatory section layout, shared by the initial and the update prompt.
const SECTIONS: &str = "\
Produce a markdown document with EXACTLY these sections, in this order. Write `(none)` \
under a section that has nothing to report; never omit a section.

## Goal & user constraints
The user's goal, then every instruction, constraint, preference or prohibition the user \
gave, as VERBATIM quotes (one bullet per quote). Do not reword user instructions.

## Decisions made & why
Each decision taken (design, library, approach rejected) with the reason it was taken.

## Files touched
One bullet per file: `path` - what changed (created / edited / deleted / only read, and \
the substance of the change).

## Commands run & outcomes
Notable commands (builds, tests, searches) with their exact command line and outcome \
(pass / fail, and the exact key error string when it failed).

## Open problems / next steps
Unfinished work, known failures, blockers, and the concrete next step to take.

## Facts learned about the codebase
Durable facts discovered (module layout, conventions, APIs, gotchas) that the agent \
would otherwise have to rediscover.

Rules: keep identifiers, paths and error strings byte-for-byte exact. Be dense, no \
filler, no preamble, no closing remarks. Output only the document.";

/// Instruction appended when there is no previous summary.
pub const SUMMARY_INITIAL: &str = "Summarize the conversation inside <conversation> so the \
work can continue without it.\n\n";

/// Instruction appended when a previous summary exists: update, do not re-summarise.
pub const SUMMARY_UPDATE: &str = "A summary of the earlier part of the session is in \
<previous-summary>; the newer part of the session is in <conversation>. UPDATE the previous \
summary with the new span instead of summarising the summary: keep every still-valid \
detail (especially verbatim user quotes, paths and error strings), add new information, \
move finished items out of `Open problems / next steps`, and correct anything the new span \
contradicts. The output replaces the previous summary entirely.\n\n";

/// Full instruction text for the final user message.
pub fn instructions(has_previous: bool) -> String {
    let lead = if has_previous {
        SUMMARY_UPDATE
    } else {
        SUMMARY_INITIAL
    };
    format!("{lead}{SECTIONS}")
}
