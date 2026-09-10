use super::*;

#[test]
fn test_error_display() {
    let io_err = AuthError::Io(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "not found",
    ));
    assert_eq!(format!("{io_err}"), "I/O error: not found");

    let denied = AuthError::Denied("user cancelled".to_string());
    assert_eq!(format!("{denied}"), "access denied: user cancelled");

    let callback = AuthError::Callback("timeout reached".to_string());
    assert_eq!(format!("{callback}"), "callback error: timeout reached");

    let store = AuthError::Store("bad json".to_string());
    assert_eq!(format!("{store}"), "store error: bad json");

    let refresh = AuthError::Refresh("invalid_grant".to_string());
    assert_eq!(format!("{refresh}"), "refresh error: invalid_grant");

    let unknown = AuthError::UnknownProvider("foo".to_string());
    assert_eq!(format!("{unknown}"), "unknown provider: foo");
}

#[test]
fn test_from_json_error() {
    let res: std::result::Result<serde_json::Value, serde_json::Error> =
        serde_json::from_str("{invalid");
    let json_err = res.unwrap_err();
    let auth_err = AuthError::from(json_err);
    assert!(matches!(auth_err, AuthError::Json(_)));
}
