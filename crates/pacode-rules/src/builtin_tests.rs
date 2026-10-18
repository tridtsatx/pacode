//! False-positive / false-negative tables for every built-in rule.

use std::path::Path;

use crate::{LoadOptions, ModeFilter, RuleSet, Severity, builtin_rules, scan_added};

fn builtin_set() -> RuleSet {
    let loaded = RuleSet::load(&LoadOptions {
        builtin: true,
        ..LoadOptions::default()
    });
    assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
    loaded.set
}

/// Rules of `rule` that fire when `code` is written as a brand-new file at `path`.
fn fires(rule: &str, path: &str, code: &str) -> bool {
    let set = builtin_set();
    scan_added(&set, Path::new(path), None, code, ModeFilter::AfterWrite)
        .findings
        .iter()
        .any(|f| f.rule == rule)
}

fn check_table(rule: &str, path: &str, hits: &[&str], misses: &[&str]) {
    for code in hits {
        assert!(fires(rule, path, code), "{rule} should fire on: {code}");
    }
    for code in misses {
        assert!(!fires(rule, path, code), "{rule} must NOT fire on: {code}");
    }
}

#[test]
fn all_builtins_parse_and_are_unique() {
    let (rules, errors) = builtin_rules();
    assert!(errors.is_empty(), "{errors:?}");
    let mut names: Vec<&str> = rules.iter().map(|r| r.name.as_str()).collect();
    names.sort_unstable();
    let before = names.len();
    names.dedup();
    assert_eq!(before, names.len(), "duplicate built-in rule names");
    assert_eq!(rules.len(), 15);
}

#[test]
fn severities_and_modes() {
    let set = builtin_set();
    assert_eq!(
        set.get("ts-no-explicit-any").map(|r| r.severity),
        Some(Severity::Block)
    );
    assert_eq!(
        set.get("ts-no-ts-ignore").map(|r| r.severity),
        Some(Severity::Block)
    );
    assert_eq!(
        set.get("rust-no-dbg").map(|r| r.severity),
        Some(Severity::Block)
    );
    assert_eq!(
        set.get("rust-no-unwrap-expect").map(|r| r.severity),
        Some(Severity::Warn)
    );
}

#[test]
fn ts_no_explicit_any() {
    check_table(
        "ts-no-explicit-any",
        "src/a.ts",
        &[
            "const x: any = 1;",
            "const x = y as any;",
            "const x = <any>y;",
            "function f(a: any[]) {}",
            "const m: Record<string, any> = {};",
            "let p: Promise<any>;",
            "type U = string | any;",
            "try {} catch (e: any) {}",
            "const x: Array<any> = [];",
            "const m: Map<string, any[]> = new Map();",
        ],
        &[
            "const many = 1;",
            "// returns any value",
            "const s = \"type: any\";",
            "const company: string = 'x';",
            "/* x: any */ const a = 1;",
            "const anyone = 1;",
            "foo(any, b);",
            "const t = `value: any`;",
            "if (x.any(y)) {}",
            "const x: unknown = 1;",
            "const x: anyThing = 1;",
        ],
    );
    assert!(!fires(
        "ts-no-explicit-any",
        "types/a.d.ts",
        "declare const x: any;"
    ));
    assert!(!fires(
        "ts-no-explicit-any",
        "src/a.rs",
        "const x: any = 1;"
    ));
    assert!(fires(
        "ts-no-explicit-any",
        "src/a.tsx",
        "const x: any = 1;"
    ));
}

#[test]
fn ts_no_ts_ignore() {
    check_table(
        "ts-no-ts-ignore",
        "a.ts",
        &[
            "// @ts-ignore",
            "// @ts-nocheck",
            "/* @ts-ignore */",
            "  // @ts-ignore: legacy",
        ],
        &[
            "// @ts-expect-error because upstream types",
            "const a = 1; // ts ignore",
            "// @ts-ignored",
        ],
    );
}

