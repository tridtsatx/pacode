//! Five-field cron expressions, evaluated in UTC.
//!
//! `minute hour day-of-month month day-of-week`, each field one of `*`, `*/n`,
//! `a-b`, `a-b/n`, a comma list of those, or a plain number. Day-of-week is
//! `0..=6` with `0` and `7` both meaning Sunday.
//!
//! No dependency and no ambient clock: the evaluator takes the instant to search
//! from, so it is a pure function and tests do not need a fake clock. The search
//! is bounded (see [`SEARCH_LIMIT_DAYS`]) so an expression that can never match —
//! 30 February, say — returns an error instead of spinning.

#[cfg(test)]
#[path = "cron_expr_tests.rs"]
mod cron_expr_tests;

use std::fmt;

/// How far ahead a match is looked for before the expression is called unsatisfiable.
/// Five years covers every leap-year cycle a calendar expression can depend on.
pub const SEARCH_LIMIT_DAYS: u32 = 366 * 5;

/// Why an expression could not be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CronError {
    /// Not five whitespace-separated fields.
    FieldCount(usize),
    /// A field could not be parsed; carries the field name and the offending text.
    Field { field: &'static str, value: String },
    /// Parsed, but no instant within the search window matches.
    Unsatisfiable,
}

impl fmt::Display for CronError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FieldCount(n) => {
                write!(f, "a cron expression needs 5 fields, got {n}")
            }
            Self::Field { field, value } => {
                write!(f, "invalid {field} field: {value}")
            }
            Self::Unsatisfiable => write!(
                f,
                "no run time within {SEARCH_LIMIT_DAYS} days matches this expression"
            ),
        }
    }
}

impl std::error::Error for CronError {}

/// A parsed expression: one allowed-value set per field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CronExpr {
    minutes: Vec<u32>,
    hours: Vec<u32>,
    days_of_month: Vec<u32>,
    months: Vec<u32>,
    days_of_week: Vec<u32>,
    /// `*` in a day field means "do not constrain by it", which is what makes the
    /// classic day-of-month / day-of-week OR rule work.
    dom_restricted: bool,
    dow_restricted: bool,
}

impl CronExpr {
    pub fn parse(expr: &str) -> Result<Self, CronError> {
        let fields: Vec<&str> = expr.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(CronError::FieldCount(fields.len()));
        }
        Ok(Self {
            minutes: parse_field(fields[0], 0, 59, "minute")?,
            hours: parse_field(fields[1], 0, 23, "hour")?,
            days_of_month: parse_field(fields[2], 1, 31, "day-of-month")?,
            months: parse_field(fields[3], 1, 12, "month")?,
            days_of_week: parse_dow(fields[4])?,
            dom_restricted: fields[2] != "*",
            dow_restricted: fields[4] != "*",
        })
    }

    /// The first matching instant strictly after `after_ms`, in Unix milliseconds.
    pub fn next_after_ms(&self, after_ms: u64) -> Result<u64, CronError> {
        // Start from the next whole minute: cron has minute resolution.
        let start_min = after_ms / 60_000 + 1;
        let limit_min = start_min + u64::from(SEARCH_LIMIT_DAYS) * 24 * 60;

        let mut minute_of_epoch = start_min;
        while minute_of_epoch < limit_min {
            let (date, tod) = civil_from_minutes(minute_of_epoch);
            if !self.months.contains(&date.month) {
                // Jump to the first minute of the next month rather than stepping.
                minute_of_epoch = minutes_from_civil(next_month(date), Tod { hour: 0, min: 0 });
                continue;
            }
            if !self.matches_day(&date) {
                minute_of_epoch = minutes_from_civil(next_day(date), Tod { hour: 0, min: 0 });
                continue;
            }
            if !self.hours.contains(&tod.hour) {
                minute_of_epoch += 60 - u64::from(tod.min);
                continue;
            }
            if !self.minutes.contains(&tod.min) {
                minute_of_epoch += 1;
                continue;
            }
            return Ok(minute_of_epoch * 60_000);
        }
        Err(CronError::Unsatisfiable)
    }

    /// Classic cron rule: with both day fields restricted, either may match; with
    /// one restricted, only that one is consulted.
    fn matches_day(&self, date: &Date) -> bool {
        let dom = self.days_of_month.contains(&date.day);
        let dow = self.days_of_week.contains(&date.day_of_week);
        match (self.dom_restricted, self.dow_restricted) {
            (true, true) => dom || dow,
            (true, false) => dom,
            (false, true) => dow,
            (false, false) => true,
        }
    }
}

