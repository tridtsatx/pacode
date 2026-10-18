use super::*;

fn anchor(content: &str, line: usize) -> Anchor {
    Anchor::for_line(line, content.lines().nth(line - 1).unwrap_or(""))
}

fn lines(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_string()).collect()
}

fn replace(c: &str, s: usize, e: usize, new: &[&str]) -> Op {
    Op::Replace {
        start: anchor(c, s),
        end: if e == s { None } else { Some(anchor(c, e)) },
        lines: lines(new),
    }
}

fn after(c: &str, at: usize, new: &[&str]) -> Op {
    Op::InsertAfter {
        at: anchor(c, at),
        lines: lines(new),
    }
}

fn before(c: &str, at: usize, new: &[&str]) -> Op {
    Op::InsertBefore {
        at: anchor(c, at),
        lines: lines(new),
    }
}

fn delete(c: &str, s: usize, e: usize) -> Op {
    Op::Delete {
        start: anchor(c, s),
        end: Some(anchor(c, e)),
    }
}

fn run(c: &str, ops: &[Op]) -> String {
    apply_ops(c, ops).unwrap().content
}

const SRC: &str = "one\ntwo\nthree\nfour\nfive\n";

#[test]
fn replace_single_and_range() {
    assert_eq!(
        run(SRC, &[replace(SRC, 2, 2, &["TWO"])]),
        "one\nTWO\nthree\nfour\nfive\n"
    );
    assert_eq!(run(SRC, &[replace(SRC, 2, 4, &["X"])]), "one\nX\nfive\n");
    assert_eq!(
        run(SRC, &[replace(SRC, 2, 2, &["a", "b", "c"])]),
        "one\na\nb\nc\nthree\nfour\nfive\n"
    );
}

#[test]
fn delete_range_and_replace_with_nothing() {
    assert_eq!(run(SRC, &[delete(SRC, 2, 3)]), "one\nfour\nfive\n");
    assert_eq!(run(SRC, &[replace(SRC, 1, 5, &[])]), "");
}

#[test]
fn insert_before_and_after() {
    assert_eq!(
        run(SRC, &[after(SRC, 2, &["new"])]),
        "one\ntwo\nnew\nthree\nfour\nfive\n"
    );
    assert_eq!(
        run(SRC, &[before(SRC, 2, &["new"])]),
        "one\nnew\ntwo\nthree\nfour\nfive\n"
    );
}

#[test]
fn insert_at_start_and_end() {
    let ops = [
        Op::InsertAfter {
            at: Anchor::Bof,
            lines: lines(&["top"]),
        },
        Op::InsertAfter {
            at: Anchor::Eof,
            lines: lines(&["bottom"]),
        },
    ];
    assert_eq!(run(SRC, &ops), "top\none\ntwo\nthree\nfour\nfive\nbottom\n");
    let by_line = [before(SRC, 1, &["top"]), after(SRC, 5, &["bottom"])];
    assert_eq!(
        run(SRC, &by_line),
        "top\none\ntwo\nthree\nfour\nfive\nbottom\n"
    );
    let flipped = [
        Op::InsertBefore {
            at: Anchor::Bof,
            lines: lines(&["top"]),
        },
        Op::InsertBefore {
            at: Anchor::Eof,
            lines: lines(&["bottom"]),
        },
    ];
    assert_eq!(run(SRC, &flipped), run(SRC, &ops));
}

#[test]
fn empty_file_accepts_inserts() {
    let ops = [Op::InsertAfter {
        at: Anchor::Eof,
        lines: lines(&["a", "b"]),
    }];
    assert_eq!(run("", &ops), "a\nb\n");
    let bof = [Op::InsertBefore {
        at: Anchor::Bof,
        lines: lines(&["a"]),
    }];
    assert_eq!(run("", &bof), "a\n");
}

#[test]
fn empty_file_rejects_line_anchor() {
    let op = Op::Delete {
        start: Anchor::for_line(1, ""),
        end: None,
    };
    assert!(matches!(apply_ops("", &[op]), Err(EditError::Stale { .. })));
}

#[test]
fn multiple_ops_use_original_numbering() {
    let ops = [
        replace(SRC, 1, 1, &["ONE", "ONE-B"]),
        delete(SRC, 3, 3),
        after(SRC, 5, &["six"]),
    ];
    let out = apply_ops(SRC, &ops).unwrap();
    assert_eq!(out.content, "ONE\nONE-B\ntwo\nfour\nfive\nsix\n");
    assert_eq!((out.added, out.removed), (3, 2));
    assert_eq!(out.first_changed_line(), Some(1));
    assert_eq!(out.changed.len(), 3);
}

#[test]
fn ops_may_be_given_in_any_order_and_touch_boundaries() {
    let ops = [
        before(SRC, 4, &["x"]),
        replace(SRC, 2, 3, &["mid"]),
        after(SRC, 3, &["y"]),
    ];
    assert_eq!(run(SRC, &ops), "one\nmid\nx\ny\nfour\nfive\n");
}

