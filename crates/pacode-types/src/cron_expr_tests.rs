use super::*;

/// Unix ms for a UTC date-time, built from the same civil arithmetic the
/// evaluator uses so a test failure points at the matcher, not at the fixture.
fn at(year: i64, month: u32, day: u32, hour: u32, min: u32) -> u64 {
    let days = days_from_civil(year, month, day);
    ((days as u64) * 1440 + u64::from(hour) * 60 + u64::from(min)) * 60_000
}

#[test]
fn every_minute_fires_on_the_next_minute() {
    let e = CronExpr::parse("* * * * *").expect("parse");
    let now = at(2026, 9, 10, 12, 30) + 15_000;
    assert_eq!(e.next_after_ms(now), Ok(at(2026, 9, 10, 12, 31)));
}

#[test]
fn a_fixed_time_fires_once_a_day() {
    let e = CronExpr::parse("30 3 * * *").expect("parse");
    assert_eq!(
        e.next_after_ms(at(2026, 9, 10, 3, 29)),
        Ok(at(2026, 9, 10, 3, 30))
    );
    // Just past it: tomorrow.
    assert_eq!(
        e.next_after_ms(at(2026, 9, 10, 3, 30)),
        Ok(at(2026, 9, 11, 3, 30))
    );
}

#[test]
fn step_ranges_and_lists_are_honoured() {
    let e = CronExpr::parse("*/15 * * * *").expect("parse");
    assert_eq!(
        e.next_after_ms(at(2026, 9, 10, 8, 1)),
        Ok(at(2026, 9, 10, 8, 15))
    );

    let e = CronExpr::parse("0 9-17/4 * * *").expect("parse");
    assert_eq!(
        e.next_after_ms(at(2026, 9, 10, 0, 0)),
        Ok(at(2026, 9, 10, 9, 0))
    );
    assert_eq!(
        e.next_after_ms(at(2026, 9, 10, 9, 0)),
        Ok(at(2026, 9, 10, 13, 0))
    );

    let e = CronExpr::parse("0,30 0 1,15 * *").expect("parse");
    assert_eq!(
        e.next_after_ms(at(2026, 9, 2, 0, 0)),
        Ok(at(2026, 9, 15, 0, 0))
    );
}

#[test]
fn day_of_week_matches_and_accepts_seven_as_sunday() {
    // 2026-09-14 is a Monday.
    let monday = CronExpr::parse("0 6 * * 1").expect("parse");
    assert_eq!(
        monday.next_after_ms(at(2026, 9, 10, 0, 0)),
        Ok(at(2026, 9, 14, 6, 0))
    );

    let sunday_zero = CronExpr::parse("0 6 * * 0").expect("parse");
    let sunday_seven = CronExpr::parse("0 6 * * 7").expect("parse");
    let from = at(2026, 9, 10, 0, 0);
    assert_eq!(
        sunday_zero.next_after_ms(from),
        sunday_seven.next_after_ms(from)
    );
    assert_eq!(sunday_zero.next_after_ms(from), Ok(at(2026, 9, 13, 6, 0)));
}

#[test]
fn both_day_fields_restricted_means_either_may_match() {
    // The 1st of the month, or any Friday.
    let e = CronExpr::parse("0 0 1 * 5").expect("parse");
    // 2026-09-11 is a Friday, and comes before the 1st of October.
    assert_eq!(
        e.next_after_ms(at(2026, 9, 10, 0, 0)),
        Ok(at(2026, 9, 11, 0, 0))
    );
    assert_eq!(
        e.next_after_ms(at(2026, 9, 26, 0, 0)),
        Ok(at(2026, 10, 1, 0, 0))
    );
}

#[test]
fn a_leap_day_expression_skips_to_the_next_leap_year() {
    let e = CronExpr::parse("0 0 29 2 *").expect("parse");
    // 2028 is the next leap year after 2026.
    assert_eq!(
        e.next_after_ms(at(2026, 3, 1, 0, 0)),
        Ok(at(2028, 2, 29, 0, 0))
    );
}

#[test]
fn a_match_a_year_away_is_still_found() {
    let e = CronExpr::parse("0 0 1 1 *").expect("parse");
    assert_eq!(
        e.next_after_ms(at(2026, 1, 1, 0, 1)),
        Ok(at(2027, 1, 1, 0, 0))
    );
}

#[test]
fn an_impossible_date_is_reported_instead_of_spinning() {
    // February never has 30 days.
    let e = CronExpr::parse("0 0 30 2 *").expect("parse");
    assert_eq!(
        e.next_after_ms(at(2026, 1, 1, 0, 0)),
        Err(CronError::Unsatisfiable)
    );
}

#[test]
fn malformed_expressions_are_rejected_with_the_offending_field() {
    assert_eq!(CronExpr::parse("* * * *"), Err(CronError::FieldCount(4)));
    assert_eq!(
        CronExpr::parse("* * * * * *"),
        Err(CronError::FieldCount(6))
    );
    assert_eq!(
        CronExpr::parse("60 * * * *"),
        Err(CronError::Field {
            field: "minute",
            value: "60".to_string()
        })
    );
    assert_eq!(
        CronExpr::parse("* 24 * * *"),
        Err(CronError::Field {
            field: "hour",
            value: "24".to_string()
        })
    );
    assert_eq!(
        CronExpr::parse("* * 0 * *"),
        Err(CronError::Field {
            field: "day-of-month",
            value: "0".to_string()
        })
    );
    assert_eq!(
        CronExpr::parse("*/0 * * * *"),
        Err(CronError::Field {
            field: "minute",
            value: "*/0".to_string()
        })
    );
    assert_eq!(
        CronExpr::parse("5-1 * * * *"),
        Err(CronError::Field {
            field: "minute",
            value: "5-1".to_string()
        })
    );
    assert!(CronExpr::parse("x * * * *").is_err());
}

#[test]
fn the_civil_calendar_round_trips() {
    for (y, m, d) in [
        (1970, 1, 1),
        (2000, 2, 29),
        (2026, 9, 10),
        (2028, 2, 29),
        (2100, 3, 1),
    ] {
        let days = days_from_civil(y, m, d);
        assert_eq!(civil_from_days(days), (y, m, d), "{y}-{m}-{d}");
    }

    // 1970-01-01 was a Thursday (4), 2026-09-10 is a Thursday too.
    let (date, tod) = civil_from_minutes(0);
    assert_eq!(date.day_of_week, 4);
    assert_eq!(tod, Tod { hour: 0, min: 0 });
    let (date, _) = civil_from_minutes(at(2026, 9, 13, 0, 0) / 60_000);
    assert_eq!(date.day_of_week, 0, "2026-09-13 is a Sunday");
}
