//! Names and labels of declarations, for markers (`fn foo`) and symbol lookup.

use tree_sitter::Node;

use crate::nodes::{named_children, text_of};
use crate::spec::{DESCEND_FIELDS, LangSpec, NAME_FIELDS, Rule};

/// Name links followed from a name candidate (C declarator chains are the deepest).
const MAX_NAME_HOPS: usize = 8;
const MAX_NAME_CHARS: usize = 80;

/// Declaration name: a name field, else a child of a name kind, followed down
/// declarator / name links; first line of its text, quotes trimmed. Empty when the
/// declaration has none (anonymous function, docstring statement).
pub(crate) fn name_of(src: &[u8], spec: &LangSpec, node: Node<'_>) -> String {
    let found = NAME_FIELDS
        .iter()
        .find_map(|f| node.child_by_field_name(f))
        .or_else(|| name_kind_child(spec, node));
    let Some(mut found) = found else {
        return String::new();
    };
    for _ in 0..MAX_NAME_HOPS {
        // `A::g` / `a.b.C` name themselves: keep the qualified text.
        if found.kind().contains("qualified") || found.kind().contains("scoped") {
            break;
        }
        let next = DESCEND_FIELDS
            .iter()
            .find_map(|f| found.child_by_field_name(f))
            .or_else(|| name_kind_child(spec, found));
        match next {
            Some(n) => found = n,
            None => break,
        }
    }
    let text = text_of(src, found);
    let line = text.lines().next().unwrap_or_default();
    line.trim()
        .trim_matches(|c: char| matches!(c, '"' | '\'' | '`'))
        .trim()
        .chars()
        .take(MAX_NAME_CHARS)
        .collect()
}

fn name_kind_child<'t>(spec: &LangSpec, node: Node<'t>) -> Option<Node<'t>> {
    named_children(node)
        .into_iter()
        .find(|c| spec.name_kinds.contains(&c.kind()))
}

/// The rule's label, or the node's first keyword token (`const`, `struct`).
pub(crate) fn label_of(node: Node<'_>, rule: &'static Rule) -> &'static str {
    if !rule.label.is_empty() {
        return rule.label;
    }
    let mut cursor = node.walk();
    let keyword = node
        .children(&mut cursor)
        .find(|c| {
            !c.is_named() && !c.kind().is_empty() && c.kind().chars().all(char::is_alphabetic)
        })
        .map(|c| c.kind());
    keyword.unwrap_or_default()
}
