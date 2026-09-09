use super::*;

// ---------------------------------------------------------------------------
// 1. Positive read-only cases
// ---------------------------------------------------------------------------

#[test]
fn basic_allowlist_commands_are_read_only() {
    let commands = [
        "ls -la",
        "cat file.txt",
        "head -n 20 src/main.rs",
        "tail -f log.txt",
        "less README.md",
        "more README.md",
        "wc -l Cargo.toml",
        "grep -rn 'TODO' src/",
        "egrep 'foo|bar' file.txt",
        "fgrep 'literal' file.txt",
        "rg 'fn classify' crates/",
        "fd --extension rs",
        "tree -L 2",
        "pwd",
        "echo 'hello world'",
        "printf '%s\\n' value",
        "which cargo",
        "type ls",
        "env",
        "printenv HOME",
        "stat Cargo.toml",
        "file src/main.rs",
        "du -sh target",
        "df -h /",
        "date",
        "uname -a",
        "whoami",
        "id",
        "ps aux",
        "jq '.name' package.json",
        "sort list.txt",
        "uniq sorted.txt",
        "cut -d: -f1 /etc/passwd",
        "diff old.rs new.rs",
        "md5sum Cargo.lock",
        "sha256sum Cargo.lock",
        "true",
        "false",
        "test -f Cargo.toml",
        "[ -f Cargo.toml ]",
        "dirname /path/to/file",
        "basename /path/to/file",
        "realpath .",
        "readlink symlink",
    ];
    for cmd in commands {
        assert!(is_read_only(cmd), "{cmd:?} should be read-only");
    }
}

#[test]
fn find_without_delete_or_exec_is_read_only() {
    assert!(is_read_only("find . -name '*.rs'"));
    assert!(is_read_only("find /tmp -type f -maxdepth 2"));
    assert!(is_read_only("find . -name '*.log' 2>/dev/null"));
}

#[test]
fn sed_and_awk_without_side_effects_are_read_only() {
    assert!(is_read_only("sed 's/foo/bar/g' file.txt"));
    assert!(is_read_only("sed -e 's/a/b/' -e 's/c/d/' file.txt"));
    assert!(is_read_only("awk '{print $1}' table.txt"));
    assert!(is_read_only("awk -F: '{print $1, $3}' /etc/passwd"));
}

#[test]
fn git_read_only_subcommands() {
    let commands = [
        "git status",
        "git log -n 10 --oneline",
        "git diff HEAD~1",
        "git show HEAD:Cargo.toml",
        "git branch",
        "git branch -a",
        "git blame src/lib.rs",
        "git rev-parse HEAD",
        "git ls-files",
        "git describe --tags",
        "git stash list",
        "git tag",
        "git tag -l 'v*'",
        "git remote",
        "git remote -v",
        "git --version",
    ];
    for cmd in commands {
        assert!(is_read_only(cmd), "{cmd:?} should be read-only");
    }
}

#[test]
fn cargo_read_only_subcommands() {
    assert!(is_read_only("cargo check"));
    assert!(is_read_only("cargo check --workspace"));
    assert!(is_read_only("cargo metadata --format-version 1"));
    assert!(is_read_only("cargo tree"));
    assert!(is_read_only("cargo --version"));
}

#[test]
fn version_flag_invocations_are_read_only() {
    assert!(is_read_only("rustc --version"));
    assert!(is_read_only("python3 --version"));
    assert!(is_read_only("python3 -V"));
    assert!(is_read_only("node --version"));
    assert!(is_read_only("node -v"));
}

#[test]
fn read_only_pipelines_are_read_only() {
    assert!(is_read_only("cat file.txt | grep pattern | wc -l"));
    assert!(is_read_only("find . -type f | xargs grep TODO"));
    assert!(is_read_only("ls -la | sort | uniq"));
    assert!(is_read_only("env | sort"));
}

// ---------------------------------------------------------------------------
// 2. Negative read-only cases
// ---------------------------------------------------------------------------

