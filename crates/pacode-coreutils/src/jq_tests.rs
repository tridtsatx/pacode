use crate::test_util::{ok, run_in, sh};

fn jq(args: &[&str], input: &str) -> String {
    ok("jq", args, input)
}

#[test]
fn paths_and_iteration() {
    assert_eq!(jq(&[".a"], "{\"a\":1}"), "1\n");
    assert_eq!(jq(&[".a.b"], "{\"a\":{\"b\":\"x\"}}"), "\"x\"\n");
    assert_eq!(jq(&[".[]"], "[1,2]"), "1\n2\n");
    assert_eq!(jq(&[".[1]"], "[1,2,3]"), "2\n");
    assert_eq!(jq(&[".[-1]"], "[1,2,3]"), "3\n");
    assert_eq!(jq(&["-c", ".[0:2]"], "[1,2,3]"), "[1,2]\n");
    assert_eq!(jq(&[".zzz"], "{}"), "null\n");
    assert_eq!(jq(&[".[]?"], "1"), "");
    assert_eq!(jq(&["-c", "[..]|length"], "{\"a\":[1]}"), "3\n");
}

#[test]
fn output_modes() {
    assert_eq!(jq(&["-r", ".name"], "{\"name\":\"x y\"}"), "x y\n");
    assert_eq!(jq(&[".name"], "{\"name\":\"x y\"}"), "\"x y\"\n");
    assert_eq!(jq(&["-c", "."], "{\"a\": [1, 2]}"), "{\"a\":[1,2]}\n");
    assert_eq!(jq(&["."], "{\"a\":1}"), "{\n  \"a\": 1\n}\n");
    assert_eq!(jq(&["-j", ".[]"], "[\"a\",\"b\"]"), "ab");
    assert_eq!(jq(&["-r", "."], "\"é\""), "é\n");
}

#[test]
fn select_map_length_keys() {
    assert_eq!(jq(&["-c", "map(select(. > 1))"], "[1,2,3]"), "[2,3]\n");
    assert_eq!(
        jq(
            &[".[] | select(.a == 1) | .b"],
            "[{\"a\":1,\"b\":7},{\"a\":2,\"b\":8}]"
        ),
        "7\n"
    );
    assert_eq!(jq(&["length"], "[1,2,3]"), "3\n");
    assert_eq!(jq(&["length"], "\"héllo\""), "5\n");
    assert_eq!(jq(&["length"], "null"), "0\n");
    assert_eq!(jq(&["-c", "keys"], "{\"b\":1,\"a\":2}"), "[\"a\",\"b\"]\n");
    assert_eq!(jq(&["-c", "map(.x)"], "[{\"x\":1},{\"x\":2}]"), "[1,2]\n");
}

#[test]
fn pipes_commas_and_arithmetic() {
    assert_eq!(jq(&[".a, .b"], "{\"a\":1,\"b\":2}"), "1\n2\n");
    assert_eq!(jq(&[".a + .b"], "{\"a\":1,\"b\":2}"), "3\n");
    assert_eq!(jq(&[".a * 2"], "{\"a\":3}"), "6\n");
    assert_eq!(jq(&["-n", "1 + 2 * 3"], ""), "7\n");
    assert_eq!(jq(&["-n", "10 / 4"], ""), "2.5\n");
    assert_eq!(jq(&["-n", "7 % 3"], ""), "1\n");
    assert_eq!(jq(&["-n", "[1,2,3] | add"], ""), "6\n");
    assert_eq!(jq(&[".a // \"d\""], "{}"), "\"d\"\n");
    assert_eq!(jq(&["-c", "[.[] | . * 2]"], "[1,2]"), "[2,4]\n");
    assert_eq!(jq(&[". + 1"], "1\n2\n"), "2\n3\n");
}

