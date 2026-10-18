use std::path::Path;

use pretty_assertions::assert_eq;
use tempfile::TempDir;

use crate::{LoadOptions, Mode, ModeFilter, Origin, RuleSet, Severity, path_excluded, scan_added};

fn rule_file(name: &str, pattern: &str, severity: &str, message: &str) -> String {
    format!(
        "---\nname: {name}\nlanguages: [ts]\nmatch: '{pattern}'\nseverity: {severity}\n---\n{message}\n"
    )
}

fn dirs() -> (TempDir, LoadOptions) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let project = tmp.path().join("project");
    let global = tmp.path().join("global");
    std::fs::create_dir_all(&project).expect("mkdir");
    std::fs::create_dir_all(&global).expect("mkdir");
    let opts = LoadOptions {
        builtin: true,
        disable: Vec::new(),
        project_dir: Some(project),
        global_dir: Some(global),
    };
    (tmp, opts)
}

fn write(dir: &Option<std::path::PathBuf>, file: &str, content: &str) {
    let dir = dir.as_ref().expect("dir");
    std::fs::write(dir.join(file), content).expect("write");
}

#[test]
fn project_rule_overrides_builtin_of_same_name() {
    let (_tmp, opts) = dirs();
    write(
        &opts.project_dir,
        "any.md",
        &rule_file(
            "ts-no-explicit-any",
            "\\bFORBIDDEN\\b",
            "warn",
            "custom message",
        ),
    );
    let loaded = RuleSet::load(&opts);
    assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
    let rule = loaded.set.get("ts-no-explicit-any").expect("rule");
    assert_eq!(rule.message, "custom message");
    assert_eq!(rule.severity, Severity::Warn);
    assert_eq!(rule.mode, Mode::AfterWrite);
    assert!(matches!(rule.origin, Origin::Project(_)));
    // the original pattern no longer applies, the new one does
    let report = scan_added(
        &loaded.set,
        Path::new("a.ts"),
        None,
        "const x: any = 1;\nconst FORBIDDEN = 1;\n",
        ModeFilter::AfterWrite,
    );
    assert_eq!(report.rule_names(), vec!["ts-no-explicit-any"]);
    assert_eq!(report.findings[0].line, 2);
}

#[test]
fn project_beats_global_beats_builtin() {
    let (_tmp, opts) = dirs();
    write(
        &opts.global_dir,
        "a.md",
        &rule_file("ts-no-var", "G", "warn", "global"),
    );
    assert_eq!(
        RuleSet::load(&opts)
            .set
            .get("ts-no-var")
            .map(|r| r.message.clone()),
        Some("global".to_string())
    );
    write(
        &opts.project_dir,
        "a.md",
        &rule_file("ts-no-var", "P", "warn", "project"),
    );
    assert_eq!(
        RuleSet::load(&opts)
            .set
            .get("ts-no-var")
            .map(|r| r.message.clone()),
        Some("project".to_string())
    );
}

#[test]
fn disable_removes_builtin_and_user_rules() {
    let (_tmp, mut opts) = dirs();
    write(
        &opts.project_dir,
        "mine.md",
        &rule_file("mine", "x", "warn", "m"),
    );
    opts.disable = vec!["ts-no-var".to_string(), "mine".to_string()];
    let set = RuleSet::load(&opts).set;
    assert!(set.get("ts-no-var").is_none());
    assert!(set.get("mine").is_none());
    assert!(set.get("ts-no-console-log").is_some());
}

#[test]
fn builtin_false_leaves_only_user_rules() {
    let (_tmp, mut opts) = dirs();
    write(
        &opts.project_dir,
        "mine.md",
        &rule_file("mine", "x", "warn", "m"),
    );
    opts.builtin = false;
    let set = RuleSet::load(&opts).set;
    let names: Vec<&str> = set.rules().iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, vec!["mine"]);
}

#[test]
fn bad_files_are_reported_not_fatal() {
    let (_tmp, opts) = dirs();
    write(&opts.project_dir, "broken.md", "no frontmatter here");
    write(
        &opts.project_dir,
        "badre.md",
        &rule_file("badre", "(", "warn", "m"),
    );
    write(&opts.project_dir, "notes.txt", "ignored, not .md");
    write(
        &opts.project_dir,
        "good.md",
        &rule_file("good", "x", "warn", "m"),
    );
    let loaded = RuleSet::load(&opts);
    assert_eq!(loaded.errors.len(), 2, "{:?}", loaded.errors);
    assert!(loaded.set.get("good").is_some());
    assert!(loaded.set.get("badre").is_none());
}

#[test]
fn missing_dirs_are_fine() {
    let opts = LoadOptions {
        builtin: true,
        project_dir: Some("/nonexistent/pacode-rules-test".into()),
        global_dir: None,
        disable: Vec::new(),
    };
    let loaded = RuleSet::load(&opts);
    assert!(loaded.errors.is_empty());
    assert!(!loaded.set.is_empty());
}

#[test]
fn fingerprint_changes_with_files_and_options() {
    let (_tmp, mut opts) = dirs();
    let empty = opts.fingerprint();
    assert_eq!(empty, opts.fingerprint());
    write(&opts.project_dir, "a.md", &rule_file("a", "x", "warn", "m"));
    let one = opts.fingerprint();
    assert!(one != empty);
    // size change is detected even when mtime granularity is coarse
    write(
        &opts.project_dir,
        "a.md",
        &rule_file("a", "x", "warn", "a longer message"),
    );
    assert!(opts.fingerprint() != one);
    let before = opts.fingerprint();
    opts.disable.push("x".to_string());
    assert!(opts.fingerprint() != before);
}

#[test]
fn applicable_filters_by_extension_mode_and_path() {
    let set = RuleSet::load(&LoadOptions {
        builtin: true,
        ..LoadOptions::default()
    })
    .set;
    let names = |p: &str, f: ModeFilter| -> Vec<String> {
        set.applicable(Path::new(p), f)
            .iter()
            .map(|r| r.name.clone())
            .collect()
    };
    assert!(names("a.rs", ModeFilter::AfterWrite).contains(&"rust-no-dbg".to_string()));
    assert_eq!(names("a.rs", ModeFilter::StreamOnly), vec!["rust-no-dbg"]);
    assert_eq!(
        names("A.TS", ModeFilter::StreamOnly),
        vec!["ts-no-explicit-any"]
    );
    assert!(names("a.d.ts", ModeFilter::StreamOnly).is_empty());
    assert!(names("README.md", ModeFilter::AfterWrite).is_empty());
}

#[test]
fn exclusion_patterns() {
    let p = |pats: &[&str], path: &str| {
        let v: Vec<String> = pats.iter().map(|s| s.to_string()).collect();
        path_excluded(&v, path)
    };
    assert!(p(&["tests/"], "crates/x/tests/it.rs"));
    assert!(!p(&["tests/"], "crates/x/src/tests.rs"));
    assert!(!p(&["tests/"], "tests"));
    assert!(p(&["*_tests.rs"], "src/a_tests.rs"));
    assert!(!p(&["*_tests.rs"], "src/a_test.rs"));
    assert!(p(&["main.rs"], "src/main.rs"));
    assert!(!p(&["main.rs"], "src/domain.rs"));
    assert!(p(&["src/bin/x.rs"], "crates/a/src/bin/x.rs"));
    assert!(p(&["*.d.ts"], "types\\a.d.ts"));
}
