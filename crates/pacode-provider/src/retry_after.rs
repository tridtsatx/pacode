//! Parsing of server back-pressure hints: `Retry-After` and the OpenAI-style
//! `x-ratelimit-reset-*` headers. Pure functions, no state.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::header::HeaderMap;

#[cfg(test)]
#[path = "retry_after_tests.rs"]
mod retry_after_tests;

/// Header names consulted, in priority order.
const HEADER_SCAN: [&str; 3] = [
    "retry-after",
    "x-ratelimit-reset-requests",
    "x-ratelimit-reset-tokens",
];

/// Delay requested by the response headers, if any (and non-zero).
///
/// `Retry-After` wins; otherwise the larger of the two `x-ratelimit-reset-*` hints.
pub(crate) fn delay_from_headers(headers: &HeaderMap, now: SystemTime) -> Option<Duration> {
    let text = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    if let Some(d) = text(HEADER_SCAN[0]).and_then(|v| parse_retry_after(v, now)) {
        return (!d.is_zero()).then_some(d);
    }
    let reset = HEADER_SCAN[1..]
        .iter()
        .filter_map(|name| text(name).and_then(parse_reset_duration))
        .max()?;
    (!reset.is_zero()).then_some(reset)
}

/// `Retry-After`: delta-seconds, or an IMF-fixdate HTTP-date (`Sun, 06 Nov 1994 08:49:37 GMT`).
/// A date in the past yields zero.
pub(crate) fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let value = value.trim();
    if let Ok(secs) = value.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    let target = parse_http_date(value)?;
    Some(target.duration_since(now).unwrap_or(Duration::ZERO))
}

/// `1s`, `250ms`, `2m3s`, `1h2m`, `1.5s`; a bare number is seconds.
pub(crate) fn parse_reset_duration(value: &str) -> Option<Duration> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let mut total = 0.0f64;
    let mut rest = value;
    while !rest.is_empty() {
        let num_end = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(rest.len());
        if num_end == 0 {
            return None;
        }
        let number: f64 = rest[..num_end].parse().ok()?;
        rest = &rest[num_end..];
        let unit_end = rest
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(rest.len());
        let factor = match &rest[..unit_end] {
            "" | "s" => 1.0,
            "ms" => 0.001,
            "m" => 60.0,
            "h" => 3600.0,
            _ => return None,
        };
        rest = &rest[unit_end..];
        total += number * factor;
    }
    Duration::try_from_secs_f64(total).ok()
}

fn parse_http_date(value: &str) -> Option<SystemTime> {
    let (_, rest) = value.split_once(',')?;
    let mut parts = rest.split_whitespace();
    let day: i64 = parts.next()?.parse().ok()?;
    let month = match parts.next()? {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year: i64 = parts.next()?.parse().ok()?;
    let mut hms = parts.next()?.split(':');
    let h: i64 = hms.next()?.parse().ok()?;
    let m: i64 = hms.next()?.parse().ok()?;
    let s: i64 = hms.next()?.parse().ok()?;
    if !(1..=31).contains(&day) || h > 23 || m > 59 || s > 60 {
        return None;
    }
    let secs = days_from_civil(year, month, day) * 86_400 + h * 3600 + m * 60 + s;
    u64::try_from(secs)
        .ok()
        .map(|s| UNIX_EPOCH + Duration::from_secs(s))
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}