#[test]
fn inserts_at_same_gap_keep_op_order() {
    let ops = [after(SRC, 2, &["first"]), before(SRC, 3, &["second"])];
    assert_eq!(
        run(SRC, &ops),
        "one\ntwo\nfirst\nsecond\nthree\nfour\nfive\n"
    );
}

#[test]
fn overlapping_ops_are_rejected() {
    let err = apply_ops(
        SRC,
        &[replace(SRC, 2, 4, &["a"]), replace(SRC, 4, 5, &["b"])],
    )
    .unwrap_err();
    assert!(
        matches!(
            err,
            EditError::Overlap {
                first: 1,
                second: 2,
                ..
            }
        ),
        "{err:?}"
    );
    let err = apply_ops(SRC, &[replace(SRC, 2, 4, &["a"]), after(SRC, 2, &["b"])]).unwrap_err();
    assert!(matches!(err, EditError::Overlap { .. }), "{err:?}");
    let err = apply_ops(SRC, &[delete(SRC, 2, 3), delete(SRC, 2, 3)]).unwrap_err();
    assert!(matches!(err, EditError::Overlap { .. }));
    let err = apply_ops(SRC, &[replace(SRC, 1, 5, &["z"]), before(SRC, 3, &["b"])]).unwrap_err();
    assert!(matches!(err, EditError::Overlap { .. }));
}

#[test]
fn reversed_and_structural_errors() {
    let rev = Op::Delete {
        start: anchor(SRC, 4),
        end: Some(anchor(SRC, 2)),
    };
    assert!(matches!(
        apply_ops(SRC, &[rev]),
        Err(EditError::ReversedRange { op: 1, .. })
    ));
    assert_eq!(apply_ops(SRC, &[]).unwrap_err(), EditError::NoOps);
    let empty = Op::InsertAfter {
        at: anchor(SRC, 1),
        lines: vec![],
    };
    assert_eq!(
        apply_ops(SRC, &[empty]).unwrap_err(),
        EditError::EmptyInsert { op: 1 }
    );
    let eof_range = Op::Delete {
        start: Anchor::Eof,
        end: None,
    };
    assert!(matches!(
        apply_ops(SRC, &[eof_range]),
        Err(EditError::NotALineAnchor {
            op: 1,
            role: "start"
        })
    ));
}

#[test]
fn stale_anchor_rejects_whole_edit_with_fresh_context() {
    let changed = "one\nTWO CHANGED\nthree\nfour\nfive\n";
    let ops = [replace(SRC, 1, 1, &["ONE"]), replace(SRC, 2, 2, &["x"])];
    let EditError::Stale { anchors, message } = apply_ops(changed, &ops).unwrap_err() else {
        panic!("expected Stale");
    };
    assert_eq!(anchors.len(), 1);
    assert_eq!((anchors[0].op, anchors[0].role), (2, "start"));
    assert!(message.contains("nothing written"));
    assert!(message.contains(&format!("2#{}| TWO CHANGED", line_hash("TWO CHANGED"))));
    assert!(message.contains("> 2#"), "focus line marked: {message}");
    assert!(message.contains(&format!("1#{}| one", line_hash("one"))));
}

#[test]
fn every_stale_anchor_is_reported() {
    let changed = "a\nb\nc\nd\ne\nf\n";
    let orig = "1\n2\n3\n4\n5\n6\n";
    let ops = [delete(orig, 1, 1), delete(orig, 6, 6)];
    let EditError::Stale { anchors, .. } = apply_ops(changed, &ops).unwrap_err() else {
        panic!("expected Stale");
    };
    // Both ops are reported; an op whose start and end anchors are both stale
    // lists each of them.
    let ops_hit: std::collections::BTreeSet<usize> = anchors.iter().map(|a| a.op).collect();
    assert_eq!(ops_hit.len(), 2);
}

#[test]
fn anchor_past_end_of_file_is_stale_not_a_panic() {
    let long = "a\nb\nc\nd\ne\nf\ng\nh\n";
    let op = Op::Delete {
        start: anchor(long, 8),
        end: None,
    };
    let short = "a\nb\n";
    let EditError::Stale { message, .. } = apply_ops(short, &[op]).unwrap_err() else {
        panic!("expected Stale");
    };
    assert!(message.contains("does not exist"));
    assert!(message.contains("2#"));
}

#[test]
fn drifted_anchor_is_relocated_when_unique() {
    let shifted = "new1\nnew2\nnew3\none\ntwo\nthree\nfour\nfive\n";
    let out = apply_ops(shifted, &[replace(SRC, 2, 2, &["TWO"])]).unwrap();
    assert_eq!(
        out.content,
        "new1\nnew2\nnew3\none\nTWO\nthree\nfour\nfive\n"
    );
    assert_eq!(
        out.relocations,
        vec![Relocation {
            op: 1,
            role: "start",
            from: 2,
            to: 5
        }]
    );
}

