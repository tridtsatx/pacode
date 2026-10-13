use super::*;
use crate::tokenize::tokenize;

fn code(command: &str) -> Vec<String> {
    let words = tokenize(command);
    let name = words[0].basename();
    inline_code(&name, &words)
        .into_iter()
        .map(str::to_string)
        .collect()
}

fn dests(command: &str) -> Vec<String> {
    let words = tokenize(command);
    let name = words[0].basename();
    write_destinations(&name, &words)
        .into_iter()
        .map(str::to_string)
        .collect()
}

#[test]
fn inline_code_is_pulled_from_the_right_flag() {
    assert_eq!(code("python3 -c 'print(1)'"), ["print(1)"]);
    assert_eq!(code("node -e 'x'"), ["x"]);
    assert_eq!(code("node --eval=y"), ["y"]);
    assert_eq!(code("perl -Eprint"), ["print"]);
    assert_eq!(code("deno eval 'Deno.exit()'"), ["Deno.exit()"]);
    assert!(code("python3 app.py").is_empty());
    assert!(code("node server.js").is_empty());
    assert!(code("cargo run").is_empty());
}

#[test]
fn destructive_markers_are_spotted() {
    assert!(
        code("python3 -c 'import os; os.system(\"x\")'")
            .iter()
            .any(|c| code_is_destructive(c))
    );
    assert!(
        !code("python3 -c 'print(1)'")
            .iter()
            .any(|c| code_is_destructive(c))
    );
}

#[test]
fn write_destinations_are_the_real_targets() {
    assert_eq!(dests("cp a b"), ["b"]);
    assert_eq!(dests("cp -r src dst"), ["dst"]);
    assert_eq!(
        dests("install -m600 k ~/.ssh/authorized_keys"),
        ["~/.ssh/authorized_keys"]
    );
    assert_eq!(dests("cp -t /etc a b"), ["/etc"]);
    assert_eq!(dests("rsync -a src/ ~/.ssh/"), ["~/.ssh/"]);
    assert_eq!(
        dests("ln -sf /etc/shadow ./leak"),
        ["/etc/shadow", "./leak"]
    );
    assert_eq!(dests("tee -a ~/.bashrc"), ["~/.bashrc"]);
    assert_eq!(dests("sponge f"), ["f"]);
    assert!(dests("grep x f").is_empty());
}
