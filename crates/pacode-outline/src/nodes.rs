//! Small syntax-tree helpers shared by the walk. Rows are 0-based line indexes.

use tree_sitter::Node;

/// First row of a node.
pub(crate) fn start_row(node: Node<'_>) -> usize {
    node.start_position().row
}

/// Last row that holds a byte of the node. A node that ends at column 0 (its text
/// includes the newline, e.g. a Rust `///` comment or a Markdown section) ends on the
/// previous row.
pub(crate) fn last_row(node: Node<'_>) -> usize {
    let start = node.start_position();
    let end = node.end_position();
    if end.column == 0 && end.row > start.row {
        end.row - 1
    } else {
        end.row
    }
}

/// Named children in order (cursor walk: linear, unlike repeated `named_child(i)`).
pub(crate) fn named_children<'t>(node: Node<'t>) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

/// First child (named or not).
pub(crate) fn first_child<'t>(node: Node<'t>) -> Option<Node<'t>> {
    node.child(0)
}

/// Last child (named or not).
pub(crate) fn last_child<'t>(node: Node<'t>) -> Option<Node<'t>> {
    let count = node.child_count();
    let index = u32::try_from(count.checked_sub(1)?).ok()?;
    node.child(index)
}

/// A punctuation / keyword token that is part of the grammar (`{`, `}`, `end`), not a
/// comment or other extra.
pub(crate) fn is_token(node: Node<'_>) -> bool {
    !node.is_named() && !node.is_extra()
}

/// Byte offsets where each line starts, for mapping a byte to its row.
pub(crate) fn line_starts(src: &[u8]) -> Vec<usize> {
    let mut starts = vec![0];
    starts.extend(
        src.iter()
            .enumerate()
            .filter(|(_, b)| **b == b'\n')
            .map(|(i, _)| i + 1),
    );
    starts
}

/// Row of byte `offset` given [`line_starts`].
pub(crate) fn row_of_byte(starts: &[usize], offset: usize) -> usize {
    starts.partition_point(|s| *s <= offset).saturating_sub(1)
}

/// Last row of a declaration's header: the body's opening token row (`{`, `do`), or
/// the row of the last token before the body (Python's `:`), or the first row when
/// the body is the node itself. `starts` = [`line_starts`] of `src`.
pub(crate) fn header_end(src: &[u8], starts: &[usize], node: Node<'_>, body: Node<'_>) -> usize {
    if body == node {
        return start_row(node);
    }
    if first_child(body).is_some_and(is_token) {
        return start_row(body).max(start_row(node));
    }
    let floor = node.start_byte();
    let mut pos = body.start_byte().saturating_sub(1).max(floor);
    while pos > floor && src.get(pos).is_some_and(u8::is_ascii_whitespace) {
        pos -= 1;
    }
    row_of_byte(starts, pos).max(start_row(node))
}

/// Row of the closing token (`}`, `end`, `#endif`) that ends `node` or its body, when
/// it is below the header. `None` for bodies without one (Python, YAML).
pub(crate) fn closer_row(node: Node<'_>, body: Node<'_>, header_end: usize) -> Option<usize> {
    let token = match last_child(node) {
        Some(last) if is_token(last) => Some(last),
        _ if body.end_byte() == node.end_byte() => last_child(body).filter(|n| is_token(*n)),
        _ => None,
    };
    token.map(start_row).filter(|row| *row > header_end)
}

/// Source text of a node (lossy: a node can never split a UTF-8 sequence of valid
/// input, but the walk never panics on odd input either).
pub(crate) fn text_of<'s>(src: &'s [u8], node: Node<'_>) -> std::borrow::Cow<'s, str> {
    let bytes = src
        .get(node.start_byte()..node.end_byte())
        .unwrap_or_default();
    String::from_utf8_lossy(bytes)
}
