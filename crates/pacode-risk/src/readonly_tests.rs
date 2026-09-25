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
    assert!(is_read_only("printenv PATH | sort"));
    assert!(is_read_only("env FOO=bar ls -la"));
    assert!(is_read_only("timeout 5 cat file.txt"));
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

// ---------------------------------------------------------------------------
// 3. Allowlisted commands that write or execute through their own flags
// ---------------------------------------------------------------------------

#[test]
fn fd_exec_flags_run_a_command_per_match() {
    assert!(!is_read_only("fd -x rm"));
    assert!(!is_read_only("fd -X wc -l"));
    assert!(!is_read_only("fd --exec touch {}"));
    assert!(!is_read_only("fd --exec-batch rm"));
    // Searching for those names is still a read.
    assert!(is_read_only("fd --extension rs"));
    assert!(is_read_only("fd pattern src/"));
}

#[test]
fn rg_pre_runs_a_command_on_every_file() {
    assert!(!is_read_only("rg --pre 'rm' TODO"));
    assert!(!is_read_only("rg --pre=cat TODO"));
    assert!(is_read_only("rg 'fn main' crates/"));
    assert!(is_read_only("rg --glob '*.rs' TODO"));
}

#[test]
fn sort_output_flag_rewrites_the_input() {
    assert!(!is_read_only("sort -o Cargo.toml Cargo.toml"));
    assert!(!is_read_only("sort --output=out.txt list.txt"));
    assert!(!is_read_only("sort --output out.txt list.txt"));
    assert!(is_read_only("sort list.txt"));
    // `-o` on other allowlisted commands is a different flag entirely.
    assert!(is_read_only("grep -o 'a.' file.txt"));
}

#[test]
fn find_file_writing_actions_are_disallowed() {
    assert!(!is_read_only("find . -name '*.rs' -fprintf out.txt '%p'"));
    assert!(!is_read_only("find . -fprint out.txt"));
    assert!(!is_read_only("find . -fprint0 out.txt"));
    assert!(!is_read_only("find . -fls out.txt"));
    // `-printf` writes to stdout only.
    assert!(is_read_only("find . -name '*.rs' -printf '%p\\n'"));
}

#[test]
fn sed_write_command_creates_a_file() {
    assert!(!is_read_only("sed 'w out.txt' file.txt"));
    assert!(!is_read_only("sed -n 's/foo/bar/w out.txt' file.txt"));
    assert!(!is_read_only("sed 's/a/b/;w out.txt' file.txt"));
    assert!(is_read_only("sed 's/foo/bar/g' file.txt"));
    // A `..` in a substitution is a regex, not a path.
    assert!(is_read_only("sed 's/../x/g' file.txt"));
}

#[test]
fn awk_print_redirection_writes_a_file() {
    assert!(!is_read_only("awk '{print > \"out.txt\"}' table.txt"));
    assert!(!is_read_only("awk '{print $1 >> \"out.txt\"}' table.txt"));
    assert!(!is_read_only("awk 'BEGIN{printf(\"x\") > \"out.txt\"}'"));
    // Comparisons and plain output still read.
    assert!(is_read_only("awk '$3 > 100 {print $1}' table.txt"));
    assert!(is_read_only("awk '{print $1}' table.txt"));
}

#[test]
fn git_output_flag_writes_the_result() {
    assert!(!is_read_only("git diff --output=changes.patch"));
    assert!(!is_read_only("git diff --output changes.patch"));
    assert!(!is_read_only("git log --output=log.txt"));
}

#[test]
fn git_ref_writing_subcommands_are_disallowed() {
    assert!(!is_read_only("git tag v1.0"));
    assert!(!is_read_only("git tag -f v1.0"));
    assert!(!is_read_only("git tag -s -u key v1.0"));
    assert!(!is_read_only("git branch --move old new"));
    assert!(!is_read_only("git branch --copy main release"));
    assert!(!is_read_only("git branch --edit-description"));
    assert!(!is_read_only("git branch --set-upstream-to=origin/main"));
    assert!(!is_read_only("git remote update"));
    assert!(!is_read_only("git remote prune origin"));
    assert!(!is_read_only("git remote set-head origin -a"));
    // Listing forms stay readable.
    assert!(is_read_only("git tag"));
    assert!(is_read_only("git tag -l 'v*'"));
    assert!(is_read_only("git branch -a"));
    assert!(is_read_only("git remote -v"));
}

#[test]
fn whole_environment_dumps_are_disallowed() {
    assert!(!is_read_only("env"));
    assert!(!is_read_only("printenv"));
    assert!(!is_read_only("env FOO=bar"));
    assert!(!is_read_only("printenv | grep TOKEN"));
    // Naming the variable keeps it a read.
    assert!(is_read_only("printenv HOME"));
    assert!(is_read_only("printenv PATH | sort"));
}

// ---------------------------------------------------------------------------
// 4. Read targets: credential stores and workspace escapes
// ---------------------------------------------------------------------------

#[test]
fn credential_stores_and_files_are_not_read_only() {
    let commands = [
        "cat ~/.ssh/id_rsa",
        "cat /home/dev/.aws/credentials",
        "head -n 5 ./.ssh/config",
        "grep -rn token .gnupg",
        "cat .kube/config",
        "cat ~/.docker/config.json",
        "cat .env",
        "cat config/.env.production",
        "cat deploy/server.key",
        "cat certs/api.pem",
        "cat id_ed25519",
        "cat aws-credentials.json",
        "cat /etc/shadow",
        "cat /etc/gshadow",
        // Every segment is checked, not just the first one.
        "cat Cargo.toml; cat ~/.ssh/id_rsa",
        "ls | head ~/.aws/credentials",
    ];
    for cmd in commands {
        assert!(!is_read_only(cmd), "{cmd:?} must not be a Plan-mode read");
    }
    // Ordinary files next to them stay readable. (`ProtectedPaths` treats
    // `.env.example` as a credential name, so it is covered by the rules above.)
    assert!(is_read_only("cat src/config.rs"));
    assert!(is_read_only("cut -d: -f1 /etc/passwd"));
}

#[test]
fn operands_climbing_out_of_the_workspace_are_disallowed() {
    let commands = [
        "cat ../secrets.txt",
        "ls ..",
        "head -n 20 ../../other-project/.env",
        "cat src/../../outside.txt",
        "grep -rn TODO ..",
        "find .. -name '*.rs'",
        "diff ../a.rs b.rs",
    ];
    for cmd in commands {
        assert!(!is_read_only(cmd), "{cmd:?} escapes the workspace");
    }
    // `..` as regex text or a leading-dot path is not an escape.
    assert!(is_read_only("grep -rn 'a\\.\\.b' src/"));
    assert!(is_read_only("ls .config"));
    assert!(is_read_only("cat ./file.txt"));
}

#[test]
fn escapes_are_seen_through_wrappers() {
    assert!(!is_read_only("timeout 5 cat ~/.ssh/id_rsa"));
    assert!(!is_read_only(
        "find . -name '*.txt' | xargs head -n 1 ../secret"
    ));
}