#[test]
fn ts_no_console_log() {
    check_table(
        "ts-no-console-log",
        "src/a.ts",
        &["console.log(x);", "  console.log (\"a\")"],
        &[
            "console.error(x);",
            "// console.log(x)",
            "const s = \"console.log(x)\";",
            "myconsole.log(x);",
        ],
    );
    assert!(!fires(
        "ts-no-console-log",
        "src/a.test.ts",
        "console.log(x);"
    ));
    assert!(!fires(
        "ts-no-console-log",
        "src/__tests__/a.ts",
        "console.log(x);"
    ));
    assert!(!fires(
        "ts-no-console-log",
        "scripts/build.ts",
        "console.log(x);"
    ));
}

#[test]
fn ts_no_var() {
    check_table(
        "ts-no-var",
        "a.ts",
        &[
            "var x = 1;",
            "  var {a} = b;",
            "for (var i = 0; i < 3; i++) {}",
        ],
        &[
            "const variable = 1;",
            "// var x = 1",
            "const s = \"var x\";",
            "variance = 3;",
        ],
    );
}

#[test]
fn ts_strict_equality() {
    check_table(
        "ts-strict-equality",
        "a.ts",
        &[
            "if (a == b) {}",
            "if (a != b) {}",
            "const t = x==y;",
            "if (typeof a == \"string\") {}",
            "if (a == null || b == c) {}",
        ],
        &[
            "if (a === b) {}",
            "if (a !== b) {}",
            "if (a == null) {}",
            "if (a != null) {}",
            "if (a!=null) {}",
            "if (x >= y) {}",
            "if (x <= y) {}",
            "const f = (a) => a;",
            "// a == b",
            "const s = \"a == b\";",
        ],
    );
}

#[test]
fn ts_prefer_union_over_enum() {
    check_table(
        "ts-prefer-union-over-enum",
        "a.ts",
        &[
            "enum Color { Red }",
            "export enum A {",
            "export const enum A {",
            "declare enum A {}",
        ],
        &[
            "const enumValue = 1;",
            "// enum A {",
            "type A = 'a' | 'b';",
            "x.enum = 1;",
        ],
    );
    assert!(!fires(
        "ts-prefer-union-over-enum",
        "a.d.ts",
        "declare enum A {}"
    ));
}

#[test]
fn rust_no_unwrap_expect() {
    check_table(
        "rust-no-unwrap-expect",
        "src/lib.rs",
        &[
            "let x = y.unwrap();",
            "let x = y.expect(\"msg\");",
            "a.b().unwrap()",
        ],
        &[
            "let x = y.unwrap_or(1);",
            "let x = y.unwrap_or_else(|| 1);",
            "// y.unwrap()",
            "let s = \".unwrap()\";",
            "y.expect_err(\"x\");",
        ],
    );
    for path in [
        "src/foo_tests.rs",
        "tests/it.rs",
        "crates/x/benches/b.rs",
        "build.rs",
    ] {
        assert!(
            !fires("rust-no-unwrap-expect", path, "let x = y.unwrap();"),
            "{path}"
        );
    }
}

#[test]
fn rust_test_code_is_skipped() {
    let code = "#[cfg(test)]\nmod tests {\n    fn t() { Some(1).unwrap(); }\n}\nfn later() { Some(1).unwrap(); }\n";
    let set = builtin_set();
    let report = scan_added(
        &set,
        Path::new("src/lib.rs"),
        None,
        code,
        ModeFilter::AfterWrite,
    );
    let lines: Vec<usize> = report.findings.iter().map(|f| f.line).collect();
    assert_eq!(lines, vec![5]);

    assert!(!fires(
        "rust-no-unwrap-expect",
        "src/lib.rs",
        "#[test]\nfn t() {\n    x.unwrap();\n}\n"
    ));
    assert!(!fires(
        "rust-no-unwrap-expect",
        "src/lib.rs",
        "#[cfg(test)]\nmod tests;\n"
    ));
    assert!(fires(
        "rust-no-unwrap-expect",
        "src/lib.rs",
        "#[cfg(test)]\nmod tests;\nfn a() { x.unwrap(); }\n"
    ));
}

