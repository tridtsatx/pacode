use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::header::{HeaderMap, HeaderValue};

use super::{delay_from_headers, parse_reset_duration, parse_retry_after};

fn at(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs)
}

#[test]
fn retry_after_delta_seconds() {
    assert_eq!(
        parse_retry_after(" 7 ", at(0)),
        Some(Duration::from_secs(7))
    );
    assert_eq!(parse_retry_after("0", at(0)), Some(Duration::ZERO));
}

#[test]
fn retry_after_http_date() {
    // Sun, 06 Nov 1994 08:49:37 GMT == 784111777
    let target = 784_111_777;
    let got = parse_retry_after("Sun, 06 Nov 1994 08:49:37 GMT", at(target - 30));
    assert_eq!(got, Some(Duration::from_secs(30)));
    // A date in the past asks for no wait.
    let past = parse_retry_after("Sun, 06 Nov 1994 08:49:37 GMT", at(target + 5));
    assert_eq!(past, Some(Duration::ZERO));
}

#[test]
fn retry_after_garbage_is_none() {
    assert_eq!(parse_retry_after("soon", at(0)), None);
    assert_eq!(parse_retry_after("", at(0)), None);
    assert_eq!(
        parse_retry_after("Sun, 99 Foo 1994 08:49:37 GMT", at(0)),
        None
    );
}

#[test]
fn reset_duration_forms() {
    assert_eq!(parse_reset_duration("1s"), Some(Duration::from_secs(1)));
    assert_eq!(
        parse_reset_duration("250ms"),
        Some(Duration::from_millis(250))
    );
    assert_eq!(parse_reset_duration("2m3s"), Some(Duration::from_secs(123)));
    assert_eq!(
        parse_reset_duration("1h2m"),
        Some(Duration::from_secs(3720))
    );
    assert_eq!(
        parse_reset_duration("1.5s"),
        Some(Duration::from_millis(1500))
    );
    assert_eq!(parse_reset_duration("20"), Some(Duration::from_secs(20)));
    assert_eq!(parse_reset_duration("abc"), None);
    assert_eq!(parse_reset_duration("5x"), None);
}

#[test]
fn headers_prefer_retry_after_then_largest_reset() {
    let mut h = HeaderMap::new();
    h.insert("x-ratelimit-reset-requests", HeaderValue::from_static("2s"));
    h.insert("x-ratelimit-reset-tokens", HeaderValue::from_static("1m"));
    assert_eq!(delay_from_headers(&h, at(0)), Some(Duration::from_secs(60)));
    h.insert("retry-after", HeaderValue::from_static("3"));
    assert_eq!(delay_from_headers(&h, at(0)), Some(Duration::from_secs(3)));
    h.insert("retry-after", HeaderValue::from_static("0"));
    assert_eq!(delay_from_headers(&h, at(0)), None);
    assert_eq!(delay_from_headers(&HeaderMap::new(), at(0)), None);
}