#[test]
fn relocation_works_for_range_ends_and_upward_drift() {
    let shifted = "two\nthree\nfour\nfive\n";
    let out = apply_ops(shifted, &[replace(SRC, 2, 3, &["X"])]).unwrap();
    assert_eq!(out.content, "X\nfour\nfive\n");
    assert_eq!(out.relocations.len(), 2);
}

#[test]
fn ambiguous_or_distant_hash_is_not_relocated() {
    // `x` occurs on lines 1 and 3 around the mismatching line 2: ambiguous.
    let op = Op::Delete {
        start: Anchor::for_line(2, "x"),
        end: None,
    };
    assert!(matches!(
        apply_ops("x\nA\nx\n", &[op]),
        Err(EditError::Stale { .. })
    ));

    let mut far = String::new();
    for _ in 0..(RELOCATE_WINDOW + 5) {
        far.push_str("pad\n");
    }
    far.push_str("target\n");
    let moved = Op::Delete {
        start: Anchor::for_line(1, "target"),
        end: None,
    };
    assert!(matches!(
        apply_ops(&far, &[moved]),
        Err(EditError::Stale { .. })
    ));
}

#[test]
fn exact_line_match_wins_over_nearby_duplicates() {
    let c = "x\nx\nx\n";
    let out = apply_ops(
        c,
        &[Op::Replace {
            start: Anchor::for_line(2, "x"),
            end: None,
            lines: lines(&["y"]),
        }],
    )
    .unwrap();
    assert_eq!(out.content, "x\ny\nx\n");
    assert!(out.relocations.is_empty());
}

#[test]
fn crlf_is_preserved() {
    let c = "one\r\ntwo\r\nthree\r\n";
    let out = run(
        c,
        &[replace(c, 2, 2, &["TWO", "TWO-B"]), after(c, 3, &["four"])],
    );
    assert_eq!(out, "one\r\nTWO\r\nTWO-B\r\nthree\r\nfour\r\n");
}

#[test]
fn mixed_endings_keep_untouched_lines_and_use_dominant_for_new() {
    let c = "a\r\nb\r\nc\nd\r\n";
    let out = run(c, &[replace(c, 2, 2, &["B"])]);
    assert_eq!(out, "a\r\nB\r\nc\nd\r\n");
}

#[test]
fn missing_final_newline_is_preserved() {
    let c = "a\nb";
    assert_eq!(run(c, &[replace(c, 2, 2, &["B"])]), "a\nB");
    assert_eq!(run(c, &[after(c, 2, &["c"])]), "a\nb\nc");
    assert_eq!(run(c, &[delete(c, 2, 2)]), "a");
    assert_eq!(run(c, &[replace(c, 1, 1, &["A"])]), "A\nb");
}

#[test]
fn final_newline_is_kept_when_original_has_it() {
    let c = "a\nb\n";
    assert!(run(c, &[replace(c, 2, 2, &["B"])]).ends_with("B\n"));
    assert!(run(c, &[after(c, 2, &["c"])]).ends_with("c\n"));
}

#[test]
fn unicode_lines_and_replacements() {
    let c = "héllo\n日本語\n🎉 party\n";
    let out = run(
        c,
        &[replace(c, 2, 2, &["日本語 ✓"]), after(c, 3, &["fin ñ"])],
    );
    assert_eq!(out, "héllo\n日本語 ✓\n🎉 party\nfin ñ\n");
}

#[test]
fn blank_lines_can_be_replaced_and_inserted() {
    let c = "a\n\nb\n";
    let out = run(c, &[replace(c, 2, 2, &["", ""])]);
    assert_eq!(out, "a\n\n\nb\n");
}

#[test]
fn replacement_lines_with_embedded_newlines_are_split() {
    let c = "a\nb\n";
    let out = run(c, &[replace(c, 2, 2, &["x\ny\n", "z\r"])]);
    assert_eq!(out, "a\nx\ny\nz\n");
}

#[test]
fn copied_display_prefixes_are_stripped() {
    let c = "a\nb\n";
    let pasted = format!("2#{}| B", line_hash("B"));
    let out = run(
        c,
        &[Op::Replace {
            start: anchor(c, 2),
            end: None,
            lines: vec![pasted],
        }],
    );
    assert_eq!(out, "a\nB\n");
    // a single non-prefixed line disables stripping
    let mixed = [format!("2#{}| B", line_hash("B")), "plain".to_string()];
    let out = run(
        c,
        &[Op::Replace {
            start: anchor(c, 2),
            end: None,
            lines: mixed.to_vec(),
        }],
    );
    assert!(out.contains("| B\nplain"));
}

#[test]
fn changed_ranges_point_into_new_content() {
    let out = apply_ops(
        SRC,
        &[replace(SRC, 2, 3, &["a", "b", "c"]), delete(SRC, 5, 5)],
    )
    .unwrap();
    assert_eq!(out.changed, vec![1..4, 5..5]);
    assert_eq!(out.first_changed_line(), Some(2));
}
