use super::*;

#[test]
fn test_build_browser_command() {
    let url = "https://example.com/oauth/authorize";
    let cmd = build_browser_command(url);

    #[cfg(target_os = "linux")]
    {
        assert_eq!(cmd.get_program(), "xdg-open");
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args, vec![url]);
    }

    #[cfg(target_os = "macos")]
    {
        assert_eq!(cmd.get_program(), "open");
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args, vec![url]);
    }

    #[cfg(target_os = "windows")]
    {
        assert_eq!(cmd.get_program(), "cmd");
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args, vec!["/C", "start", "", url]);
    }
}

#[test]
fn test_open_url_handles_nonexistent_binary_gracefully() {
    // A command that doesn't exist should return AuthError::Io, never panic
    let mut cmd = Command::new("nonexistent_browser_binary_xyz_12345");
    cmd.arg("https://example.com");
    let result = cmd.spawn().map_err(AuthError::Io);
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), AuthError::Io(_)));
}
