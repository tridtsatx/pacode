use pretty_assertions::assert_eq;

use super::*;

fn row(hash: &str, parents: &[&str]) -> CommitRow {
    CommitRow {
        hash: hash.into(),
        parents: parents.iter().map(|p| p.to_string()).collect(),
        ..CommitRow::default()
    }
}

fn lines(rows: &[CommitRow]) -> Vec<String> {
    lanes(rows).iter().map(|c| c.line(false)).collect()
}

fn lines_ascii(rows: &[CommitRow]) -> Vec<String> {
    lanes(rows).iter().map(|c| c.line(true)).collect()
}

#[test]
fn linear_history() {
    let rows = vec![row("c", &["b"]), row("b", &["a"]), row("a", &[])];
    assert_eq!(lines(&rows), vec!["●", "●", "●"]);
    assert_eq!(lines_ascii(&rows), vec!["*", "*", "*"]);
}

#[test]
fn merge_commit_joins_below() {
    // Canonical order: merge, first-parent side, side branch, shared base.
    let rows = vec![
        row("m", &["a", "f2"]),
        row("a", &["r"]),
        row("f2", &["f1"]),
        row("f1", &["r"]),
        row("r", &[]),
    ];
    assert_eq!(lines(&rows), vec!["●╰", "●│", "│●", "│●", "●╯"]);
    assert_eq!(lines_ascii(&rows), vec!["*\\", "*|", "|*", "|*", "*/"]);
}

#[test]
fn merge_with_date_skewed_order() {
    // Real `git log` order when the base is dated inside the side branch:
    // f1 arrives after r, so its edge to r simply ends (no zombie lane).
    let rows = vec![
        row("m", &["a", "f2"]),
        row("a", &["r"]),
        row("f2", &["f1"]),
        row("r", &[]),
        row("f1", &["r"]),
    ];
    assert_eq!(lines(&rows), vec!["●╰", "●│", "│●", "●│", " ●"]);
}

#[test]
fn two_parallel_branches_join_at_base() {
    // Two tips interleaved (as `git log` would emit them); they converge on
    // the shared base commit.
    let rows = vec![
        row("m2", &["m1"]),
        row("t2", &["t1"]),
        row("m1", &["r"]),
        row("t1", &["r"]),
        row("r", &[]),
    ];
    assert_eq!(lines(&rows), vec!["●", "│●", "●│", "│●", "●╯"]);
}

#[test]
fn shallow_boundary_keeps_edge_open() {
    // c1's parent x is outside the fetched page (shallow or not yet paged):
    // its lane stays a bare edge for the rows below.
    let rows = vec![row("c2", &["c1"]), row("c1", &["x"]), row("d", &[])];
    assert_eq!(lines(&rows), vec!["●", "●", "│●"]);
}

#[test]
fn octopus_fork_opens_two_lanes() {
    let rows = vec![
        row("o", &["a", "b", "c"]),
        row("a", &[]),
        row("b", &[]),
        row("c", &[]),
    ];
    // a, b, c are leaf rows: each closes its lane as it lands.
    assert_eq!(lines(&rows), vec!["●╰╰", "●││", " ●│", "  ●"]);
}

#[test]
fn extra_parent_into_waiting_lane_is_a_junction() {
    // d's extra parent c is already awaited by a's fork: the edge joins the
    // live lane instead of opening a second one.
    let rows = vec![
        row("a", &["b", "c"]),
        row("d", &["e", "c"]),
        row("b", &[]),
        row("e", &[]),
        row("c", &[]),
    ];
    assert_eq!(lines(&rows), vec!["●╰", "│├●", "●││", " │●", " ●"]);
}

#[test]
fn edges_cross_live_lanes_as_junctions() {
    // m's extra parent x is awaited at lane 0: the edge passes left through
    // lane 1. Later x closes lane 3 into the node at lane 0, crossing lanes
    // 1 and 2.
    let rows = vec![
        row("t", &["x"]),
        row("u", &["y"]),
        row("m", &["a", "x"]),
        row("w", &["x"]),
        row("x", &[]),
        row("a", &[]),
        row("y", &[]),
    ];
    assert_eq!(
        lines(&rows),
        vec!["●", "│●", "├├●", "│││●", "●├├╯", " │●", " ●"]
    );
}

#[test]
fn more_than_max_lanes_overflows() {
    let rows: Vec<CommitRow> = (1..=8)
        .map(|i| row(&format!("t{i}"), &[&format!("p{i}")]))
        .collect();
    let cells = lanes(&rows);
    assert!(!cells[..6].iter().any(|c| c.overflow));
    assert!(cells[6].overflow);
    assert!(cells[7].overflow);
    assert_eq!(cells[6].line(false), "││││││»");
    assert_eq!(cells[6].line(true), "||||||>");
}

#[test]
fn glyphs_map_to_ascii() {
    assert_eq!(LaneMark::Node.glyph(true), '*');
    assert_eq!(LaneMark::Line.glyph(true), '|');
    assert_eq!(LaneMark::Junction.glyph(true), '+');
    assert_eq!(LaneMark::Merge.glyph(true), '/');
    assert_eq!(LaneMark::Fork.glyph(true), '\\');
    assert_eq!(LaneMark::Blank.glyph(true), ' ');
}

#[test]
fn empty_input() {
    assert!(lanes(&[]).is_empty());
}
