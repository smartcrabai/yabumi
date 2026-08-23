//! time namespace (a homegrown calendar calculation, STDLIB.md §9, ARCHITECTURE.md §1.6/§2.1).
//! effect: `time`. Has no dedicated DateTime/Duration type; represented as epoch milliseconds
//! (int) (D-STDPOL-06). Implemented using integer arithmetic (Gregorian calendar, including leap
//! year determination) as in Howard Hinnant's civil_from_days/days_from_civil, without a
//! timezone database. UTC only.

use crate::eval::value::Value;
use crate::stdlib::{err_value, error_value, ok_value};
use std::fmt::Write as _;
use std::sync::Arc;

const MS_PER_DAY: i64 = 86_400_000;
const MS_PER_HOUR: i64 = 3_600_000;
const MS_PER_MINUTE: i64 = 60_000;
const MS_PER_SECOND: i64 = 1_000;

/// `now(): int uses {time}`. UNIX epoch milliseconds.
#[must_use]
pub fn now() -> Value {
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let ms = i64::try_from(dur.as_millis()).unwrap_or(i64::MAX);
    Value::Int(ms)
}

/// `sleep(ms: int): void uses {time}`.
pub fn sleep(ms: i64) {
    let ms_u64 = u64::try_from(ms).unwrap_or(0);
    std::thread::sleep(std::time::Duration::from_millis(ms_u64));
}

/// `format(epoch_ms: int, fmt: str): str uses {time}`. strftime-style formatting
/// (`%Y-%m-%d %H:%M:%S`, interpreted as UTC).
#[must_use]
pub fn format(epoch_ms: i64, fmt: &str) -> Value {
    let days = epoch_ms.div_euclid(MS_PER_DAY);
    let ms_of_day = epoch_ms.rem_euclid(MS_PER_DAY);
    let (year, month, day) = civil_from_days(days);
    let hour = ms_of_day / MS_PER_HOUR;
    let minute = (ms_of_day % MS_PER_HOUR) / MS_PER_MINUTE;
    let second = (ms_of_day % MS_PER_MINUTE) / MS_PER_SECOND;
    Value::Str(Arc::from(
        strftime_like(fmt, year, month, day, hour, minute, second).as_str(),
    ))
}

/// `parse(s: str, fmt: str): Result[int, Error] uses {time}` (kind: "time").
#[must_use]
pub fn parse(s: &str, fmt: &str) -> Value {
    let parsed = parse_civil(s, fmt).and_then(|(year, month, day, hour, minute, second)| {
        let days = days_from_civil(year, month, day)?;
        let milliseconds = i128::from(days) * i128::from(MS_PER_DAY)
            + i128::from(hour) * i128::from(MS_PER_HOUR)
            + i128::from(minute) * i128::from(MS_PER_MINUTE)
            + i128::from(second) * i128::from(MS_PER_SECOND);
        i64::try_from(milliseconds).ok()
    });
    match parsed {
        Some(milliseconds) => ok_value(Value::Int(milliseconds)),
        None => err_value(error_value(
            "time",
            format!("failed to parse '{s}' with format '{fmt}'"),
        )),
    }
}

fn strftime_like(
    fmt: &str,
    year: i64,
    month: u32,
    day: u32,
    hour: i64,
    minute: i64,
    second: i64,
) -> String {
    let mut out = String::new();
    let mut chars = fmt.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('Y') => {
                let _ = write!(out, "{year:04}");
            }
            Some('m') => {
                let _ = write!(out, "{month:02}");
            }
            Some('d') => {
                let _ = write!(out, "{day:02}");
            }
            Some('H') => {
                let _ = write!(out, "{hour:02}");
            }
            Some('M') => {
                let _ = write!(out, "{minute:02}");
            }
            Some('S') => {
                let _ = write!(out, "{second:02}");
            }
            // A stray trailing `%` at the end of fmt (chars.next() returns None) and a literal
            // `%%` both just write a single `%` and are indistinguishable in the output (the
            // patterns are combined with `|` to avoid match_same_arms).
            Some('%') | None => out.push('%'),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
        }
    }
    out
}

/// Literal characters in `fmt` must match `s` exactly; `%Y/%m/%d/%H/%M/%S` greedily consume
/// consecutive digits (since `format` always separates fields with separators, this poses no
/// practical ambiguity).
fn parse_civil(s: &str, fmt: &str) -> Option<(i64, u32, u32, i64, i64, i64)> {
    let mut year: i64 = 1970;
    let mut month: u32 = 1;
    let mut day: u32 = 1;
    let mut hour: i64 = 0;
    let mut minute: i64 = 0;
    let mut second: i64 = 0;

    let mut fmt_chars = fmt.chars();
    let mut s_chars = s.chars().peekable();

    while let Some(fc) = fmt_chars.next() {
        if fc == '%' {
            let spec = fmt_chars.next()?;
            match spec {
                'Y' => year = take_digits(&mut s_chars)?,
                'm' => month = u32::try_from(take_digits(&mut s_chars)?).ok()?,
                'd' => day = u32::try_from(take_digits(&mut s_chars)?).ok()?,
                'H' => hour = take_digits(&mut s_chars)?,
                'M' => minute = take_digits(&mut s_chars)?,
                'S' => second = take_digits(&mut s_chars)?,
                '%' => {
                    if s_chars.next()? != '%' {
                        return None;
                    }
                }
                _ => return None,
            }
        } else if s_chars.next()? != fc {
            return None;
        }
    }
    if s_chars.next().is_some()
        || !(1..=12).contains(&month)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=59).contains(&second)
        || day == 0
        || day > days_in_month(year, month)
    {
        return None;
    }
    Some((year, month, day, hour, minute, second))
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year.rem_euclid(400) == 0 || year.rem_euclid(4) == 0 && year.rem_euclid(100) != 0 => {
            29
        }
        2 => 28,
        _ => 0,
    }
}