fn parse_field(text: &str, min: u32, max: u32, field: &'static str) -> Result<Vec<u32>, CronError> {
    let invalid = || CronError::Field {
        field,
        value: text.to_string(),
    };
    let mut out: Vec<u32> = Vec::new();
    for part in text.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return Err(invalid());
        }
        let (range_text, step) = match part.split_once('/') {
            Some((range_text, step_text)) => {
                let step: u32 = step_text.parse().map_err(|_| invalid())?;
                if step == 0 {
                    return Err(invalid());
                }
                (range_text, step)
            }
            None => (part, 1),
        };
        let (lo, hi) = if range_text == "*" {
            (min, max)
        } else if let Some((lo_text, hi_text)) = range_text.split_once('-') {
            let lo: u32 = lo_text.parse().map_err(|_| invalid())?;
            let hi: u32 = hi_text.parse().map_err(|_| invalid())?;
            if lo > hi {
                return Err(invalid());
            }
            (lo, hi)
        } else {
            let v: u32 = range_text.parse().map_err(|_| invalid())?;
            (v, v)
        };
        if lo < min || hi > max {
            return Err(invalid());
        }
        let mut v = lo;
        while v <= hi {
            out.push(v);
            v += step;
        }
    }
    out.sort_unstable();
    out.dedup();
    if out.is_empty() {
        return Err(invalid());
    }
    Ok(out)
}

/// Day-of-week, accepting `7` as another spelling of Sunday.
fn parse_dow(text: &str) -> Result<Vec<u32>, CronError> {
    let mut days = parse_field(text, 0, 7, "day-of-week")?;
    for d in &mut days {
        if *d == 7 {
            *d = 0;
        }
    }
    days.sort_unstable();
    days.dedup();
    Ok(days)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Date {
    year: i64,
    month: u32,
    day: u32,
    /// 0 = Sunday.
    day_of_week: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Tod {
    hour: u32,
    min: u32,
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap(year) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

/// Days since the Unix epoch for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = i64::from(month);
    let d = i64::from(day);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Inverse of [`days_from_civil`].
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn civil_from_minutes(minutes: u64) -> (Date, Tod) {
    let days = (minutes / 1440) as i64;
    let rem = (minutes % 1440) as u32;
    let (year, month, day) = civil_from_days(days);
    // 1970-01-01 was a Thursday; the epoch day count is therefore offset by 4.
    let dow = ((days % 7 + 7 + 4) % 7) as u32;
    (
        Date {
            year,
            month,
            day,
            day_of_week: dow,
        },
        Tod {
            hour: rem / 60,
            min: rem % 60,
        },
    )
}

fn minutes_from_civil(date: Date, tod: Tod) -> u64 {
    let days = days_from_civil(date.year, date.month, date.day);
    (days.max(0) as u64) * 1440 + u64::from(tod.hour) * 60 + u64::from(tod.min)
}

fn next_day(date: Date) -> Date {
    let days = days_from_civil(date.year, date.month, date.day) + 1;
    let (year, month, day) = civil_from_days(days);
    Date {
        year,
        month,
        day,
        day_of_week: ((days % 7 + 7 + 4) % 7) as u32,
    }
}

fn next_month(date: Date) -> Date {
    let (year, month) = if date.month == 12 {
        (date.year + 1, 1)
    } else {
        (date.year, date.month + 1)
    };
    let day = 1.min(days_in_month(year, month));
    let days = days_from_civil(year, month, day);
    Date {
        year,
        month,
        day,
        day_of_week: ((days % 7 + 7 + 4) % 7) as u32,
    }
}
