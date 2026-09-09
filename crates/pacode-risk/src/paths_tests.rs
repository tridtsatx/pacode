use super::*;

fn home() -> PathBuf {
    PathBuf::from("/home/tester")
}

fn cwd() -> PathBuf {
    PathBuf::from("/home/tester/proj")
}

#[test]
fn home_directory_is_catastrophic_in_every_spelling() {
    let h = home();
    let c = cwd();
    let spellings = [
        "~",
        "$HOME",
        "${HOME}",
        "/home/tester",
        "/home/tester/",
        "/home/tester/.",
        "/home/tester/foo/..",
        "~/",
        "~/.",
        "/home/tester/../tester",
        "/home/./tester",
    ];
    for spelling in spellings {
        let expanded = expand(spelling, &c, &h);
        assert!(
            is_catastrophic_target(&expanded, &h),
            "{spelling:?} resolved to {expanded:?} and was not caught"
        );
    }
}

#[test]
fn parent_traversal_out_of_home_reaches_root_and_is_caught() {
    let h = home();
    let c = cwd();
    for spelling in [
        "~/../..",
        "/home/tester/../..",
        "~/../../",
        "/home/tester/../../",
    ] {
        let expanded = expand(spelling, &c, &h);
        assert_eq!(expanded, PathBuf::from("/"), "{spelling:?}");
        assert!(is_catastrophic_target(&expanded, &h), "{spelling:?}");
    }
}

#[test]
fn root_and_system_paths_are_catastrophic() {
    let h = home();
    for p in [
        "/", "/etc", "/usr", "/var", "/boot", "/home", "/Users", "/bin", "/sbin", "/lib", "/root",
        "/opt",
    ] {
        assert!(
            is_catastrophic_target(Path::new(p), &h),
            "{p} must be protected"
        );
    }
}

#[test]
fn credential_stores_inside_home_are_catastrophic() {
    let h = home();
    let c = cwd();
    for sub in [".ssh", ".gnupg", ".aws", ".config", ".local"] {
        let expanded = expand(&format!("~/{sub}"), &c, &h);
        assert!(is_catastrophic_target(&expanded, &h), "~/{sub}");
    }
}

#[test]
fn ordinary_project_paths_are_not_catastrophic() {
    let h = home();
    for p in [
        "/home/tester/proj/target",
        "/home/tester/proj/node_modules",
        "/home/tester/proj/src/old.rs",
        "/tmp/scratch",
        "/home/tester/scratchpad",
    ] {
        assert!(
            !is_catastrophic_target(Path::new(p), &h),
            "{p} should not be treated as catastrophic"
        );
    }
}

#[test]
fn home_subdirectory_is_not_itself_catastrophic() {
    let h = home();
    assert!(!is_catastrophic_target(Path::new("/home/tester/proj"), &h));
    assert!(is_catastrophic_target(Path::new("/home/tester"), &h));
}

#[test]
fn inside_working_directory_is_low_risk() {
    let h = home();
    let c = cwd();
    let finding = classify_target("target", true, &c, &h, false).expect("finding");
    assert_eq!(finding.level, RiskLevel::Low);
}

#[test]
fn non_recursive_delete_inside_cwd_is_low_risk() {
    let h = home();
    let c = cwd();
    let finding = classify_target("notes.txt", false, &c, &h, false).expect("finding");
    assert_eq!(finding.level, RiskLevel::Low);
}

#[test]
fn concrete_outside_working_directory_target_is_low_risk() {
    let h = home();
    let c = cwd();
    let finding =
        classify_target("/home/tester/other-project", true, &c, &h, false).expect("finding");
    assert_eq!(finding.level, RiskLevel::Low);
}

#[test]
fn temp_paths_are_low_risk_when_destructive() {
    let h = home();
    let c = cwd();
    let finding = classify_target("/tmp/build-cache", true, &c, &h, false).expect("finding");
    assert_eq!(finding.level, RiskLevel::Low);

    // Redirect to temp path is safe
    assert_eq!(
        classify_target("/tmp/build-cache", false, &c, &h, true),
        None
    );
}

#[test]
fn glob_over_a_protected_directory_is_catastrophic() {
    let h = home();
    let c = cwd();
    for raw in ["~/*", "/*", "$HOME/*"] {
        let finding = classify_target(raw, true, &c, &h, false)
            .unwrap_or_else(|| panic!("{raw} produced no finding"));
        assert_eq!(finding.level, RiskLevel::Catastrophic, "{raw}");
    }
}

#[test]
fn unresolvable_targets_escalate_rather_than_pass() {
    let h = home();
    let c = cwd();
    for raw in ["$SOME_VAR", "`cat paths.txt`", "$(echo /)"] {
        let finding = classify_target(raw, true, &c, &h, false)
            .unwrap_or_else(|| panic!("{raw} produced no finding"));
        assert!(
            finding.level >= RiskLevel::Confirm,
            "{raw} must not be silently allowed"
        );
    }
}

#[test]
fn missing_home_context_does_not_panic_or_misfire() {
    let empty_home = Path::new("");
    assert!(is_catastrophic_target(Path::new("/"), empty_home));
    assert!(!is_catastrophic_target(
        Path::new("/srv/app/tmp"),
        empty_home
    ));
}

#[test]
fn normalize_never_escapes_above_root() {
    let h = home();
    let c = cwd();
    let expanded = expand("/../../../../..", &c, &h);
    assert_eq!(expanded, PathBuf::from("/"));
    assert!(is_catastrophic_target(&expanded, &h));
}

#[test]
fn individual_credential_files_are_protected() {
    let h = home();
    for path in [
        "/home/tester/.ssh/id_ed25519",
        "/home/tester/.ssh/id_rsa",
        "/home/tester/.gnupg/secring.gpg",
        "/home/tester/.aws/credentials",
        "/home/tester/proj/.env",
        "/home/tester/proj/server.pem",
        "/home/tester/proj/credentials.json",
    ] {
        assert!(
            is_catastrophic_target(Path::new(path), &h),
            "{path} must be protected"
        );
    }
}

#[test]
fn ordinary_config_files_remain_editable() {
    let h = home();
    for path in [
        "/home/tester/.config/app/settings.toml",
        "/home/tester/.pacode/sessions/old.json",
        "/home/tester/Documents/notes/draft.md",
    ] {
        assert!(
            !is_catastrophic_target(Path::new(path), &h),
            "{path} should stay editable"
        );
    }
    for path in [
        "/home/tester/.config",
        "/home/tester/.local",
        "/home/tester/Documents",
    ] {
        assert!(is_catastrophic_target(Path::new(path), &h), "{path}");
    }
}
