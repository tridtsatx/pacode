//! Effort levels a model catalog enumerates, and the shapes real endpoints
//! spell them in.
//!
//! Most `/models` payloads say nothing about reasoning effort. That is not the
//! same as "every level works", and it is not the same as "the catalog knows
//! nothing" either — it is a third case, the common one, and the answer for it
//! is `None`: the picker keeps the whole ladder selectable rather than greying
//! out a level the API would have accepted. These parsers answer only when a
//! catalog *names* the levels it accepts:
//!
//! - `supported_reasoning_levels` — the ChatGPT backend catalog
//!   (`chatgpt.com/backend-api/codex/models`) and the GitHub Copilot `/models`
//!   catalog, as `[{"reasoning_effort": "high"}, …]`.
//! - `supported_reasoning_efforts` — the same catalogs in the flat spelling,
//!   `["none", "low", "xhigh"]`, and gateways that return it directly.
//! - `reasoning_options` — opencode's catalog, whose `type: "effort"` entry
//!   carries the same list.
//!
//! Values with no [`Effort`] counterpart (`none`, `minimal`, vendor sentinels
//! like `swarm`) are dropped, not guessed at: a level this client cannot send
//! is not a level to offer as selectable, and mapping it onto the nearest rung
//! we do have would claim a support the catalog never stated.

use pacode_types::Effort;
use serde_json::Value;

/// Keys a catalog may enumerate its reasoning levels under, in the order they
/// are tried: the object spelling first, the flat one second.
const LEVEL_KEYS: [&str; 2] = ["supported_reasoning_levels", "supported_reasoning_efforts"];

/// The levels one catalog entry enumerates, in [`Effort::ALL`] order.
///
/// `None` when the entry enumerates nothing, or names only values this client
/// cannot send. Both are "unknown" to the picker, and unknown has to stay
/// unknown: an invented "unsupported" list would grey out a level that works.
pub fn efforts(entry: &Value) -> Option<Vec<Effort>> {
    let mut found: Vec<Effort> = Vec::new();
    for value in levels(entry) {
        if let Some(effort) = level_name(value).and_then(Effort::parse)
            && !found.contains(&effort)
        {
            found.push(effort);
        }
    }
    if found.is_empty() {
        return None;
    }
    // Catalog order is the vendor's, and two catalogs spelling the same set in
    // different orders would render the picker's detail line differently.
    found.sort();
    Some(found)
}

/// Every value the entry enumerates, whichever key and whichever element shape
/// its catalog used: `[{"reasoning_effort": "high"}, …]` and `["high", …]` are
/// the same statement about the same model.
fn levels(entry: &Value) -> impl Iterator<Item = &Value> {
    LEVEL_KEYS
        .iter()
        .find_map(|key| entry.get(*key))
        .and_then(Value::as_array)
        .or_else(|| option_values(entry))
        .into_iter()
        .flatten()
}

/// opencode's spelling: a `reasoning_options` entry tagged `type: "effort"`
/// whose `values` array lists the level names.
fn option_values(entry: &Value) -> Option<&Vec<Value>> {
    entry
        .get("reasoning_options")?
        .as_array()?
        .iter()
        .find(|option| option.get("type").and_then(Value::as_str) == Some("effort"))?
        .get("values")?
        .as_array()
}

/// The level name of one enumerated element: a bare string, or an object naming
/// it under any of the keys the catalogs use.
fn level_name(value: &Value) -> Option<&str> {
    value.as_str().or_else(|| {
        value
            .get("reasoning_effort")
            .or_else(|| value.get("effort"))
            .or_else(|| value.get("value"))
            .and_then(Value::as_str)
    })
}

#[cfg(test)]
#[path = "catalog_efforts_tests.rs"]
mod catalog_efforts_tests;
