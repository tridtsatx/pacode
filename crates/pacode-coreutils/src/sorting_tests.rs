use crate::test_util::{ok, sh};

#[test]
fn sort_orders_lines_bytewise() {
    assert_eq!(ok("sort", &[], "b\na\nc\n"), "a\nb\nc\n");
    assert_eq!(ok("sort", &[], "b\na"), "a\nb\n");
    assert_eq!(ok("sort", &[], "é\nz\na\n"), "a\nz\né\n");
    assert_eq!(ok("sort", &[], ""), "");
    assert_eq!(ok("sort", &[], "b\r\na\r\n"), "a\r\nb\r\n");
}

#[test]
fn sort_numeric_and_reverse() {
    assert_eq!(ok("sort", &["-n"], "10\n9\n100\n"), "9\n10\n100\n");
    assert_eq!(ok("sort", &["-nr"], "10\n9\n100\n"), "100\n10\n9\n");
    assert_eq!(ok("sort", &["-n"], "x\n2\n-1\n1.5\n"), "-1\nx\n1.5\n2\n");
}

#[test]
fn sort_unique_drops_duplicates() {
    assert_eq!(ok("sort", &["-u"], "b\na\nb\n"), "a\nb\n");
}

#[test]
fn sort_by_key_with_delimiter() {
    assert_eq!(
        ok("sort", &["-t", ",", "-k", "2"], "x,3\ny,1\nz,2\n"),
        "y,1\nz,2\nx,3\n"
    );
    assert_eq!(
        ok("sort", &["-k", "2,2n"], "a 10\nb 9\nc 100\n"),
        "b 9\na 10\nc 100\n"
    );
}

#[test]
fn sort_ties_fall_back_to_the_whole_line_unless_stable() {
    assert_eq!(ok("sort", &["-k", "2,2"], "b 1\na 1\n"), "a 1\nb 1\n");
    assert_eq!(ok("sort", &["-s", "-k", "2,2"], "b 1\na 1\n"), "b 1\na 1\n");
    assert_eq!(ok("sort", &["-u", "-k", "2,2"], "a 1\nb 1\n"), "a 1\n");
}

#[test]
fn sort_rejects_what_it_cannot_do() {
    assert_eq!(sh("sort", &["-k", "1.2"], Some("a\n")).1, 2);
    assert_eq!(sh("sort", &["-o", "out"], Some("a\n")).1, 2);
    assert_eq!(sh("sort", &[], None).1, 2);
}

#[test]
fn uniq_collapses_adjacent_duplicates() {
    assert_eq!(ok("uniq", &[], "a\na\nb\n"), "a\nb\n");
    assert_eq!(ok("uniq", &[], "a\nb\na\n"), "a\nb\na\n");
    assert_eq!(ok("uniq", &[], "a\na"), "a\n");
    assert_eq!(ok("uniq", &[], ""), "");
}

#[test]
fn uniq_counts_and_filters() {
    assert_eq!(ok("uniq", &["-c"], "a\na\nb\n"), "      2 a\n      1 b\n");
    assert_eq!(ok("uniq", &["-d"], "a\na\nb\n"), "a\n");
    assert_eq!(ok("uniq", &["-u"], "a\na\nb\n"), "b\n");
    assert_eq!(ok("uniq", &["-i"], "A\na\n"), "A\n");
}