#[test]
fn rust_unsafe_needs_safety() {
    check_table(
        "rust-unsafe-needs-safety",
        "src/lib.rs",
        &[
            "let p = unsafe { *ptr };",
            "pub unsafe fn f() {}",
            "unsafe impl Send for X {}",
            "// SAFETY: fine\n\nunsafe { go() }",
            "// SAFETY: fine\nlet a = 1;\nunsafe { go() }",
        ],
        &[
            "// SAFETY: ptr is valid\nlet p = unsafe { *ptr };",
            "// SAFETY: a\n// continued\nunsafe { go() }",
            "// SAFETY: a\n#[inline]\nunsafe fn f() {}",
            "unsafe { go() } // SAFETY: ok",
            "/// # Safety\n/// caller upholds x\npub unsafe fn f() {}",
            "#![forbid(unsafe_code)]",
            "// unsafe { }",
            "let s = \"unsafe {\";",
            "let unsafe_count = 1;",
        ],
    );
}

#[test]
fn rust_no_todo() {
    check_table(
        "rust-no-todo",
        "src/lib.rs",
        &["todo!()", "unimplemented!(\"x\")", "    todo!"],
        &[
            "// todo!()",
            "let s = \"todo!()\";",
            "fn todo() {}",
            "mytodo!()",
        ],
    );
}

#[test]
fn rust_no_dbg() {
    check_table(
        "rust-no-dbg",
        "src/lib.rs",
        &["dbg!(x);", "let y = dbg!(x + 1);"],
        &["// dbg!(x)", "fn dbg() {}", "xdbg!(x);"],
    );
}

#[test]
fn rust_no_println() {
    check_table(
        "rust-no-println",
        "src/lib.rs",
        &["println!(\"hi\");", "eprintln!(\"{e}\");", "print!(\"x\");"],
        &[
            "// println!(\"x\")",
            "let s = \"println!(\";",
            "writeln!(out, \"x\")?;",
            "format!(\"x\")",
        ],
    );
    for path in [
        "src/main.rs",
        "src/bin/tool.rs",
        "build.rs",
        "examples/a.rs",
    ] {
        assert!(
            !fires("rust-no-println", path, "println!(\"hi\");"),
            "{path}"
        );
    }
    assert!(!fires(
        "rust-no-println",
        "src/lib.rs",
        "#[cfg(test)]\nmod t {\n fn a() { println!(\"x\"); }\n}\n"
    ));
}

#[test]
fn rust_no_lossy_cast() {
    check_table(
        "rust-no-lossy-cast",
        "src/lib.rs",
        &[
            "let a = x as u8;",
            "let a = x as i32;",
            "let a = n as f32;",
            "let a = n as u16;",
        ],
        &[
            "let a = x as u64;",
            "let a = x as usize;",
            "let a = x as f64;",
            "let a = x as i64;",
            "let a = x as u128;",
            "use foo as u8x;",
            "// x as u8",
        ],
    );
}

#[test]
fn rust_no_box_leak() {
    check_table(
        "rust-no-box-leak",
        "src/lib.rs",
        &[
            "Box::leak(b)",
            "let s: &'static str = Box::leak(s.into_boxed_str());",
        ],
        &["// Box::leak(x)", "Box::new(x)", "let s = \"Box::leak(\";"],
    );
}

#[test]
fn rust_allow_needs_reason() {
    check_table(
        "rust-allow-needs-reason",
        "src/lib.rs",
        &[
            "#[allow(clippy::too_many_arguments)]",
            "#![allow(clippy::module_inception)]",
            "#[allow(clippy::a, clippy::b)]",
        ],
        &[
            "#[allow(clippy::too_many_arguments, reason = \"builder\")]",
            "#[allow(clippy::x)] // why: generated code",
            "#[allow(dead_code)]",
            "#[warn(clippy::x)]",
            "// #[allow(clippy::x)]",
        ],
    );
}

#[test]
fn rust_no_transmute() {
    check_table(
        "rust-no-transmute",
        "src/lib.rs",
        &[
            "let x: u32 = std::mem::transmute(y);",
            "mem::transmute::<u32, f32>(y)",
            "transmute(y)",
        ],
        &[
            "// transmute(x)",
            "transmute_copy(&x)",
            "fn transmute_ok() {}",
            "let s = \"transmute(\";",
        ],
    );
}
