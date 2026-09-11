//! Folding the Devin catalog's per-effort model ids into families.
//!
//! The server publishes one model id per reasoning effort — `swe-2-medium`,
//! `swe-2-high`, `swe-2-max` — which turns the model picker into a list of the
//! same handful of models repeated at every effort. The picker instead offers one
//! entry per family (`swe-2`) and [`resolve`] turns the family plus the session's
//! effort back into the concrete id the request must carry.

use pacode_types::Effort;

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod catalog_tests;

/// Effort suffixes a model id may end with, weakest first. `none` and `minimal`
/// have no counterpart in [`Effort`]; they take part in nearest-match so that a
/// family offering only those is still reachable.
const EFFORT_TIERS: &[(&str, u8)] = &[
    ("none", 0),
    ("minimal", 1),
    ("low", 2),
    ("medium", 3),
    ("high", 4),
    ("xhigh", 5),
    ("max", 6),
];

/// Suffixes that sit after the effort and name a serving variant rather than an
/// effort. They stay part of the family id, so `claude-opus-5-high-fast` and
/// `claude-opus-5-high` end up in different families.
const VARIANT_SUFFIXES: &[&str] = &["fast", "priority", "1m"];

/// Rank of an [`Effort`] on the same scale as [`EFFORT_TIERS`].
fn effort_rank(effort: Effort) -> u8 {
    match effort {
        Effort::Low => 2,
        Effort::Medium => 3,
        Effort::High => 4,
        Effort::XHigh => 5,
        Effort::Max => 6,
    }
}

fn tier_rank(name: &str) -> Option<u8> {
    EFFORT_TIERS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, r)| *r)
}

/// A catalog id split into the family it belongs to and the effort it encodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedModelId {
    /// Family id shown in the picker, e.g. `swe-2` or `claude-opus-5-fast`.
    pub family: String,
    /// Rank of the effort suffix, `None` when the id carries no effort.
    pub effort_rank: Option<u8>,
}

/// Split a catalog id into its family and the effort it encodes. An id with no
/// effort suffix (`swe-1-6`, `MODEL_PRIVATE_2`) is a family of its own.
pub fn parse_model_id(id: &str) -> ParsedModelId {
    let mut parts: Vec<&str> = id.split('-').collect();

    let mut variants: Vec<&str> = Vec::new();
    while let Some(last) = parts.last()
        && VARIANT_SUFFIXES.contains(last)
        && parts.len() > 1
    {
        variants.push(parts.pop().unwrap_or_default());
    }
    variants.reverse();

    let effort_rank = match parts.last() {
        Some(last) if parts.len() > 1 => tier_rank(last).inspect(|_| {
            parts.pop();
        }),
        _ => None,
    };

    let mut family = parts.join("-");
    for v in variants {
        family.push('-');
        family.push_str(v);
    }

    ParsedModelId {
        family,
        effort_rank,
    }
}

/// Drop the effort word from a catalog display name, so the family reads as
/// "Claude Opus 5" rather than "Claude Opus 5 Max".
pub fn family_display_name(display: &str) -> String {
    const EFFORT_WORDS: &[&str] = &[
        "low", "medium", "high", "xhigh", "x-high", "max", "minimal", "none",
    ];
    let mut words: Vec<&str> = Vec::new();
    let mut iter = display.split_whitespace().peekable();
    while let Some(w) = iter.next() {
        // "No Thinking" is how the catalog spells the `none` tier.
        if w.eq_ignore_ascii_case("no")
            && iter
                .peek()
                .is_some_and(|n| n.eq_ignore_ascii_case("thinking"))
        {
            iter.next();
            continue;
        }
        if EFFORT_WORDS.iter().any(|e| w.eq_ignore_ascii_case(e)) {
            continue;
        }
        words.push(w);
    }
    if words.is_empty() {
        display.to_string()
    } else {
        words.join(" ")
    }
}

/// One family with the concrete ids it was folded from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Family {
    pub id: String,
    /// `(effort rank, concrete id)`, weakest effort first. A member with no
    /// effort suffix is stored with rank `None`.
    pub members: Vec<(Option<u8>, String)>,
}

impl Family {
    /// The concrete id to request at `effort`: the member with that exact rank,
    /// else the nearest one, preferring the stronger side on a tie. A family with
    /// a single effort-less member always resolves to that member.
    pub fn resolve(&self, effort: Effort) -> Option<&str> {
        let want = effort_rank(effort);
        let mut best: Option<(u8, bool, &str)> = None;
        for (rank, id) in &self.members {
            let (distance, stronger) = match rank {
                Some(r) => (r.abs_diff(want), *r >= want),
                // An id without an effort suffix is the family's own default: it
                // is only picked when nothing carries an effort.
                None => (u8::MAX, false),
            };
            let better = match best {
                None => true,
                Some((bd, bs, _)) => distance < bd || (distance == bd && stronger && !bs),
            };
            if better {
                best = Some((distance, stronger, id.as_str()));
            }
        }
        best.map(|(_, _, id)| id)
    }
}

/// Effort a display name spells out, for ids that carry no effort suffix of their
/// own: `swe-1-7` is "SWE-1.7 Max", `glm-5-2` is "GLM-5.2 High".
pub fn effort_rank_from_display(display: &str) -> Option<u8> {
    let lowered = display.to_ascii_lowercase();
    if lowered.ends_with("no thinking") {
        return tier_rank("none");
    }
    let last = lowered.split_whitespace().next_back()?;
    let last = last.replace("x-high", "xhigh");
    tier_rank(&last)
}

/// Fold catalog entries into families, keeping the order in which families first
/// appear. Each entry is `(id, display name)`; the display name only matters for
/// ids without an effort suffix, where it is what names the effort.
pub fn fold_families<'a>(entries: impl IntoIterator<Item = (&'a str, &'a str)>) -> Vec<Family> {
    let mut families: Vec<Family> = Vec::new();
    for (id, display) in entries {
        let mut parsed = parse_model_id(id);
        if parsed.effort_rank.is_none() {
            parsed.effort_rank = effort_rank_from_display(display);
        }
        match families.iter_mut().find(|f| f.id == parsed.family) {
            Some(f) => f.members.push((parsed.effort_rank, id.to_string())),
            None => families.push(Family {
                id: parsed.family,
                members: vec![(parsed.effort_rank, id.to_string())],
            }),
        }
    }
    for f in &mut families {
        f.members.sort_by_key(|(rank, _)| *rank);
    }
    families
}
