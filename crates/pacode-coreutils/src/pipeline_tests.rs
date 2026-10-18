use std::fs;

use super::{PipelineError, Stage, parse, run_pipeline};

fn tmp() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("a.rs"), "x\ny\n").expect("write");
    fs::write(dir.path().join("b.rs"), "z\n").expect("write");
    fs::write(dir.path().join("c.txt"), "q\n").expect("write");
    fs::write(dir.path().join(".hid.rs"), "h\n").expect("write");
    fs::write(dir.path().join("nums.txt"), "3\n1\n2\n").expect("write");
    fs::create_dir(dir.path().join("sub")).expect("mkdir");
    fs::write(dir.path().join("sub/1.txt"), "s\n").expect("write");
    dir
}

#[test]
fn parses_stages_with_quoting() {
    let dir = tmp();
    let stages = parse("wc -l a.rs | sort -n", dir.path(), false).expect("parse");
    assert_eq!(
        stages,
        vec![
            Stage {
                name: "wc".into(),
                args: vec!["-l".into(), "a.rs".into()]
            },
            Stage {
                name: "sort".into(),
                args: vec!["-n".into()]
            },
        ]
    );
    let stages = parse("sed 's/a b/c|d/' \"f g\" 'it'\\''s'", dir.path(), false).expect("parse");
    assert_eq!(stages[0].args, vec!["s/a b/c|d/", "f g", "it's"]);
    let stages =
        parse("sed 's/.*/x/' f", dir.path(), false).expect("quoted glob chars are literal");
    assert_eq!(stages[0].args[0], "s/.*/x/");
    let stages = parse("tr '' x", dir.path(), false).expect("empty arg");
    assert_eq!(stages[0].args, vec!["", "x"]);
}

#[test]
fn rejects_what_a_pipeline_cannot_express() {
    let dir = tmp();
    for bad in [
        "wc -l a > out",
        "wc < a",
        "wc -l $HOME",
        "wc -l \"$x\"",
        "wc && wc",
        "wc || wc",
        "wc; wc",
        "wc -l `x`",
        "wc -l $(x)",
        "wc &",
        "(wc)",
        "wc ~/x",
        "wc -l a |",
        "| wc",
        "wc 'a",
        "wc \"a",
        "",
        "wc\nwc",
    ] {
        assert!(
            parse(bad, dir.path(), true).is_err(),
            "should reject {bad:?}"
        );
    }
    assert!(matches!(
        parse("ls -la", dir.path(), true),
        Err(PipelineError::UnknownUtility(name)) if name == "ls"
    ));
    assert!(matches!(
        parse("FOO=1 wc", dir.path(), true),
        Err(PipelineError::UnknownUtility(_))
    ));
}

#[test]
fn expands_globs_only_when_asked() {
    let dir = tmp();
    let stages = parse("wc -l *.rs", dir.path(), true).expect("parse");
    assert_eq!(stages[0].args, vec!["-l", "a.rs", "b.rs"]);
    let stages = parse("wc sub/*.txt", dir.path(), true).expect("parse");
    assert_eq!(stages[0].args, vec!["sub/1.txt"]);
    let stages = parse("wc ?.txt", dir.path(), true).expect("parse");
    assert_eq!(stages[0].args, vec!["c.txt"]);
    assert!(matches!(
        parse("wc *.rs", dir.path(), false),
        Err(PipelineError::Glob(_))
    ));
    assert!(matches!(
        parse("wc *.zzz", dir.path(), true),
        Err(PipelineError::Glob(_))
    ));
}

#[test]
fn runs_pipelines_end_to_end() {
    let dir = tmp();
    let run = |cmd: &str| {
        let stages = parse(cmd, dir.path(), true).expect("parse");
        run_pipeline(&stages, None, dir.path())
    };
    assert_eq!(
        String::from_utf8_lossy(&run("sort -n nums.txt | head -n 2").stdout),
        "1\n2\n"
    );
    assert_eq!(
        String::from_utf8_lossy(&run("sort nums.txt | wc -l").stdout),
        "3\n"
    );
    assert_eq!(
        String::from_utf8_lossy(&run("wc -l *.rs | sort -n").stdout),
        "1 b.rs\n2 a.rs\n3 total\n"
    );
}

#[test]
fn failures_are_reported_per_stage() {
    let dir = tmp();
    let stages = parse("wc -l missing.txt | sort", dir.path(), true).expect("parse");
    let out = run_pipeline(&stages, None, dir.path());
    assert_eq!(out.code, 0);
    assert_eq!(out.failure, Some(("wc".to_string(), 1)));
    assert!(out.stderr.contains("missing.txt"), "{}", out.stderr);
    assert!(!out.is_benign());

    let stages = parse("diff a.rs b.rs", dir.path(), true).expect("parse");
    let out = run_pipeline(&stages, None, dir.path());
    assert_eq!(out.code, 1);
    assert!(out.is_benign());

    let stages = parse("sort", dir.path(), true).expect("parse");
    let out = run_pipeline(&stages, None, dir.path());
    assert_eq!(out.code, 2, "a stage without input is a usage error");
}
