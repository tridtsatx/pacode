use super::path_operands;

fn paths(name: &str, args: &[&str]) -> Vec<String> {
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    path_operands(name, &args)
}

#[test]
fn file_operands_are_found() {
    assert_eq!(paths("wc", &["-l", "a.rs", "b.rs"]), vec!["a.rs", "b.rs"]);
    assert_eq!(paths("head", &["-n", "5", "f"]), vec!["f"]);
    assert_eq!(paths("head", &["-5", "f"]), vec!["f"]);
    assert_eq!(paths("cut", &["-d", "/", "-f1", "p"]), vec!["p"]);
    assert_eq!(paths("sort", &["-k", "2,2", "-t", ",", "f"]), vec!["f"]);
    assert_eq!(paths("tree", &["-L", "2", "src"]), vec!["src"]);
    assert_eq!(paths("stat", &["-c", "%s", "f"]), vec!["f"]);
}

#[test]
fn scripts_and_filters_are_not_paths() {
    assert_eq!(paths("sed", &["-n", "/a/p", "f"]), vec!["f"]);
    assert_eq!(paths("sed", &["-e", "s/a/b/", "f"]), vec!["f"]);
    assert_eq!(paths("jq", &["-r", ".a", "f.json"]), vec!["f.json"]);
    assert_eq!(
        paths("jq", &["--arg", "x", "1", ".a"]),
        Vec::<String>::new()
    );
    assert!(paths("tr", &["a-z", "A-Z"]).is_empty());
    assert!(paths("basename", &["/a/b"]).is_empty());
}
