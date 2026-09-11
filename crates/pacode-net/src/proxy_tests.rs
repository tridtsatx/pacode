use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct EnvGuard {
    prev_http_proxy: Option<String>,
    prev_http_proxy_lower: Option<String>,
    prev_no_proxy: Option<String>,
    prev_no_proxy_lower: Option<String>,
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            match &self.prev_http_proxy {
                Some(v) => std::env::set_var("HTTP_PROXY", v),
                None => std::env::remove_var("HTTP_PROXY"),
            }
            match &self.prev_http_proxy_lower {
                Some(v) => std::env::set_var("http_proxy", v),
                None => std::env::remove_var("http_proxy"),
            }
            match &self.prev_no_proxy {
                Some(v) => std::env::set_var("NO_PROXY", v),
                None => std::env::remove_var("NO_PROXY"),
            }
            match &self.prev_no_proxy_lower {
                Some(v) => std::env::set_var("no_proxy", v),
                None => std::env::remove_var("no_proxy"),
            }
        }
    }
}

async fn isolate_env() -> (tokio::sync::MutexGuard<'static, ()>, EnvGuard) {
    let lock = ENV_LOCK.lock().await;
    let guard = EnvGuard {
        prev_http_proxy: std::env::var("HTTP_PROXY").ok(),
        prev_http_proxy_lower: std::env::var("http_proxy").ok(),
        prev_no_proxy: std::env::var("NO_PROXY").ok(),
        prev_no_proxy_lower: std::env::var("no_proxy").ok(),
    };
    (lock, guard)
}

#[test]
fn test_parse_accepted_schemes() {
    let http = ProxySetting::parse(Some("http://127.0.0.1:8080")).unwrap();
    match http {
        ProxySetting::Explicit(u) => {
            assert_eq!(u.scheme, "http");
            assert_eq!(u.host, "127.0.0.1");
            assert_eq!(u.port, Some(8080));
            assert_eq!(u.username, None);
            assert_eq!(u.password, None);
        }
        _ => panic!("expected Explicit, got {http:?}"),
    }

    let https = ProxySetting::parse(Some("https://secure.proxy:8443")).unwrap();
    match https {
        ProxySetting::Explicit(u) => {
            assert_eq!(u.scheme, "https");
            assert_eq!(u.host, "secure.proxy");
            assert_eq!(u.port, Some(8443));
        }
        _ => panic!("expected Explicit, got {https:?}"),
    }

    let socks5 = ProxySetting::parse(Some("socks5://proxy.example.com:1080")).unwrap();
    match socks5 {
        ProxySetting::Explicit(u) => {
            assert_eq!(u.scheme, "socks5");
            assert_eq!(u.host, "proxy.example.com");
            assert_eq!(u.port, Some(1080));
        }
        _ => panic!("expected Explicit, got {socks5:?}"),
    }

    let socks5h =
        ProxySetting::parse(Some("socks5h://alice:secret@socks.domain.net:1080")).unwrap();
    match socks5h {
        ProxySetting::Explicit(u) => {
            assert_eq!(u.scheme, "socks5h");
            assert_eq!(u.host, "socks.domain.net");
            assert_eq!(u.port, Some(1080));
            assert_eq!(u.username.as_deref(), Some("alice"));
            assert_eq!(u.password.as_deref(), Some("secret"));
        }
        _ => panic!("expected Explicit, got {socks5h:?}"),
    }
}

#[test]
fn test_parse_credentials_and_ports_preserved() {
    let raw = "socks5h://user:pass@host:1080";
    let setting = ProxySetting::parse(Some(raw)).unwrap();
    match setting {
        ProxySetting::Explicit(u) => {
            assert_eq!(u.scheme, "socks5h");
            assert_eq!(u.host, "host");
            assert_eq!(u.port, Some(1080));
            assert_eq!(u.username.as_deref(), Some("user"));
            assert_eq!(u.password.as_deref(), Some("pass"));
        }
        _ => panic!("expected Explicit"),
    }

    let no_port = ProxySetting::parse(Some("http://proxy.host")).unwrap();
    match no_port {
        ProxySetting::Explicit(u) => {
            assert_eq!(u.scheme, "http");
            assert_eq!(u.host, "proxy.host");
            assert_eq!(u.port, None);
            assert_eq!(u.username, None);
            assert_eq!(u.password, None);
        }
        _ => panic!("expected Explicit"),
    }
}