#[test]
fn file_redirections_are_disallowed() {
    assert!(!is_read_only("ls > out.txt"));
    assert!(!is_read_only("echo 'data' >> log.txt"));
    assert!(!is_read_only("cat foo > bar"));
    assert!(!is_read_only("cargo check > check.log"));
}

#[test]
fn tee_is_disallowed() {
    assert!(!is_read_only("cat file.txt | tee output.txt"));
    assert!(!is_read_only("ls | tee -a log.txt"));
}

#[test]
fn sudo_is_disallowed() {
    assert!(!is_read_only("sudo ls"));
    assert!(!is_read_only("sudo cat /etc/shadow"));
    assert!(!is_read_only("doas ls"));
}

#[test]
fn command_substitutions_are_disallowed() {
    assert!(!is_read_only("echo $(pwd)"));
    assert!(!is_read_only("echo `pwd`"));
    assert!(!is_read_only("ls $(cat targets.txt)"));
}

#[test]
fn find_with_delete_or_exec_is_disallowed() {
    assert!(!is_read_only("find . -name '*.o' -delete"));
    assert!(!is_read_only("find . -name '*.tmp' -exec rm {} +"));
    assert!(!is_read_only("find . -name '*.bak' -execdir rm {} \\;"));
    assert!(!is_read_only("find . -ok rm {} +"));
}

#[test]
fn sed_in_place_is_disallowed() {
    assert!(!is_read_only("sed -i 's/foo/bar/g' file.txt"));
    assert!(!is_read_only("sed -i.bak 's/foo/bar/g' file.txt"));
    assert!(!is_read_only("sed --in-place 's/foo/bar/g' file.txt"));
}

#[test]
fn awk_with_system_is_disallowed() {
    assert!(!is_read_only("awk '{system(\"rm file\")}' file.txt"));
    assert!(!is_read_only("awk '{ system (\"rm file\") }' file.txt"));
}

#[test]
fn non_read_only_git_subcommands_are_disallowed() {
    assert!(!is_read_only("git push"));
    assert!(!is_read_only("git commit -m 'commit'"));
    assert!(!is_read_only("git checkout main"));
    assert!(!is_read_only("git reset --hard"));
    assert!(!is_read_only("git clean -fdx"));
    assert!(!is_read_only("git stash drop"));
    assert!(!is_read_only("git branch -D feat"));
    assert!(!is_read_only("git branch -d old"));
    assert!(!is_read_only("git remote add origin https://..."));
    assert!(!is_read_only("git tag -d v1.0"));
}

#[test]
fn non_read_only_cargo_subcommands_are_disallowed() {
    assert!(!is_read_only("cargo build"));
    assert!(!is_read_only("cargo test"));
    assert!(!is_read_only("cargo run"));
    assert!(!is_read_only("cargo clean"));
    assert!(!is_read_only("cargo install ripgrep"));
}

#[test]
fn general_script_runtimes_without_version_are_disallowed() {
    assert!(!is_read_only("python3 script.py"));
    assert!(!is_read_only("python3 -c 'print(1)'"));
    assert!(!is_read_only("node index.js"));
    assert!(!is_read_only("node -e 'console.log(1)'"));
    assert!(!is_read_only("rustc main.rs"));
}

#[test]
fn xargs_with_non_read_only_command_is_disallowed() {
    assert!(!is_read_only("find . -name '*.o' | xargs rm"));
    assert!(!is_read_only("cat list.txt | xargs rm -rf"));
    assert!(!is_read_only("find . | xargs -n 1 sh"));
}

#[test]
fn destructive_commands_are_disallowed() {
    assert!(!is_read_only("rm file.txt"));
    assert!(!is_read_only("rmdir empty_dir"));
    assert!(!is_read_only("shred secrets.txt"));
    assert!(!is_read_only("truncate -s 0 file.txt"));
    assert!(!is_read_only("dd if=/dev/zero of=disk.img"));
}

#[test]
fn empty_command_is_disallowed() {
    assert!(!is_read_only(""));
    assert!(!is_read_only("   "));
}
