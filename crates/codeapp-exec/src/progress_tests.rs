use super::*;

#[test]
fn cargo_test_progress_flow() {
    let mut parser = ProgressParser::new();

    let p1 = parser.feed_line("running 3 tests", 100);
    assert!(p1.is_some());
    let p1 = p1.unwrap();
    assert_eq!(p1.current, Some(0));
    assert_eq!(p1.total, Some(3));
    assert_eq!(p1.percent, Some(0.0));

    let p2 = parser.feed_line("test tests::test_one ... ok", 101);
    assert!(p2.is_some());
    let p2 = p2.unwrap();
    assert_eq!(p2.current, Some(1));
    assert_eq!(p2.total, Some(3));
    assert_eq!(p2.message.as_deref(), Some("tests::test_one"));

    let p3 = parser.feed_line("test tests::test_two ... ok", 102);
    assert!(p3.is_some());
    let p3 = p3.unwrap();
    assert_eq!(p3.current, Some(2));
    assert_eq!(p3.total, Some(3));
    assert_eq!(p3.message.as_deref(), Some("tests::test_two"));

    let p4 = parser.feed_line(
        "test result: ok. 2 passed; 0 failed; 0 ignored; finished in 0.01s",
        103,
    );
    assert!(p4.is_some());
    let p4 = p4.unwrap();
    assert_eq!(p4.current, Some(3));
    assert_eq!(p4.total, Some(3));
    assert_eq!(p4.percent, Some(100.0));

    assert_eq!(parser.last(), Some(&p4));
}

#[test]
fn cargo_build_and_warnings_errors() {
    let mut parser = ProgressParser::new();

    let p = parser.feed_line("   Compiling codeapp-exec v0.1.0 (/path)", 100);
    assert!(p.is_some());
    let p = p.unwrap();
    assert_eq!(p.current, None);
    assert_eq!(p.total, None);
    assert_eq!(p.message.as_deref(), Some("Compiling codeapp-exec"));

    let _ = parser.feed_line("warning: unused variable `x`", 101);
    assert_eq!(parser.warnings(), 1);

    let _ = parser.feed_line("error[E0425]: cannot find value `y` in this scope", 102);
    assert_eq!(parser.errors(), 1);

    let _ = parser.feed_line("warning[dead_code]: struct `Foo` is never constructed", 103);
    assert_eq!(parser.warnings(), 2);
}

#[test]
fn eslint_problems_summary() {
    let mut parser = ProgressParser::new();
    let _ = parser.feed_line("✖ 3 problems (2 errors, 1 warning)", 100);
    assert_eq!(parser.errors(), 2);
    assert_eq!(parser.warnings(), 1);
}

#[test]
fn jest_tests_line() {
    let mut parser = ProgressParser::new();
    let p = parser.feed_line("Tests: 3 failed, 200 passed, 203 total", 100);
    assert!(p.is_some());
    let p = p.unwrap();
    assert_eq!(p.current, Some(203));
    assert_eq!(p.total, Some(203));
    assert_eq!(p.percent, Some(100.0));
}

#[test]
fn pytest_progress() {
    let mut parser = ProgressParser::new();
    let p1 = parser.feed_line("collected 50 items", 100);
    assert!(p1.is_some());
    let p1 = p1.unwrap();
    assert_eq!(p1.total, Some(50));
    assert_eq!(p1.current, Some(0));

    let p2 = parser.feed_line("tests/test_foo.py ....   [ 45%]", 101);
    assert!(p2.is_some());
    let p2 = p2.unwrap();
    assert_eq!(p2.percent, Some(45.0));
    assert_eq!(p2.total, Some(50));
}

#[test]
fn generic_ratio_and_percent() {
    let mut parser = ProgressParser::new();
    let p1 = parser.feed_line("Downloaded 5 / 20", 100);
    assert!(p1.is_some());
    let p1 = p1.unwrap();
    assert_eq!(p1.current, Some(5));
    assert_eq!(p1.total, Some(20));
    assert_eq!(p1.percent, Some(25.0));

    let p2 = parser.feed_line("Building packages 80%", 101);
    assert!(p2.is_some());
    let p2 = p2.unwrap();
    assert_eq!(p2.percent, Some(80.0));
}

#[test]
fn ansi_escape_stripping() {
    let mut parser = ProgressParser::new();
    let p = parser.feed_line("\x1b[32mrunning 5 tests\x1b[0m", 100);
    assert!(p.is_some());
    let p = p.unwrap();
    assert_eq!(p.total, Some(5));
}

#[test]
fn unchanged_line_returns_none() {
    let mut parser = ProgressParser::new();
    let _ = parser.feed_line("Building packages 80%", 100);
    let p2 = parser.feed_line("Building packages 80%", 101);
    assert!(p2.is_none());
}