#[test]
fn test_parse_none_and_absent() {
    assert_eq!(ProxySetting::parse(None).unwrap(), ProxySetting::Inherit);
    assert_eq!(
        ProxySetting::parse(Some("none")).unwrap(),
        ProxySetting::Disabled
    );
    assert_eq!(
        ProxySetting::parse(Some("None")).unwrap(),
        ProxySetting::Disabled
    );
    assert_eq!(
        ProxySetting::parse(Some("NONE")).unwrap(),
        ProxySetting::Disabled
    );
}

#[test]
fn test_parse_rejected_inputs_name_value() {
    // Empty string
    let err = ProxySetting::parse(Some("")).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("''"), "expected empty value in '{msg}'");

    // Whitespace only
    let err = ProxySetting::parse(Some("   ")).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("'   '"), "expected value in '{msg}'");

    // Unknown scheme
    let err = ProxySetting::parse(Some("ftp://myproxy:21")).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("ftp://myproxy:21"),
        "expected value in '{msg}'"
    );
    assert!(msg.contains("ftp"), "expected scheme in '{msg}'");

    // Garbage
    let err = ProxySetting::parse(Some("not-a-valid-proxy-value")).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("not-a-valid-proxy-value"),
        "expected value in '{msg}'"
    );

    // Bare host with no scheme
    let err = ProxySetting::parse(Some("127.0.0.1:8080")).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("127.0.0.1:8080"), "expected value in '{msg}'");

    // Missing host
    let err = ProxySetting::parse(Some("http://")).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("http://"), "expected value in '{msg}'");
}

#[test]
fn test_resolution_precedence() {
    // 1. Provider value wins over global
    let res = ProxySetting::resolve(
        Some("http://prov-proxy:8080"),
        Some("http://global-proxy:9090"),
    )
    .unwrap();
    match res {
        ProxySetting::Explicit(u) => assert_eq!(u.host, "prov-proxy"),
        _ => panic!("expected Explicit with prov-proxy"),
    }

    // 2. Global applies when the provider has none
    let res = ProxySetting::resolve(None, Some("http://global-proxy:9090")).unwrap();
    match res {
        ProxySetting::Explicit(u) => assert_eq!(u.host, "global-proxy"),
        _ => panic!("expected Explicit with global-proxy"),
    }

    // 3. "none" at provider level overrides a global URL
    let res = ProxySetting::resolve(Some("none"), Some("http://global-proxy:9090")).unwrap();
    assert_eq!(res, ProxySetting::Disabled);

    // 4. "none" globally can be overridden by a provider URL
    let res = ProxySetting::resolve(Some("http://prov-proxy:8080"), Some("none")).unwrap();
    match res {
        ProxySetting::Explicit(u) => assert_eq!(u.host, "prov-proxy"),
        _ => panic!("expected Explicit with prov-proxy"),
    }

    // 5. Both absent -> Inherit
    let res = ProxySetting::resolve(None, None).unwrap();
    assert_eq!(res, ProxySetting::Inherit);

    // 6. Global "none", provider absent -> Disabled
    let res = ProxySetting::resolve(None, Some("none")).unwrap();
    assert_eq!(res, ProxySetting::Disabled);
}

#[test]
fn test_client_builder_accepts_every_valid_form() {
    // Inherit
    assert!(client_builder(&ProxySetting::Inherit).is_ok());

    // Disabled
    assert!(client_builder(&ProxySetting::Disabled).is_ok());

    // HTTP
    let http = ProxySetting::parse(Some("http://127.0.0.1:8080")).unwrap();
    assert!(client_builder(&http).is_ok());

    // HTTPS
    let https = ProxySetting::parse(Some("https://127.0.0.1:8443")).unwrap();
    assert!(client_builder(&https).is_ok());

    // SOCKS5
    let socks5 = ProxySetting::parse(Some("socks5://127.0.0.1:1080")).unwrap();
    assert!(client_builder(&socks5).is_ok());

    // SOCKS5H with credentials
    let socks5h = ProxySetting::parse(Some("socks5h://user:pass@host:1080")).unwrap();
    assert!(client_builder(&socks5h).is_ok());
}

#[tokio::test]
async fn test_explicit_proxy_receives_absolute_uri_request() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = listener.local_addr().unwrap();

    let proxy_handle = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await.unwrap();
        let request_str = String::from_utf8_lossy(&buf[..n]).to_string();
        let response = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK";
        let _ = stream.write_all(response.as_bytes()).await;
        request_str
    });

    let proxy_url_str = format!("http://{proxy_addr}");
    let setting = ProxySetting::parse(Some(&proxy_url_str)).unwrap();
    let client = client_builder(&setting).unwrap().build().unwrap();

    let target_url = "http://target.example.test:12345/api/v1/chat";
    let resp = client.get(target_url).send().await;
    assert!(resp.is_ok());

    let received_request = proxy_handle.await.unwrap();
    // HTTP proxy receives the absolute URI in the request line
    assert!(
        received_request.starts_with("GET http://target.example.test:12345/api/v1/chat HTTP/1.1"),
        "expected absolute URI request line, got: {received_request}"
    );
}

