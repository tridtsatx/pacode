use super::*;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn send_raw_request(port: u16, target: &str) -> String {
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("tcp connect");
    let req =
        format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).await.expect("tcp write");
    stream.flush().await.expect("tcp flush");

    let mut resp = Vec::new();
    stream.read_to_end(&mut resp).await.expect("tcp read");
    String::from_utf8_lossy(&resp).to_string()
}

#[tokio::test]
async fn test_callback_success_case() {
    let listener = bind_callback(None).expect("bind ephemeral callback");
    let port = listener.port();
    assert!(port > 0);
    assert_eq!(
        listener.redirect_uri("/oauth/callback"),
        format!("http://127.0.0.1:{port}/oauth/callback")
    );

    let client_handle = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        send_raw_request(
            port,
            "/oauth/callback?code=auth_test_code_123&state=expected_state_abc",
        )
        .await
    });

    let params = listener
        .wait(Duration::from_secs(5), "expected_state_abc")
        .await
        .expect("wait for callback");

    assert_eq!(params.code.as_deref(), Some("auth_test_code_123"));
    assert_eq!(params.state.as_deref(), Some("expected_state_abc"));
    assert_eq!(params.error, None);
    assert!(params.is_success());

    let http_resp = client_handle.await.expect("client task");
    assert!(http_resp.starts_with("HTTP/1.1 200 OK"));
    assert!(http_resp.contains("Authorization Successful"));
}

#[tokio::test]
async fn test_callback_wrong_state_case() {
    let listener = bind_callback(None).expect("bind ephemeral callback");
    let port = listener.port();

    let client_handle = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        send_raw_request(
            port,
            "/oauth/callback?code=some_code&state=attacker_manipulated_state",
        )
        .await
    });

    let result = listener
        .wait(Duration::from_secs(5), "legitimate_state")
        .await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err, AuthError::Callback(_)));
    assert!(format!("{err}").contains("mismatch"));

    let http_resp = client_handle.await.expect("client task");
    assert!(http_resp.starts_with("HTTP/1.1 400 Bad Request"));
    assert!(http_resp.contains("Authorization Failed"));
}

#[tokio::test]
async fn test_callback_error_access_denied_case() {
    let listener = bind_callback(None).expect("bind ephemeral callback");
    let port = listener.port();

    let client_handle = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        send_raw_request(
            port,
            "/oauth/callback?error=access_denied&state=my_csrf_state",
        )
        .await
    });

    let params = listener
        .wait(Duration::from_secs(5), "my_csrf_state")
        .await
        .expect("callback wait should succeed in parsing error param");

    assert_eq!(params.error.as_deref(), Some("access_denied"));
    assert_eq!(params.code, None);
    assert_eq!(params.state.as_deref(), Some("my_csrf_state"));
    assert!(!params.is_success());

    // into_result should return AuthError::Denied
    let into_res = params.into_result();
    assert!(matches!(into_res, Err(AuthError::Denied(ref s)) if s == "access_denied"));

    let http_resp = client_handle.await.expect("client task");
    assert!(http_resp.starts_with("HTTP/1.1 200 OK"));
    assert!(http_resp.contains("Authorization Failed"));
    assert!(http_resp.contains("access_denied"));
}

#[tokio::test]
async fn test_callback_timeout_case() {
    let listener = bind_callback(None).expect("bind ephemeral callback");

    let result = listener.wait(Duration::from_millis(60), "any_state").await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err, AuthError::Callback(_)));
    assert!(format!("{err}").contains("timed out"));
}
