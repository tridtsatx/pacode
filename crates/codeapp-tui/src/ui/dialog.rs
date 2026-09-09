//! The transcript (main dialog and the agent panel share this renderer).
//!
//! Cell rendering (mockup): user message = text with a cyan left bar (`▎`/`|`) and a
//! blank line after; assistant = markdown via `codeapp_render::render_markdown`;
//! reasoning = collapsed to one faint line `thinking…` while streaming / `thought for
//! Ns` when complete (expandable later); tool call = `▣ Name` cyan + title dim +
//! status: running (`…` accent), ok (`+64 −12` green/red for diffs, `ok 3.2s` green),
//! error red preview line, backgrounded `→ background` violet, denied red; notices
//! faint. Lines come from the cache (`(cell.id, width, cell.version)`); the live cell
//! uses `split_stable_tail` so only its tail re-renders. Scrolling by lines; when
//! `scroll_from_bottom == 0` the view follows the tail. When scrolled up, a faint
//! `↓ N new lines` marker appears at the bottom. Reaching the top with
//! `has_more_history` triggers `Action::LoadHistory`.