fn take_digits(iter: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<i64> {
    let mut digits = String::new();
    while let Some(&c) = iter.peek() {
        if !c.is_ascii_digit() {
            break;
        }
        digits.push(c);
        iter.next();
    }
    if digits.is_empty() {
        return None;
    }
    digits.parse::<i64>().ok()
}

/// Converts between the number of days since the UNIX epoch (1970-01-01) and a Gregorian
/// calendar year/month/day (Howard Hinnant's algorithm,
/// <http://howardhinnant.github.io/date_algorithms.html>). No timezone support (UTC only,
/// D-STDPOL-06).
fn days_from_civil(year: i64, month: u32, day: u32) -> Option<i64> {
    let month = i128::from(month);
    let day = i128::from(day);
    let mut year = i128::from(year);
    if month <= 2 {
        year -= 1;
    }
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let adjusted_month = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * adjusted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    i64::try_from(era * 146_097 + day_of_era - 719_468).ok()
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };
    (
        year,
        u32::try_from(m).unwrap_or(1),
        u32::try_from(d).unwrap_or(1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_renders_fixed_epoch_ms_as_utc() {
        let formatted = format(1_700_000_000_000, "%Y-%m-%d %H:%M:%S");
        assert_eq!(formatted, Value::Str(Arc::from("2023-11-14 22:13:20")));
    }

    #[test]
    fn parse_round_trips_the_formatted_string() {
        let parsed = parse("2023-11-14 22:13:20", "%Y-%m-%d %H:%M:%S");
        let Value::Enum(inst) = &parsed else {
            panic!("expected Result[int, Error]")
        };
        assert_eq!(inst.variant_name.as_ref(), "Ok");
        assert_eq!(inst.fields[0], Value::Int(1_700_000_000_000));
    }

    #[test]
    fn parse_epoch_zero_round_trips() {
        let formatted = format(0, "%Y-%m-%d %H:%M:%S");
        assert_eq!(formatted, Value::Str(Arc::from("1970-01-01 00:00:00")));
        let parsed = parse("1970-01-01 00:00:00", "%Y-%m-%d %H:%M:%S");
        let Value::Enum(inst) = &parsed else {
            panic!("expected Result")
        };
        assert_eq!(inst.fields[0], Value::Int(0));
    }

    #[test]
    fn parse_before_epoch_round_trips_negative_ms() {
        let epoch_ms = -1_000; // 1969-12-31 23:59:59
        let formatted = format(epoch_ms, "%Y-%m-%d %H:%M:%S");
        assert_eq!(formatted, Value::Str(Arc::from("1969-12-31 23:59:59")));
        let parsed = parse("1969-12-31 23:59:59", "%Y-%m-%d %H:%M:%S");
        let Value::Enum(inst) = &parsed else {
            panic!("expected Result")
        };
        assert_eq!(inst.fields[0], Value::Int(epoch_ms));
    }

    #[test]
    fn parse_invalid_input_is_err_with_time_kind() {
        let parsed = parse("not-a-date", "%Y-%m-%d %H:%M:%S");
        let Value::Enum(inst) = &parsed else {
            panic!("expected Result")
        };
        assert_eq!(inst.variant_name.as_ref(), "Err");
        let Value::Struct(err) = &inst.fields[0] else {
            panic!("expected Error")
        };
        assert_eq!(err.fields[0], Value::Str(Arc::from("time")));
    }

    #[test]
    fn now_returns_a_positive_int() {
        let Value::Int(n) = now() else {
            panic!("expected int")
        };
        assert!(n > 0);
    }

    #[test]
    fn sleep_returns_promptly_for_small_duration() {
        sleep(1);
    }

    #[test]
    fn civil_days_round_trip_is_an_exact_inverse() {
        for days in [-800_000_i64, -1, 0, 1, 19675, 700_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), Some(days));
        }
    }

    /// Verifies SPEC §11.2 / STDLIB.md §9 through the full pipeline
    /// (`samples/ok/11-2_time/entry_main.ybm`).
    #[test]
    fn sample_time_runs_end_to_end() {
        let result = crate::stdlib::builtins::test_pipeline::run_ok_sample("11-2_time");
        assert!(
            result.is_ok(),
            "sample should run without Abort: {result:?}"
        );
    }
}