#[test]
fn construction_and_conditionals() {
    assert_eq!(
        jq(&["-c", "{a: .x, b: (.y + 1)}"], "{\"x\":1,\"y\":2}"),
        "{\"a\":1,\"b\":3}\n"
    );
    assert_eq!(jq(&["-c", "{x}"], "{\"x\":1,\"y\":2}"), "{\"x\":1}\n");
    assert_eq!(
        jq(&["if . > 1 then \"big\" else \"small\" end"], "2"),
        "\"big\"\n"
    );
    assert_eq!(
        jq(
            &[
                "-r",
                "if . == 1 then \"one\" elif . == 2 then \"two\" else \"many\" end"
            ],
            "2"
        ),
        "two\n"
    );
    assert_eq!(jq(&["-c", ". and true"], "false"), "false\n");
}

#[test]
fn builtins() {
    assert_eq!(jq(&["-r", "join(\"-\")"], "[\"a\",\"b\"]"), "a-b\n");
    assert_eq!(jq(&["-c", "split(\",\")"], "\"a,b\""), "[\"a\",\"b\"]\n");
    assert_eq!(jq(&["test(\"^a\")"], "\"abc\""), "true\n");
    assert_eq!(
        jq(&["-c", "sort_by(.a)"], "[{\"a\":2},{\"a\":1}]"),
        "[{\"a\":1},{\"a\":2}]\n"
    );
    assert_eq!(jq(&["-c", "unique"], "[2,1,2]"), "[1,2]\n");
    assert_eq!(
        jq(
            &["-c", "group_by(.k) | map(length)"],
            "[{\"k\":1},{\"k\":2},{\"k\":1}]"
        ),
        "[2,1]\n"
    );
    assert_eq!(jq(&["has(\"a\")"], "{\"a\":1}"), "true\n");
    assert_eq!(
        jq(&["-c", "to_entries"], "{\"a\":1}"),
        "[{\"key\":\"a\",\"value\":1}]\n"
    );
    assert_eq!(jq(&["-c", "[range(3)]"], "null"), "[0,1,2]\n");
    assert_eq!(jq(&["first, last"], "[1,2,3]"), "1\n3\n");
    assert_eq!(jq(&["-c", "map(tonumber)"], "[\"1\",\"2.5\"]"), "[1,2.5]\n");
    assert_eq!(jq(&["ascii_downcase"], "\"AbC\""), "\"abc\"\n");
    assert_eq!(jq(&["type"], "[]"), "\"array\"\n");
    assert_eq!(jq(&["contains(\"ell\")"], "\"hello\""), "true\n");
    assert_eq!(
        jq(&["-r", ".[] | [.a, .b] | @csv"], "[{\"a\":1,\"b\":\"x\"}]"),
        "1,\"x\"\n"
    );
}

#[test]
fn flags_slurp_null_input_and_args() {
    assert_eq!(jq(&["-c", "-s", "."], "1 2 3"), "[1,2,3]\n");
    assert_eq!(sh("jq", &["-n", "1"], None), ("1\n".to_string(), 0));
    assert_eq!(jq(&["-n", "--arg", "x", "5", "$x"], ""), "\"5\"\n");
    assert_eq!(
        jq(&["-n", "--argjson", "x", "{\"a\":1}", "-c", "$x"], ""),
        "{\"a\":1}\n"
    );
    assert_eq!(sh("jq", &["-e", "."], Some("false")).1, 1);
    assert_eq!(sh("jq", &["-e", "."], Some("1")).1, 0);
}

#[test]
fn files_and_errors() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("d.json"), "{\"a\": 5}").expect("write");
    let out = run_in(dir.path(), "jq", &[".a", "d.json"], None);
    assert_eq!(out.stdout_lossy(), "5\n");

    let out = run_in(dir.path(), "jq", &[".a"], Some("1"));
    assert_eq!(out.code, 5);
    assert!(out.stderr.contains("Cannot index number"), "{}", out.stderr);

    assert_eq!(sh("jq", &[".a"], Some("{oops")).1, 1);
    assert_eq!(sh("jq", &[".a |"], Some("{}")).1, 2);
    assert_eq!(sh("jq", &["def f: .; f"], Some("1")).1, 2);
    assert_eq!(sh("jq", &[".a = 1"], Some("{}")).1, 2);
    assert_eq!(sh("jq", &[], Some("{}")).1, 2);
    assert_eq!(sh("jq", &["nosuchfn"], Some("1")).1, 5);
    assert_eq!(jq(&["."], ""), "");
}
