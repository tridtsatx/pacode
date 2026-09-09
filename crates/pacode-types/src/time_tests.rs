use super::*;

#[test]
fn durations() {
    assert_eq!(format_duration_ms(0), "0ms");
    assert_eq!(format_duration_ms(342), "342ms");
    assert_eq!(format_duration_ms(999), "999ms");
    assert_eq!(format_duration_ms(1_000), "1.0s");
    assert_eq!(format_duration_ms(7_300), "7.3s");
    assert_eq!(format_duration_ms(9_900), "9.9s");
    assert_eq!(format_duration_ms(10_000), "10s");
    assert_eq!(format_duration_ms(41_000), "41s");
    assert_eq!(format_duration_ms(42_000), "42s");
    assert_eq!(format_duration_ms(59_000), "59s");
    assert_eq!(format_duration_ms(59_999), "59s");
    assert_eq!(format_duration_ms(60_000), "1m00s");
    assert_eq!(format_duration_ms(65_000), "1m05s");
    assert_eq!(format_duration_ms(107_000), "1m47s");
    assert_eq!(format_duration_ms(3_599_000), "59m59s");
    assert_eq!(format_duration_ms(3_600_000), "1h00m");
    assert_eq!(format_duration_ms(3_720_000), "1h02m");
}

#[test]
fn tokens() {
    assert_eq!(format_tokens(842), "842");
    assert_eq!(format_tokens(9_100), "9.1k");
    assert_eq!(format_tokens(207_600), "207.6k");
    assert_eq!(format_tokens(1_200_000), "1.2M");
}