#[tokio::test]
async fn test_disabled_proxy_suppresses_env_proxy() {
    let (_lock, _guard) = isolate_env().await;

    let proxy_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();

    let target_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target_addr = target_listener.local_addr().unwrap();

    let proxy_url = format!("http://{proxy_addr}");
    unsafe {
        std::env::set_var("HTTP_PROXY", &proxy_url);
        std::env::set_var("http_proxy", &proxy_url);
    }

    let proxy_contacted = std::sync::Arc::new(AtomicBool::new(false));
    let proxy_contacted_clone = proxy_contacted.clone();
    tokio::spawn(async move {
        if let Ok((mut stream, _)) = proxy_listener.accept().await {
            proxy_contacted_clone.store(true, Ordering::SeqCst);
            let resp = "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nPROXY";
            let _ = stream.write_all(resp.as_bytes()).await;
        }
    });

    let target_handle = tokio::spawn(async move {
        let (mut stream, _) = target_listener.accept().await.unwrap();
        let mut buf = vec![0u8; 1024];
        let n = stream.read(&mut buf).await.unwrap();
        let request_str = String::from_utf8_lossy(&buf[..n]).to_string();
        let response = "HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nDIRECT";
        let _ = stream.write_all(response.as_bytes()).await;
        request_str
    });

    let client = client_builder(&ProxySetting::Disabled)
        .unwrap()
        .build()
        .unwrap();

    let target_url = format!("http://{target_addr}/direct-check");
    let resp = client.get(&target_url).send().await;
    assert!(resp.is_ok());
    let body = resp.unwrap().text().await.unwrap();
    assert_eq!(body, "DIRECT");

    let target_req = target_handle.await.unwrap();
    assert!(target_req.starts_with("GET /direct-check HTTP/1.1"));

    assert!(
        !proxy_contacted.load(Ordering::SeqCst),
        "proxy should not have been contacted with Disabled proxy setting"
    );
}

#[tokio::test]
async fn test_explicit_proxy_ignores_no_proxy() {
    let (_lock, _guard) = isolate_env().await;

    let proxy_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();

    // Set NO_PROXY to match all or specific domains
    unsafe {
        std::env::set_var("NO_PROXY", "*");
        std::env::set_var("no_proxy", "*");
    }

    let proxy_handle = tokio::spawn(async move {
        let (mut stream, _) = proxy_listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await.unwrap();
        let request_str = String::from_utf8_lossy(&buf[..n]).to_string();
        let response = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK";
        let _ = stream.write_all(response.as_bytes()).await;
        request_str
    });

    let proxy_url_str = format!("http://{proxy_addr}");
    let setting = ProxySetting::parse(Some(&proxy_url_str)).unwrap();
    let client = client_builder(&setting).unwrap().build().unwrap();

    let target_url = "http://target.example.test:54321/api/test";
    let resp = client.get(target_url).send().await;
    assert!(resp.is_ok());

    let received_request = proxy_handle.await.unwrap();
    assert!(
        received_request.starts_with("GET http://target.example.test:54321/api/test HTTP/1.1"),
        "expected request to go through explicit proxy despite NO_PROXY=*"
    );
}

#[tokio::test]
async fn test_inherit_uses_env_proxy() {
    let (_lock, _guard) = isolate_env().await;

    let proxy_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();

    let proxy_url = format!("http://{proxy_addr}");
    unsafe {
        std::env::set_var("HTTP_PROXY", &proxy_url);
        std::env::set_var("http_proxy", &proxy_url);
        std::env::remove_var("NO_PROXY");
        std::env::remove_var("no_proxy");
    }

    let proxy_handle = tokio::spawn(async move {
        let (mut stream, _) = proxy_listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await.unwrap();
        let request_str = String::from_utf8_lossy(&buf[..n]).to_string();
        let response = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK";
        let _ = stream.write_all(response.as_bytes()).await;
        request_str
    });

    let client = client_builder(&ProxySetting::Inherit)
        .unwrap()
        .build()
        .unwrap();

    let target_url = "http://env-target.example.test:1111/hello";
    let resp = client.get(target_url).send().await;
    assert!(resp.is_ok());

    let received_request = proxy_handle.await.unwrap();
    assert!(
        received_request.starts_with("GET http://env-target.example.test:1111/hello HTTP/1.1"),
        "expected request to reach proxy via HTTP_PROXY with Inherit setting"
    );
}
