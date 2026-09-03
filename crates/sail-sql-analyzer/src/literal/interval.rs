use std::iter::once;
use std::str::FromStr;

use chrono::{self, TimeDelta};
use lazy_static::lazy_static;
use regex::Regex;
use sail_common::spec;
use sail_sql_parser::ast::data_type::{IntervalDayTimeUnit, IntervalYearMonthUnit};
use sail_sql_parser::ast::expression::{
    Expr, IntervalExpr, IntervalQualifier, IntervalUnit, IntervalValueWithUnit,
};

use crate::error::{SqlError, SqlResult};
use crate::literal::utils::{Signed, extract_fraction_match, extract_match, parse_signed_value};
use crate::value::from_ast_string;

fn create_regex(regex: Result<Regex, regex::Error>) -> Regex {
    #[expect(clippy::unwrap_used)]
    regex.unwrap()
}

// Spark's patterns give the leading field any width and the fields after it
// one or two digits; a fraction is one to nine digits and requires its dot.
// Field ranges (hours 0-23 under a day, minutes and seconds 0-59, months 0-11
// under a year) are checked in code, since they are about values, not widths.
lazy_static! {
    static ref INTERVAL_YEAR_REGEX: Regex =
        create_regex(Regex::new(r"^\s*(?P<sign>[+-]?)(?P<year>\d+)\s*$"));
    static ref INTERVAL_YEAR_TO_MONTH_REGEX: Regex = create_regex(Regex::new(
        r"^\s*(?P<sign>[+-]?)(?P<year>\d+)-(?P<month>\d+)\s*$"
    ));
    static ref INTERVAL_MONTH_REGEX: Regex =
        create_regex(Regex::new(r"^\s*(?P<sign>[+-]?)(?P<month>\d+)\s*$"));
    static ref INTERVAL_DAY_REGEX: Regex =
        create_regex(Regex::new(r"^\s*(?P<sign>[+-]?)(?P<day>\d+)\s*$"));
    static ref INTERVAL_DAY_TO_HOUR_REGEX: Regex = create_regex(Regex::new(
        r"^\s*(?P<sign>[+-]?)(?P<day>\d+)\s+(?P<hour>\d{1,2})\s*$"
    ));
    static ref INTERVAL_DAY_TO_MINUTE_REGEX: Regex = create_regex(Regex::new(
        r"^\s*(?P<sign>[+-]?)(?P<day>\d+)\s+(?P<hour>\d{1,2}):(?P<minute>\d{1,2})\s*$"
    ));
    static ref INTERVAL_DAY_TO_SECOND_REGEX: Regex = create_regex(Regex::new(
        r"^\s*(?P<sign>[+-]?)(?P<day>\d+)\s+(?P<hour>\d{1,2}):(?P<minute>\d{1,2}):(?P<second>\d{1,2})([.](?P<fraction>\d{1,9}))?\s*$"
    ));
    static ref INTERVAL_HOUR_REGEX: Regex =
        create_regex(Regex::new(r"^\s*(?P<sign>[+-]?)(?P<hour>\d+)\s*$"));
    static ref INTERVAL_HOUR_TO_MINUTE_REGEX: Regex = create_regex(Regex::new(
        r"^\s*(?P<sign>[+-]?)(?P<hour>\d+):(?P<minute>\d{1,2})\s*$"
    ));
    static ref INTERVAL_HOUR_TO_SECOND_REGEX: Regex = create_regex(Regex::new(
        r"^\s*(?P<sign>[+-]?)(?P<hour>\d+):(?P<minute>\d{1,2}):(?P<second>\d{1,2})([.](?P<fraction>\d{1,9}))?\s*$"
    ));
    static ref INTERVAL_MINUTE_REGEX: Regex =
        create_regex(Regex::new(r"^\s*(?P<sign>[+-]?)(?P<minute>\d+)\s*$"));
    static ref INTERVAL_MINUTE_TO_SECOND_REGEX: Regex = create_regex(Regex::new(
        r"^\s*(?P<sign>[+-]?)(?P<minute>\d+):(?P<second>\d{1,2})([.](?P<fraction>\d{1,9}))?\s*$"
    ));
    static ref INTERVAL_SECOND_REGEX: Regex = create_regex(Regex::new(
        r"^\s*(?P<sign>[+-]?)(?P<second>\d+)([.](?P<fraction>\d{1,9}))?\s*$"
    ));
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum IntervalValue {
    YearMonth {
        months: i32,
    },
    Microsecond {
        microseconds: i64,
    },
    MonthDayNanosecond {
        months: i32,
        days: i32,
        nanoseconds: i64,
    },
}

impl From<IntervalValue> for spec::Literal {
    fn from(value: IntervalValue) -> Self {
        match value {
            IntervalValue::YearMonth { months } => spec::Literal::IntervalYearMonth {
                months: Some(months),
            },
            IntervalValue::Microsecond { microseconds } => spec::Literal::DurationMicrosecond {
                microseconds: Some(microseconds),
            },
            IntervalValue::MonthDayNanosecond {
                months,
                days,
                nanoseconds,
            } => spec::Literal::IntervalMonthDayNano {
                value: Some(spec::IntervalMonthDayNano {
                    months,
                    days,
                    nanoseconds,
                }),
            },
        }
    }
}

pub fn from_ast_signed_interval(value: Signed<IntervalExpr>) -> SqlResult<IntervalValue> {
    // TODO: support the legacy calendar interval when `spark.sql.legacy.interval.enabled` is `true`
    let negated = value.is_negative();
    let interval = value.into_inner();
    match interval.clone() {
        IntervalExpr::Standard { value, qualifier } => {
            let kind = from_ast_interval_qualifier(qualifier)?;
            from_ast_standard_interval(value, kind, negated)
        }
        IntervalExpr::MultiUnit { head, tail } => {
            if tail.is_empty() {
                match head.unit {
                    IntervalUnit::Year(_) | IntervalUnit::Years(_) => {
                        from_ast_standard_interval(head.value, StandardIntervalKind::Year, negated)
                    }
                    IntervalUnit::Month(_) | IntervalUnit::Months(_) => {
                        from_ast_standard_interval(head.value, StandardIntervalKind::Month, negated)
                    }
                    IntervalUnit::Day(_) | IntervalUnit::Days(_) => {
                        from_ast_standard_interval(head.value, StandardIntervalKind::Day, negated)
                    }
                    IntervalUnit::Hour(_) | IntervalUnit::Hours(_) => {
                        from_ast_standard_interval(head.value, StandardIntervalKind::Hour, negated)
                    }
                    IntervalUnit::Minute(_) | IntervalUnit::Minutes(_) => {
                        from_ast_standard_interval(
                            head.value,
                            StandardIntervalKind::Minute,
                            negated,
                        )
                    }
                    IntervalUnit::Second(_) | IntervalUnit::Seconds(_) => {
                        from_ast_standard_interval(
                            head.value,
                            StandardIntervalKind::Second,
                            negated,
                        )
                    }
                    _ => from_ast_multi_unit_interval(vec![head], negated),
                }
            } else {
                let values = once(head).chain(tail).collect();
                from_ast_multi_unit_interval(values, negated)
            }
        }
        IntervalExpr::Literal(value) => {
            parse_unqualified_interval_string(&from_ast_string(value)?, negated)
        }
    }
}

struct DecimalSecond {
    seconds: u32,
    microseconds: u32,
}

impl FromStr for Signed<DecimalSecond> {
    type Err = SqlError;

    fn from_str(s: &str) -> SqlResult<Self> {
        let error = || SqlError::invalid(format!("second: {s:?}"));
        let captures = INTERVAL_SECOND_REGEX.captures(s).ok_or_else(error)?;
        let negated = captures.name("sign").map(|s| s.as_str()) == Some("-");
        let seconds: u32 = extract_match(&captures, "second", error)?.unwrap_or(0);
        let microseconds: u32 =
            extract_fraction_match(&captures, "fraction", 6, error)?.unwrap_or(0);
        let value = DecimalSecond {
            seconds,
            microseconds,
        };
        if negated {
            Ok(Signed::Negative(value))
        } else {
            Ok(Signed::Positive(value))
        }
    }
}

fn parse_interval_year_month_string(
    s: &str,
    negated: bool,
    interval_regex: &Regex,
) -> SqlResult<IntervalValue> {
    let error = || SqlError::invalid(format!("interval: {s}"));
    let captures = interval_regex.captures(s).ok_or_else(error)?;
    let string_negates = captures.name("sign").map(|s| s.as_str()) == Some("-");
    let years: i64 = extract_match(&captures, "year", error)?.unwrap_or(0);
    let months: i64 = extract_match(&captures, "month", error)?.unwrap_or(0);
    // Months are a field of the year when one is written, so they are 0-11.
    if captures.name("year").is_some() && months > 11 {
        return Err(error());
    }
    let mut total = years
        .checked_mul(12)
        .and_then(|y| y.checked_add(months))
        .ok_or_else(error)?;
    if string_negates {
        total = total.checked_neg().ok_or_else(error)?;
    }
    let mut n = i32::try_from(total).map_err(|_| error())?;
    if negated {
        n = n.checked_neg().ok_or_else(error)?;
    }
    Ok(IntervalValue::YearMonth { months: n })
}

fn parse_interval_day_time_string(
    s: &str,
    negated: bool,
    interval_regex: &Regex,
) -> SqlResult<IntervalValue> {
    let error = || SqlError::invalid(format!("interval: {s}"));
    let captures = interval_regex.captures(s).ok_or_else(error)?;
    let string_negates = captures.name("sign").map(|s| s.as_str()) == Some("-");
    let days: i64 = extract_match(&captures, "day", error)?.unwrap_or(0);
    let hours: i64 = extract_match(&captures, "hour", error)?.unwrap_or(0);
    let minutes: i64 = extract_match(&captures, "minute", error)?.unwrap_or(0);
    let seconds: i64 = extract_match(&captures, "second", error)?.unwrap_or(0);
    let microseconds: i64 = extract_fraction_match(&captures, "fraction", 6, error)?.unwrap_or(0);
    // Fields below the leading one are bounded: hours 0-23, minutes/seconds 0-59.
    let has = |name: &str| captures.name(name).is_some();
    if (has("day") && hours > 23)
        || (has("hour") && minutes > 59)
        || ((has("hour") || has("minute")) && seconds > 59)
    {
        return Err(error());
    }
    // Accumulate with the in-string sign so `'-106751991 04:00:54.775808'`
    // reaches `i64::MIN`.
    let mut total: i64 = 0;
    for (value, unit) in [
        (days, 86_400_000_000),
        (hours, 3_600_000_000),
        (minutes, 60_000_000),
        (seconds, 1_000_000),
        (microseconds, 1),
    ] {
        let part = value.checked_mul(unit).ok_or_else(error)?;
        total = if string_negates {
            total.checked_sub(part)
        } else {
            total.checked_add(part)
        }
        .ok_or_else(error)?;
    }
    let n = if negated {
        total.checked_neg().ok_or_else(error)?
    } else {
        total
    };
    Ok(IntervalValue::Microsecond { microseconds: n })
}

enum StandardIntervalKind {
    Year,
    YearToMonth,
    Month,
    Day,
    DayToHour,
    DayToMinute,
    DayToSecond,
    Hour,
    HourToMinute,
    HourToSecond,
    Minute,
    MinuteToSecond,
    Second,
}

fn from_ast_interval_qualifier(qualifier: IntervalQualifier) -> SqlResult<StandardIntervalKind> {
    match qualifier {
        IntervalQualifier::YearMonth(IntervalYearMonthUnit::Year(_), None) => {
            Ok(StandardIntervalKind::Year)
        }
        IntervalQualifier::YearMonth(
            IntervalYearMonthUnit::Year(_),
            Some((_, IntervalYearMonthUnit::Month(_))),
        ) => Ok(StandardIntervalKind::YearToMonth),
        IntervalQualifier::YearMonth(IntervalYearMonthUnit::Month(_), None) => {
            Ok(StandardIntervalKind::Month)
        }
        IntervalQualifier::DayTime(IntervalDayTimeUnit::Day(_), None) => {
            Ok(StandardIntervalKind::Day)
        }
        IntervalQualifier::DayTime(
            IntervalDayTimeUnit::Day(_),
            Some((_, IntervalDayTimeUnit::Hour(_))),
        ) => Ok(StandardIntervalKind::DayToHour),
        IntervalQualifier::DayTime(
            IntervalDayTimeUnit::Day(_),
            Some((_, IntervalDayTimeUnit::Minute(_))),
        ) => Ok(StandardIntervalKind::DayToMinute),
        IntervalQualifier::DayTime(
            IntervalDayTimeUnit::Day(_),
            Some((_, IntervalDayTimeUnit::Second(_))),
        ) => Ok(StandardIntervalKind::DayToSecond),
        IntervalQualifier::DayTime(IntervalDayTimeUnit::Hour(_), None) => {
            Ok(StandardIntervalKind::Hour)
        }
        IntervalQualifier::DayTime(
            IntervalDayTimeUnit::Hour(_),
            Some((_, IntervalDayTimeUnit::Minute(_))),
        ) => Ok(StandardIntervalKind::HourToMinute),
        IntervalQualifier::DayTime(
            IntervalDayTimeUnit::Hour(_),
            Some((_, IntervalDayTimeUnit::Second(_))),
        ) => Ok(StandardIntervalKind::HourToSecond),
        IntervalQualifier::DayTime(IntervalDayTimeUnit::Minute(_), None) => {
            Ok(StandardIntervalKind::Minute)
        }
        IntervalQualifier::DayTime(
            IntervalDayTimeUnit::Minute(_),
            Some((_, IntervalDayTimeUnit::Second(_))),
        ) => Ok(StandardIntervalKind::MinuteToSecond),
        IntervalQualifier::DayTime(IntervalDayTimeUnit::Second(_), None) => {
            Ok(StandardIntervalKind::Second)
        }
        _ => Err(SqlError::invalid("interval qualifier")),
    }
}

fn from_ast_standard_interval(
    value: Expr,
    kind: StandardIntervalKind,
    negated: bool,
) -> SqlResult<IntervalValue> {
    let signed: Signed<String> = parse_signed_value(value)?;
    let negated = signed.is_negative() ^ negated;
    let value = signed.into_inner();
    match kind {
        StandardIntervalKind::Year => {
            parse_interval_year_month_string(&value, negated, &INTERVAL_YEAR_REGEX)
        }
        StandardIntervalKind::YearToMonth => {
            parse_interval_year_month_string(&value, negated, &INTERVAL_YEAR_TO_MONTH_REGEX)
        }
        StandardIntervalKind::Month => {
            parse_interval_year_month_string(&value, negated, &INTERVAL_MONTH_REGEX)
        }
        StandardIntervalKind::Day => {
            parse_interval_day_time_string(&value, negated, &INTERVAL_DAY_REGEX)
        }
        StandardIntervalKind::DayToHour => {
            parse_interval_day_time_string(&value, negated, &INTERVAL_DAY_TO_HOUR_REGEX)
        }
        StandardIntervalKind::DayToMinute => {
            parse_interval_day_time_string(&value, negated, &INTERVAL_DAY_TO_MINUTE_REGEX)
        }
        StandardIntervalKind::DayToSecond => {
            parse_interval_day_time_string(&value, negated, &INTERVAL_DAY_TO_SECOND_REGEX)
        }
        StandardIntervalKind::Hour => {
            parse_interval_day_time_string(&value, negated, &INTERVAL_HOUR_REGEX)
        }
        StandardIntervalKind::HourToMinute => {
            parse_interval_day_time_string(&value, negated, &INTERVAL_HOUR_TO_MINUTE_REGEX)
        }
        StandardIntervalKind::HourToSecond => {
            parse_interval_day_time_string(&value, negated, &INTERVAL_HOUR_TO_SECOND_REGEX)
        }
        StandardIntervalKind::Minute => {
            parse_interval_day_time_string(&value, negated, &INTERVAL_MINUTE_REGEX)
        }
        StandardIntervalKind::MinuteToSecond => {
            parse_interval_day_time_string(&value, negated, &INTERVAL_MINUTE_TO_SECOND_REGEX)
        }
        StandardIntervalKind::Second => {
            parse_interval_day_time_string(&value, negated, &INTERVAL_SECOND_REGEX)
        }
    }
}

fn from_ast_multi_unit_interval(
    values: Vec<IntervalValueWithUnit>,
    negated: bool,
) -> SqlResult<IntervalValue> {
    let error = || SqlError::invalid("multi-unit interval");
    let mut months = 0i32;
    let mut delta = TimeDelta::zero();
    for value in values {
        let IntervalValueWithUnit { value, unit } = value;
        match unit {
            IntervalUnit::Year(_) | IntervalUnit::Years(_) => {
                let value: i32 = parse_signed_value(value)?;
                let m = value.checked_mul(12).ok_or_else(error)?;
                months = months.checked_add(m).ok_or_else(error)?;
            }
            IntervalUnit::Month(_) | IntervalUnit::Months(_) => {
                let value: i32 = parse_signed_value(value)?;
                months = months.checked_add(value).ok_or_else(error)?;
            }
            IntervalUnit::Week(_) | IntervalUnit::Weeks(_) => {
                let value: i64 = parse_signed_value(value)?;
                let weeks = TimeDelta::try_weeks(value).ok_or_else(error)?;
                delta = delta.checked_add(&weeks).ok_or_else(error)?;
            }
            IntervalUnit::Day(_) | IntervalUnit::Days(_) => {
                let value: i64 = parse_signed_value(value)?;
                let days = TimeDelta::try_days(value).ok_or_else(error)?;
                delta = delta.checked_add(&days).ok_or_else(error)?;
            }
            IntervalUnit::Hour(_) | IntervalUnit::Hours(_) => {
                let value: i64 = parse_signed_value(value)?;
                let hours = TimeDelta::try_hours(value).ok_or_else(error)?;
                delta = delta.checked_add(&hours).ok_or_else(error)?;
            }
            IntervalUnit::Minute(_) | IntervalUnit::Minutes(_) => {
                let value: i64 = parse_signed_value(value)?;
                let minutes = TimeDelta::try_minutes(value).ok_or_else(error)?;
                delta = delta.checked_add(&minutes).ok_or_else(error)?;
            }
            IntervalUnit::Second(_) | IntervalUnit::Seconds(_) => {
                let value: Signed<DecimalSecond> = parse_signed_value(value)?;
                let negated = value.is_negative();
                let value = value.into_inner();
                let seconds = TimeDelta::seconds(value.seconds as i64);
                let microseconds = TimeDelta::microseconds(value.microseconds as i64);
                if negated {
                    delta = delta.checked_sub(&seconds).ok_or_else(error)?;
                    delta = delta.checked_sub(&microseconds).ok_or_else(error)?;
                } else {
                    delta = delta.checked_add(&seconds).ok_or_else(error)?;
                    delta = delta.checked_add(&microseconds).ok_or_else(error)?;
                }
            }
            IntervalUnit::Millisecond(_) | IntervalUnit::Milliseconds(_) => {
                let value: i64 = parse_signed_value(value)?;
                let milliseconds = TimeDelta::try_milliseconds(value).ok_or_else(error)?;
                delta = delta.checked_add(&milliseconds).ok_or_else(error)?;
            }
            IntervalUnit::Microsecond(_) | IntervalUnit::Microseconds(_) => {
                let value: i64 = parse_signed_value(value)?;
                let microseconds = TimeDelta::microseconds(value);
                delta = delta.checked_add(&microseconds).ok_or_else(error)?;
            }
        }
    }
    match (months != 0, delta != TimeDelta::zero()) {
        (true, false) => {
            let n = if negated {
                months.checked_mul(-1).ok_or_else(error)?
            } else {
                months
            };
            Ok(IntervalValue::YearMonth { months: n })
        }
        (true, true) => {
            let days = delta.num_days();
            let remainder = delta - chrono::Duration::days(days);
            let microseconds = remainder.num_microseconds().ok_or_else(error)?;

            let months = if negated {
                months.checked_mul(-1).ok_or_else(error)?
            } else {
                months
            };
            let days = if negated {
                days.checked_mul(-1).ok_or_else(error)?
            } else {
                days
            };
            let days = i32::try_from(days).map_err(|_| {
                SqlError::invalid(format!("Days value out of range for i32: {days}"))
            })?;
            let microseconds = if negated {
                microseconds.checked_mul(-1).ok_or_else(error)?
            } else {
                microseconds
            };
            let nanoseconds = microseconds * 1_000;

            Ok(IntervalValue::MonthDayNanosecond {
                months,
                days,
                nanoseconds,
            })
        }
        (false, _) => {
            let microseconds = delta.num_microseconds().ok_or_else(error)?;
            let n = if negated {
                microseconds.checked_mul(-1).ok_or_else(error)?
            } else {
                microseconds
            };
            Ok(IntervalValue::Microsecond { microseconds: n })
        }
    }
}

pub(crate) fn parse_unqualified_interval_string(
    s: &str,
    negated: bool,
) -> SqlResult<IntervalValue> {
    // The language of Spark's `stringToInterval`, which reads the unqualified
    // literal and the window gap: multi-unit terms only. The ANSI shapes
    // belong to the qualified casts — `SELECT INTERVAL '1 02:03:04'` is an
    // error in Spark — and are read by [parse_interval_cast_string].
    parse_unqualified_interval_string_fast(s, negated)
        .ok_or_else(|| SqlError::invalid(format!("interval string: {s:?}")))
}

/// Reads a string cast to a qualified interval type. Spark reads the ANSI
/// shapes there (`castStringToDTInterval` / `castStringToYMInterval`); the
/// multi-unit language is also accepted, looser than Spark, because the target
/// qualifier that would refuse it does not reach this far (see the TODO in
/// `spark_interval.rs`).
pub(crate) fn parse_interval_cast_string(s: &str, negated: bool) -> SqlResult<IntervalValue> {
    if let Some(value) = parse_unqualified_interval_string_fast(s, negated) {
        return Ok(value);
    }
    if let Some(value) = parse_ansi_interval_string_fast(s, negated) {
        return Ok(value);
    }
    Err(SqlError::invalid(format!("interval string: {s:?}")))
}

/// A calendar interval with Spark `stringToInterval` bucketing: the unit the
/// user wrote decides the bucket (year/month → `months`, week/day → `days`,
/// sub-day units → `microseconds`); nothing is rebucketed across the day
/// boundary, so a `'1 day'` gap stays calendar and `'25 hours'` stays absolute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CalendarInterval {
    pub months: i32,
    pub days: i32,
    pub microseconds: i64,
}

impl CalendarInterval {
    /// Splits whole days out of the microsecond bucket only when the
    /// microseconds do not fit `i64` nanoseconds; below that bound the
    /// calendar/absolute distinction is preserved exactly.
    pub fn days_and_nanoseconds(&self) -> Option<(i32, i64)> {
        if let Some(nanoseconds) = self.microseconds.checked_mul(1_000) {
            return Some((self.days, nanoseconds));
        }
        const MICROSECONDS_PER_DAY: i64 = 24 * 60 * 60 * 1_000_000;
        let extra_days = i32::try_from(self.microseconds / MICROSECONDS_PER_DAY).ok()?;
        let days = self.days.checked_add(extra_days)?;
        let nanoseconds = (self.microseconds % MICROSECONDS_PER_DAY).checked_mul(1_000)?;
        Some((days, nanoseconds))
    }
}

pub fn parse_calendar_interval_string(s: &str) -> SqlResult<CalendarInterval> {
    parse_calendar_interval_string_fast(s)
        .ok_or_else(|| SqlError::invalid(format!("interval string: {s:?}")))
}

/// Scans the ANSI forms `[+|-]d h:m:s[.f]`, `[+|-]h:m:s[.f]` and `[+|-]y-m`.
///
/// Spark prints these for the qualified interval types and accepts them when a
/// string is cast to one; the multi-unit scanner declines them because none of
/// their words is a unit. Bounds follow Spark: hours 0-23, minutes and seconds
/// 0-59, months 0-11, and exactly one space between the day and the time. A
/// leading sign applies to the whole value, not just the first component.
fn parse_ansi_interval_string_fast(s: &str, negated: bool) -> Option<IntervalValue> {
    // Spark trims every byte up to the space at both ends (`trimAll`), so a
    // non-breaking space is content rather than padding and is refused.
    let s = s.trim_matches(|c: char| c <= '\u{20}');
    let (rendering_negates, s, family) = match strip_interval_rendering(s) {
        Some((rendering_negates, value, family)) => (rendering_negates, value, Some(family)),
        None => (false, s, None),
    };
    let (signed, rest) = match s.as_bytes().first()? {
        b'-' => (true, &s[1..]),
        b'+' => (false, &s[1..]),
        _ => (false, s),
    };
    // Both written signs — before the quote of a rendering and inside the
    // value — are part of the string and narrow with it; the caller's
    // negation applies to the finished value and cannot reach the minimum.
    let negative = signed != rendering_negates;
    if rest.contains(':') {
        if family == Some(AnsiFamily::YearMonth) {
            return None;
        }
        let (days, time) = match rest.split_once(' ') {
            Some((days, time)) => (Some(parse_ansi_component(days)?), time),
            None => (None, rest),
        };
        // Hours are a field of the day only when a day was written; on its own
        // `100:00:00` is a hundred hours, which is what Spark reads for
        // `HOUR TO SECOND`. Which of the two Spark would accept depends on the
        // target qualifier, and Arrow has already lost it by here — every
        // day-time interval is one `Duration(Microsecond)` — so the shape of
        // the string decides, and both shapes are read rather than refused.
        let time = ansi_time_microseconds(time, days.is_some())?;
        let days = days.unwrap_or(0).checked_mul(86_400_000_000)?;
        // Accumulate with the sign rather than negating at the end: the
        // smallest interval Spark prints, `-106751991 04:00:54.775808`, has a
        // magnitude one past `i64::MAX`.
        let mut microseconds = if negative {
            0i64.checked_sub(days)?.checked_sub(time)?
        } else {
            days.checked_add(time)?
        };
        if negated {
            microseconds = microseconds.checked_neg()?;
        }
        Some(IntervalValue::Microsecond { microseconds })
    } else {
        if family == Some(AnsiFamily::DayTime) {
            return None;
        }
        let (years, months) = rest.split_once('-')?;
        let months = parse_ansi_component(months)?;
        if months > 11 {
            return None;
        }
        let total = parse_ansi_component(years)?
            .checked_mul(12)?
            .checked_add(months)?;
        // Negate in `i64` and narrow after, so `-178956970-8` — the smallest
        // year-month interval Spark prints — is reachable.
        let total = if negative {
            0i64.checked_sub(total)?
        } else {
            total
        };
        let mut months = i32::try_from(total).ok()?;
        if negated {
            months = months.checked_neg()?;
        }
        Some(IntervalValue::YearMonth { months })
    }
}

/// The two families an ANSI qualifier can name.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AnsiFamily {
    YearMonth,
    DayTime,
}

/// Unwraps `INTERVAL [+|-]'<value>' <unit> [TO <unit>]`, which is what Spark
/// prints for a qualified interval and reads back when the qualifier names the
/// target type. The qualifier's family has to match the shape of the value,
/// but not the target type, which does not reach this far: Arrow keeps every
/// day-time interval as one `Duration`, so `INTERVAL '12:30:45' HOUR TO
/// SECOND` is read for a `DAY TO SECOND` target where Spark would refuse it.
fn strip_interval_rendering(s: &str) -> Option<(bool, &str, AnsiFamily)> {
    s.get(..8)
        .filter(|head| head.eq_ignore_ascii_case("interval"))?;
    // Spark's pattern is `interval\s+`, so the keyword does not glue to the
    // value, and a sign sits directly before the quote, negating the value:
    // `INTERVAL -'-1-2' YEAR TO MONTH` is a year and two months.
    let rest = &s[8..];
    let trimmed = rest.trim_start_matches(|c: char| c.is_ascii_whitespace());
    if trimmed.len() == rest.len() {
        return None;
    }
    let (negated, trimmed) = match trimmed.as_bytes().first()? {
        b'-' => (true, &trimmed[1..]),
        b'+' => (false, &trimmed[1..]),
        _ => (false, trimmed),
    };
    let value_and_tail = trimmed.strip_prefix('\'')?;
    let (value, tail) = value_and_tail.split_once('\'')?;
    if !tail.starts_with(|c: char| c.is_ascii_whitespace()) {
        return None;
    }
    // The qualifier is a unit, or `unit TO unit` within one family; anything
    // else — a bare `to`, a plural, a unit of the other family — is not one.
    let family = |word: &str| -> Option<AnsiFamily> {
        if ["year", "month"]
            .iter()
            .any(|w| word.eq_ignore_ascii_case(w))
        {
            Some(AnsiFamily::YearMonth)
        } else if ["day", "hour", "minute", "second"]
            .iter()
            .any(|w| word.eq_ignore_ascii_case(w))
        {
            Some(AnsiFamily::DayTime)
        } else {
            None
        }
    };
    let mut words = tail.split_ascii_whitespace();
    let start = family(words.next()?)?;
    if let Some(second_word) = words.next() {
        if !second_word.eq_ignore_ascii_case("to") {
            return None;
        }
        if family(words.next()?)? != start || words.next().is_some() {
            return None;
        }
    }
    Some((negated, value, start))
}

/// Digits only, so a sign inside a component or an empty one is declined.
fn parse_ansi_component(s: &str) -> Option<i64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// `h:m:s[.f]` in microseconds. Minutes and seconds are always fields of the
/// unit above; hours are only when a day was written before them.
fn ansi_time_microseconds(time: &str, hours_are_a_field: bool) -> Option<i64> {
    let mut parts = time.split(':');
    let hours_part = parts.next()?;
    let minutes_part = parts.next()?;
    let seconds_part = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let (seconds_digits, fraction) = match seconds_part.split_once('.') {
        Some((seconds, fraction)) => (seconds, Some(fraction)),
        None => (seconds_part, None),
    };
    // The leading field takes any number of digits in Spark's patterns; every
    // field under it takes one or two. Hours lead only without a day.
    if (hours_are_a_field && hours_part.len() > 2)
        || minutes_part.len() > 2
        || seconds_digits.len() > 2
    {
        return None;
    }
    let hours = parse_ansi_component(hours_part)?;
    let minutes = parse_ansi_component(minutes_part)?;
    let seconds = parse_ansi_component(seconds_digits)?;
    if (hours_are_a_field && hours > 23) || minutes > 59 || seconds > 59 {
        return None;
    }
    // Spark's pattern takes one to nine fraction digits and then truncates to
    // six; a longer one does not match at all.
    if fraction
        .is_some_and(|f| f.is_empty() || f.len() > 9 || !f.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    hours
        .checked_mul(3_600_000_000)?
        .checked_add(minutes.checked_mul(60_000_000)?)?
        .checked_add(seconds.checked_mul(1_000_000)?)?
        .checked_add(fraction_microseconds(fraction)?)
}

/// A scanned interval term: (negated, integer digits, fraction digits, unit).
type IntervalTerm<'a> = (bool, &'a str, Option<&'a str>, Unit);

/// Scans `[interval] [+|-] value unit ([+|-] value unit)*` into terms; a
/// free-standing sign applies to the term that follows, as Spark reads it.
fn scan_interval_terms(s: &str) -> Option<Vec<IntervalTerm<'_>>> {
    // Ends: Spark's `trimAll` (bytes up to the space). Between words: Java
    // whitespace, which adds `\u{0b}` and `\u{1c}`-`\u{1f}` to the ASCII set.
    let s = s.trim_matches(|c: char| c <= '\u{20}');
    let is_separator =
        |c: char| c.is_ascii_whitespace() || c == '\u{0b}' || ('\u{1c}'..='\u{1f}').contains(&c);
    let mut words = s.split(is_separator).filter(|w| !w.is_empty()).peekable();
    if words
        .peek()
        .is_some_and(|w| w.eq_ignore_ascii_case("interval"))
    {
        words.next();
    }
    let mut terms = Vec::new();
    while let Some(word) = words.next() {
        let (separator_negates, value_word) = match word {
            "+" => (Some(false), words.next()?),
            "-" => (Some(true), words.next()?),
            _ => (None, word),
        };
        let (attached, int_part, fraction) = parse_value_word(value_word)?;
        // One sign per term: Spark takes a sign and then wants a digit.
        let negated = match (separator_negates, attached) {
            (Some(_), Some(_)) => return None,
            (Some(sign), None) | (None, Some(sign)) => sign,
            (None, None) => false,
        };
        let unit = parse_unit_word(words.next()?)?;
        if fraction.is_some() && unit != Unit::Second {
            return None;
        }
        terms.push((negated, int_part, fraction, unit));
    }
    Some(terms)
}

/// Per-unit bucketing over [scan_interval_terms].
fn parse_calendar_interval_string_fast(s: &str) -> Option<CalendarInterval> {
    let terms = scan_interval_terms(s)?;
    if terms.is_empty() {
        return None;
    }
    let mut months: i32 = 0;
    let mut days: i32 = 0;
    let mut microseconds: i64 = 0;
    for (neg, int_part, fraction, unit) in terms {
        use Unit::*;
        match unit {
            Year | Month => {
                let value = signed_int_part(neg, int_part)?;
                let value = if matches!(unit, Year) {
                    value.checked_mul(12)?
                } else {
                    value
                };
                months = months.checked_add(i32::try_from(value).ok()?)?;
            }
            Week | Day => {
                let value = signed_int_part(neg, int_part)?;
                let value = if matches!(unit, Week) {
                    value.checked_mul(7)?
                } else {
                    value
                };
                days = days.checked_add(i32::try_from(value).ok()?)?;
            }
            Hour | Minute | Second | Millisecond | Microsecond => {
                // Per-term overflow is an error, as in Spark's `addExact`.
                let value = signed_int_part(neg, int_part)?;
                let unit_microseconds = match unit {
                    Hour => 3_600_000_000,
                    Minute => 60_000_000,
                    Second => 1_000_000,
                    Millisecond => 1_000,
                    _ => 1,
                };
                microseconds = microseconds.checked_add(value.checked_mul(unit_microseconds)?)?;
                if matches!(unit, Second) {
                    let fraction = fraction_microseconds(fraction)?;
                    microseconds = if neg {
                        microseconds.checked_sub(fraction)?
                    } else {
                        microseconds.checked_add(fraction)?
                    };
                }
            }
        }
    }
    Some(CalendarInterval {
        months,
        days,
        microseconds,
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Unit {
    Year,
    Month,
    Week,
    Day,
    Hour,
    Minute,
    Second,
    Millisecond,
    Microsecond,
}

fn parse_unit_word(word: &str) -> Option<Unit> {
    use Unit::*;
    for (names, unit) in [
        (["year", "years"], Year),
        (["month", "months"], Month),
        (["week", "weeks"], Week),
        (["day", "days"], Day),
        (["hour", "hours"], Hour),
        (["minute", "minutes"], Minute),
        (["second", "seconds"], Second),
        (["millisecond", "milliseconds"], Millisecond),
        (["microsecond", "microseconds"], Microsecond),
    ] {
        if names.iter().any(|n| word.eq_ignore_ascii_case(n)) {
            return Some(unit);
        }
    }
    None
}

/// Splits a value word into (written sign, integer digits, fraction digits);
/// either side of the dot may be empty, and whether a fraction is allowed at
/// all is the caller's to check.
fn parse_value_word(word: &str) -> Option<(Option<bool>, &str, Option<&str>)> {
    let (negated, rest) = match word.strip_prefix('-') {
        Some(rest) => (Some(true), rest),
        None => match word.strip_prefix('+') {
            Some(rest) => (Some(false), rest),
            None => (None, word),
        },
    };
    let (int_part, fraction) = match rest.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (rest, None),
    };
    // Some digit has to exist on one side of the dot.
    if int_part.is_empty() && fraction.is_none_or(str::is_empty) {
        return None;
    }
    if !int_part.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // 1-9 digits, then truncated to six; longer does not match Spark at all.
    if let Some(f) = fraction
        && (f.len() > 9 || !f.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    Some((negated, int_part, fraction))
}

/// An absent integer part means zero, so that `'.5 seconds'` scans.
fn parse_int_part<T: std::str::FromStr + Default>(int_part: &str) -> Option<T> {
    if int_part.is_empty() {
        return Some(T::default());
    }
    int_part.parse().ok()
}

/// The magnitude is read as `i64` and the sign applied before any narrowing,
/// so that `'-2147483648 month'` scans: its magnitude does not fit `i32` but
/// the signed value does.
fn signed_int_part(negated: bool, int_part: &str) -> Option<i64> {
    let value: i64 = parse_int_part(int_part)?;
    if negated {
        value.checked_neg()
    } else {
        Some(value)
    }
}

/// Fraction digits of a second to microseconds, as [extract_fraction_match].
fn fraction_microseconds(fraction: Option<&str>) -> Option<i64> {
    match fraction {
        None => Some(0),
        Some(f) => f
            .chars()
            .chain(std::iter::repeat('0'))
            .take(6)
            .collect::<String>()
            .parse::<i64>()
            .ok(),
    }
}

/// Reads the `[interval] (value unit)+` strings and nothing else.
fn parse_unqualified_interval_string_fast(s: &str, negated: bool) -> Option<IntervalValue> {
    let terms = scan_interval_terms(s)?;

    if let [(neg, int_part, fraction, unit)] = terms[..] {
        use Unit::*;
        match unit {
            Year | Month => {
                // The term's sign narrows with the value, so
                // `'-2147483648 month'` reaches `i32::MIN`.
                let value = signed_int_part(neg, int_part)?;
                let months = match unit {
                    Year => value.checked_mul(12)?,
                    _ => value,
                };
                let mut months = i32::try_from(months).ok()?;
                if negated {
                    months = months.checked_neg()?;
                }
                return Some(IntervalValue::YearMonth { months });
            }
            Day | Hour | Minute | Second => {
                // Same reason: `'-9223372036854.775808 seconds'` is `i64::MIN`.
                let value = signed_int_part(neg, int_part)?;
                let unit_microseconds = match unit {
                    Day => 86_400_000_000,
                    Hour => 3_600_000_000,
                    Minute => 60_000_000,
                    _ => 1_000_000,
                };
                let mut microseconds = value.checked_mul(unit_microseconds)?;
                let fraction = fraction_microseconds(fraction)?;
                microseconds = if neg {
                    microseconds.checked_sub(fraction)?
                } else {
                    microseconds.checked_add(fraction)?
                };
                if negated {
                    microseconds = microseconds.checked_neg()?;
                }
                return Some(IntervalValue::Microsecond { microseconds });
            }
            Week | Millisecond | Microsecond => {}
        }
    } else if terms.is_empty() {
        return None;
    }

    let mut months: i32 = 0;
    let mut total: i64 = 0;
    for (neg, int_part, fraction, unit) in &terms {
        use Unit::*;
        match unit {
            Year | Month => {
                let value = signed_int_part(*neg, int_part)?;
                let value = if matches!(unit, Year) {
                    value.checked_mul(12)?
                } else {
                    value
                };
                months = months.checked_add(i32::try_from(value).ok()?)?;
            }
            _ => {
                // Per-term overflow is an error, as in Spark's `addExact`.
                let value = signed_int_part(*neg, int_part)?;
                let unit_microseconds = match unit {
                    Week => 604_800_000_000,
                    Day => 86_400_000_000,
                    Hour => 3_600_000_000,
                    Minute => 60_000_000,
                    Second => 1_000_000,
                    Millisecond => 1_000,
                    _ => 1,
                };
                total = total.checked_add(value.checked_mul(unit_microseconds)?)?;
                if matches!(unit, Second) {
                    let fraction = fraction_microseconds(*fraction)?;
                    total = if *neg {
                        total.checked_sub(fraction)?
                    } else {
                        total.checked_add(fraction)?
                    };
                }
            }
        }
    }
    match (months != 0, total != 0) {
        (true, false) => {
            if negated {
                months = months.checked_neg()?;
            }
            Some(IntervalValue::YearMonth { months })
        }
        (true, true) => {
            const MICROSECONDS_PER_DAY: i64 = 24 * 60 * 60 * 1_000_000;
            let mut days = total / MICROSECONDS_PER_DAY;
            let mut microseconds = total % MICROSECONDS_PER_DAY;
            if negated {
                months = months.checked_neg()?;
                days = days.checked_neg()?;
                microseconds = microseconds.checked_neg()?;
            }
            let days = i32::try_from(days).ok()?;
            Some(IntervalValue::MonthDayNanosecond {
                months,
                days,
                nanoseconds: microseconds * 1_000,
            })
        }
        (false, _) => {
            let mut microseconds = total;
            if negated {
                microseconds = microseconds.checked_neg()?;
            }
            Some(IntervalValue::Microsecond { microseconds })
        }
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::expect_used, clippy::panic)]
    use super::*;

    /// Values and rules taken from JVM Spark 4.x. The plain accept/refuse
    /// matrix lives in the feature file and runs against both engines; what
    /// stays here is what a scenario cannot carry — exact microsecond values,
    /// bytes no Gherkin table holds, and the internal entry itself.
    #[test]
    fn test_ansi_interval_forms() {
        const HOUR: i64 = 3_600_000_000;
        const MINUTE: i64 = 60_000_000;
        const SECOND: i64 = 1_000_000;
        const DAY: i64 = 24 * HOUR;

        let micros = |s: &str| match parse_ansi_interval_string_fast(s, false) {
            Some(IntervalValue::Microsecond { microseconds }) => Some(microseconds),
            _ => None,
        };
        let months = |s: &str| match parse_ansi_interval_string_fast(s, false) {
            Some(IntervalValue::YearMonth { months }) => Some(months),
            _ => None,
        };

        let d1_2_3_4 = DAY + 2 * HOUR + 3 * MINUTE + 4 * SECOND;
        let h2_3_4 = 2 * HOUR + 3 * MINUTE + 4 * SECOND;
        for (input, expected) in [
            ("1 02:03:04", d1_2_3_4),
            // A leading sign covers the whole value, not just the day.
            ("-1 02:03:04", -d1_2_3_4),
            ("+1 02:03:04", d1_2_3_4),
            // Single digits, leading zeros, outer padding, `trimAll` bytes.
            ("1 2:3:4", d1_2_3_4),
            ("001 02:03:04", d1_2_3_4),
            (" 1 02:03:04 ", d1_2_3_4),
            ("\u{b}1 02:03:04", d1_2_3_4),
            ("100 00:00:00", 100 * DAY),
            // Without a day, hours lead and take any width or value.
            ("02:03:04", h2_3_4),
            ("-02:03:04", -h2_3_4),
            ("002:03:04", h2_3_4),
            ("100:00:00", 100 * HOUR),
            ("24:00:00", 24 * HOUR),
            // Fractions truncate to six digits; an explicit zero is fine.
            ("1 02:03:04.5", d1_2_3_4 + 500_000),
            ("1 02:03:04.1234567", d1_2_3_4 + 123_456),
            ("1 02:03:04.123456789", d1_2_3_4 + 123_456),
            ("1 02:03:04.0", d1_2_3_4),
            ("1 23:59:59", DAY + 23 * HOUR + 59 * MINUTE + 59 * SECOND),
            // The rendering Spark prints, read back; case and spacing free.
            ("INTERVAL '1 02:03:04' DAY TO SECOND", d1_2_3_4),
            ("interval  '1 02:03:04'  day to second", d1_2_3_4),
        ] {
            assert_eq!(micros(input), Some(expected), "{input:?}");
        }
        for (input, expected) in [
            ("1-2", 14),
            ("-1-2", -14),
            ("+1-2", 14),
            ("1-11", 23),
            ("INTERVAL '1-2' YEAR TO MONTH", 14),
            // The two written signs multiply.
            ("INTERVAL -'-1-2' YEAR TO MONTH", 14),
            ("INTERVAL -'1-2' YEAR TO MONTH", -14),
            ("INTERVAL +'1-2' YEAR TO MONTH", 14),
        ] {
            assert_eq!(months(input), Some(expected), "{input:?}");
        }
        // Refused: bytes and shapes the feature file cannot carry, and the
        // rendering rules. The field-range and spacing refusals run there
        // instead, on both engines.
        for input in [
            "1 002:03:04", // non-leading fields take one or two digits
            "1 02:003:04",
            "1 02:03:004",
            "1 +2:03:04",       // a sign is not a digit
            "\u{a0}1 02:03:04", // NBSP is content, not padding
            "1",
            "1 -02:03:04",
            "1 02:03:.5",
            "1 02:03:04.x",
            "1 - 2",
            "INTERVAL '1 02:03:04'",      // a rendering needs its qualifier
            "'1 02:03:04' DAY TO SECOND", // and the keyword
            "INTERVAL '1 02:03:04' DAY TO FORTNIGHT",
            "INTERVAL'1 02:03:04' DAY TO SECOND", // and its space
            "INTERVAL '1-2' to",
            "INTERVAL '1-2' DAY TO SECOND", // the family has to fit the shape
            "INTERVAL '02:03:04' YEAR TO MONTH",
            "INTERVAL '1-2' YEAR TO SECOND", // one family per pair
            "INTERVAL '1-2' WEEK",
            "5 seconds", // the multi-unit forms stay with their own scanner
        ] {
            assert_eq!(
                parse_ansi_interval_string_fast(input, false),
                None,
                "{input:?}"
            );
        }
    }

    /// Signs in the multi-unit form, taken from JVM Spark's `stringToInterval`.
    #[test]
    fn test_ansi_forms_are_not_multi_unit() {
        // The two scanners must not overlap: an ANSI string is not a sequence
        // of value-unit pairs, and a multi-unit string has no colon or dash to
        // mistake for one.
        for s in ["1 02:03:04", "02:03:04", "1-2", "-1 02:03:04"] {
            assert!(
                parse_calendar_interval_string_fast(s).is_none(),
                "{s:?} is ANSI, not multi-unit"
            );
            assert!(
                parse_ansi_interval_string_fast(s, false).is_some(),
                "{s:?} should be scanned as ANSI"
            );
        }
        for s in ["5 seconds", "1 day 2 hour", "interval 5 minutes"] {
            assert!(
                parse_ansi_interval_string_fast(s, false).is_none(),
                "{s:?} is multi-unit, not ANSI"
            );
        }
    }

    #[test]
    fn test_parse_interval() -> SqlResult<()> {
        let parse = parse_unqualified_interval_string;
        // The cast language is wider: it also reads the ANSI shapes, which the
        // unqualified literal refuses the way Spark's `stringToInterval` does.
        let cast = parse_interval_cast_string;

        assert!(parse("178956970 year 7 month", false).is_ok());
        assert!(parse("178956970 year 7 month", true).is_ok());
        assert!(parse("178956970 year 8 month", false).is_err());
        assert!(parse("178956970 year 8 month", true).is_err());
        assert!(parse("-178956970 year -8 month", false).is_ok());
        assert!(parse("-178956970 year -8 month", true).is_err());
        assert!(parse("-178956970 year -9 month", false).is_err());
        assert!(parse("-178956970 year -9 month", true).is_err());

        // The ANSI shapes are the cast language only: `SELECT INTERVAL
        // '1 02:03:04'` is an error in Spark, `CAST` to a qualified type reads
        // it.
        assert!(parse("178956970-7", false).is_err());
        assert!(parse("1 02:03:04", false).is_err());
        assert!(cast("178956970-7", false).is_ok());
        assert!(cast("178956970-7", true).is_ok());
        assert!(cast("178956970-8", false).is_err());
        // The smallest year-month interval Spark prints. It is reachable only
        // through its own written sign: the sign inside the string narrows
        // with the value, and the caller's negation applies to the finished
        // value, so negating the positive magnitude is an error, not the
        // minimum.
        assert_eq!(
            cast("-178956970-8", false)?,
            IntervalValue::YearMonth { months: i32::MIN }
        );
        assert!(cast("178956970-8", true).is_err());
        assert!(cast("-178956970-9", false).is_err());

        assert_eq!(cast("-2-1", false)?, cast("2-1", true)?);
        assert_eq!(cast("-2-1", false)?, parse("-2 year -1 month", false)?);

        assert!(parse("106751991 day 14454775807 microsecond", false).is_ok());
        assert!(parse("106751991 day 14454775807 microsecond", true).is_ok());
        assert!(parse("106751991 day 14454775808 microsecond", false).is_err());
        assert!(parse("106751991 day 14454775808 microsecond", true).is_err());
        assert!(parse("-106751991 day -14454775808 microsecond", false).is_ok());
        assert!(parse("-106751991 day -14454775808 microsecond", true).is_err());
        assert!(parse("-106751991 day -14454775809 microsecond", false).is_err());
        assert!(parse("-106751991 day -14454775809 microsecond", true).is_err());

        assert_eq!(
            cast("-106751991 04:00:54.775808", false)?,
            IntervalValue::Microsecond {
                microseconds: i64::MIN
            }
        );
        assert!(cast("106751991 04:00:54.775808", false).is_err());
        assert!(cast("106751991 04:00:54.775807", false).is_ok());
        // A single term reaches the same minimum through its own sign.
        assert_eq!(
            parse("-9223372036854.775808 seconds", false)?,
            IntervalValue::Microsecond {
                microseconds: i64::MIN
            }
        );
        assert!(parse("9223372036854.775808 seconds", false).is_err());
        assert!(parse("-9223372036854.775808 seconds", true).is_err());

        assert_eq!(
            cast("-1 2:3:4.567890", false)?,
            cast("1 2:3:4.567890", true)?
        );
        assert_eq!(
            cast("-1 2:3:4.567890", false)?,
            parse(
                "-1 day -2 hour -3 minute -4 second -567 millisecond -890 microsecond",
                false
            )?
        );
        // Per-term overflow is an error, as with Spark's `addExact`.
        assert!(
            parse(
                "9223372036854775807 microseconds 1 microsecond -1 microsecond",
                false
            )
            .is_err()
        );
        Ok(())
    }

    /// The function behind every divergence from Spark on huge sub-day
    /// amounts, so pin what it does.
    #[test]
    fn test_days_and_nanoseconds() {
        const MICROS_PER_DAY: i64 = 24 * 60 * 60 * 1_000_000;
        let split = |days: i32, microseconds: i64| {
            CalendarInterval {
                months: 0,
                days,
                microseconds,
            }
            .days_and_nanoseconds()
        };

        // Below the bound nothing moves.
        assert_eq!(split(1, 0), Some((1, 0)));
        assert_eq!(split(0, 5), Some((0, 5_000)));
        assert_eq!(split(-1, -5), Some((-1, -5_000)));
        assert_eq!(split(2, MICROS_PER_DAY), Some((2, MICROS_PER_DAY * 1_000)));

        // Past it the microseconds are split, and the remainder keeps the sign.
        let huge = i64::MAX / 1_000 + 1;
        let (days, nanoseconds) = split(0, huge).expect("splits");
        assert_eq!(i64::from(days), huge / MICROS_PER_DAY);
        assert_eq!(nanoseconds, (huge % MICROS_PER_DAY) * 1_000);
        let (days, nanoseconds) = split(0, -huge).expect("splits");
        assert_eq!(i64::from(days), -(huge / MICROS_PER_DAY));
        assert_eq!(nanoseconds, -((huge % MICROS_PER_DAY) * 1_000));

        // Days already present are added to, not replaced.
        let (days, _) = split(10, huge).expect("splits");
        assert_eq!(i64::from(days), 10 + huge / MICROS_PER_DAY);

        // A day count that cannot hold the split is refused, not wrapped.
        assert_eq!(split(i32::MAX, huge), None);
    }

    /// Both readers run different arms over the same terms, so a weight is
    /// only pinned when both agree on it.
    #[test]
    fn test_each_unit_weighs_what_it_says() {
        const HOUR: i64 = 3_600_000_000;
        let calendar = |s: &str| parse_calendar_interval_string_fast(s).expect("scans");
        let value = |s: &str| parse_unqualified_interval_string(s, false).expect("reads");
        let micros = |s: &str| match value(s) {
            IntervalValue::Microsecond { microseconds } => microseconds,
            other => panic!("{s:?} should be an absolute amount, got {other:?}"),
        };
        let months = |s: &str| match value(s) {
            IntervalValue::YearMonth { months } => months,
            other => panic!("{s:?} should be months, got {other:?}"),
        };

        for (input, microseconds) in [
            ("1 microsecond", 1),
            ("1 millisecond", 1_000),
            ("1 second", 1_000_000),
            ("1 minute", 60_000_000),
            ("1 hour", HOUR),
        ] {
            assert_eq!(calendar(input).microseconds, microseconds, "{input:?}");
            assert_eq!(micros(input), microseconds, "{input:?}");
        }
        assert_eq!(calendar("1 day").days, 1);
        assert_eq!(micros("1 day"), 24 * HOUR);
        assert_eq!(calendar("1 week").days, 7);
        assert_eq!(micros("1 week"), 7 * 24 * HOUR);
        assert_eq!(calendar("1 month").months, 1);
        assert_eq!(months("1 month"), 1);
        assert_eq!(calendar("1 year").months, 12);
        assert_eq!(months("1 year"), 12);

        // A day is calendar and an hour is absolute, so they do not merge.
        let both = calendar("1 day 1 hour");
        assert_eq!((both.days, both.microseconds), (1, HOUR));

        // Months and a time together are the third variant.
        match value("1 month 1 day 2 hour") {
            IntervalValue::MonthDayNanosecond {
                months,
                days,
                nanoseconds,
            } => assert_eq!((months, days, nanoseconds), (1, 1, 2 * HOUR * 1_000)),
            other => panic!("expected months, days and a time, got {other:?}"),
        }

        // Two signs that cancel combine, one does not win over the other.
        assert_eq!(
            parse_unqualified_interval_string("-1 hour", true).expect("reads"),
            IntervalValue::Microsecond { microseconds: HOUR }
        );

        assert!(parse_unqualified_interval_string("1.123456789 second", false).is_ok());
        assert!(parse_unqualified_interval_string("1.1234567890 second", false).is_err());
    }

    /// The two written signs of a qualified literal combine:
    /// `INTERVAL -'-1-2' YEAR TO MONTH` is positive.
    #[test]
    fn test_qualified_literal_signs_combine() -> SqlResult<()> {
        const DAY: i64 = 86_400_000_000;
        let months = |s: &str, negated: bool| {
            parse_interval_year_month_string(s, negated, &INTERVAL_YEAR_TO_MONTH_REGEX)
        };
        let micros = |s: &str, negated: bool| {
            parse_interval_day_time_string(s, negated, &INTERVAL_DAY_TO_SECOND_REGEX)
        };

        for (input, negated, expected) in [
            ("1-2", false, 14),
            ("-1-2", false, -14),
            ("1-2", true, -14),
            ("-1-2", true, 14),
        ] {
            assert_eq!(
                months(input, negated)?,
                IntervalValue::YearMonth { months: expected },
                "{input:?} negated={negated}"
            );
        }

        for (input, negated, expected) in [
            ("1 00:00:00", false, DAY),
            ("-1 00:00:00", false, -DAY),
            ("1 00:00:00", true, -DAY),
            ("-1 00:00:00", true, DAY),
        ] {
            assert_eq!(
                micros(input, negated)?,
                IntervalValue::Microsecond {
                    microseconds: expected
                },
                "{input:?} negated={negated}"
            );
        }

        assert!(!"1.5".parse::<Signed<DecimalSecond>>()?.is_negative());
        assert!("-1.5".parse::<Signed<DecimalSecond>>()?.is_negative());
        Ok(())
    }

    /// Values from JVM Spark; the gold corpus never writes a sign between
    /// the keyword and the value, so that syntax is pinned here.
    #[test]
    fn test_qualified_literal_forms() -> SqlResult<()> {
        let literal = |sql: &str| -> SqlResult<spec::Literal> {
            match crate::expression::from_ast_expression(crate::parser::parse_expression(sql)?)? {
                spec::Expr::Literal(value) => Ok(value),
                other => panic!("{sql} did not read as a literal: {other:?}"),
            }
        };
        let months = |months: i32| spec::Literal::IntervalYearMonth {
            months: Some(months),
        };
        let micros = |microseconds: i64| spec::Literal::DurationMicrosecond {
            microseconds: Some(microseconds),
        };

        for (sql, expected) in [
            ("INTERVAL '5' YEAR", months(60)),
            ("INTERVAL ' 5 ' YEAR", months(60)),
            ("INTERVAL '5' MONTH", months(5)),
            ("INTERVAL ' 5 ' MONTH", months(5)),
            ("INTERVAL '1-2' YEAR TO MONTH", months(14)),
            ("INTERVAL ' 1-2 ' YEAR TO MONTH", months(14)),
            ("INTERVAL '5' DAY", micros(432_000_000_000)),
            ("INTERVAL ' 5 ' DAY", micros(432_000_000_000)),
            ("INTERVAL '5' HOUR", micros(18_000_000_000)),
            ("INTERVAL ' 5 ' HOUR", micros(18_000_000_000)),
            ("INTERVAL '5' MINUTE", micros(300_000_000)),
            ("INTERVAL ' 5 ' MINUTE", micros(300_000_000)),
            ("INTERVAL '5' SECOND", micros(5_000_000)),
            ("INTERVAL ' 5 ' SECOND", micros(5_000_000)),
            ("INTERVAL '5.5' SECOND", micros(5_500_000)),
            // A second count past `u32::MAX`; Spark reads it too.
            (
                "INTERVAL '4294967296' SECOND",
                micros(4_294_967_296_000_000),
            ),
            (
                "INTERVAL '-4294967296' SECOND",
                micros(-4_294_967_296_000_000),
            ),
            ("INTERVAL '1 02' DAY TO HOUR", micros(93_600_000_000)),
            ("INTERVAL '1 02:03' DAY TO MINUTE", micros(93_780_000_000)),
            (
                "INTERVAL '1 02:03:04' DAY TO SECOND",
                micros(93_784_000_000),
            ),
            (
                "INTERVAL '1 23:59:59' DAY TO SECOND",
                micros(172_799_000_000),
            ),
            ("INTERVAL '1-11' YEAR TO MONTH", months(23)),
            ("INTERVAL '02:03' HOUR TO MINUTE", micros(7_380_000_000)),
            ("INTERVAL '02:03:04' HOUR TO SECOND", micros(7_384_000_000)),
            ("INTERVAL '03:04' MINUTE TO SECOND", micros(184_000_000)),
            ("INTERVAL -'1-2' YEAR TO MONTH", months(-14)),
            ("INTERVAL +'1-2' YEAR TO MONTH", months(14)),
            ("INTERVAL - '1-2' YEAR TO MONTH", months(-14)),
            ("INTERVAL -'-1-2' YEAR TO MONTH", months(14)),
            ("INTERVAL -'5' DAY", micros(-432_000_000_000)),
            ("INTERVAL -'-5' DAY", micros(432_000_000_000)),
            // A sign belongs to its own term: `-'5' WEEK '1' DAY` is -34 days.
            ("INTERVAL 1 YEAR 2 MONTH", months(14)),
            ("INTERVAL 1 DAY 2 HOUR", micros(93_600_000_000)),
            ("INTERVAL -'5' WEEK '1' DAY", micros(-2_937_600_000_000)),
        ] {
            assert_eq!(literal(sql)?, expected, "{sql}");
        }

        // A fraction belongs to seconds only.
        assert!(literal("INTERVAL '5.5' DAY").is_err());

        // The range, width, and fraction refusals run in the feature file;
        // here stay the rows no scenario covers.
        for sql in [
            "INTERVAL '1 02:03:04.' DAY TO SECOND",
            "INTERVAL '03:60' MINUTE TO SECOND",
        ] {
            assert!(literal(sql).is_err(), "{sql}");
        }
        assert_eq!(literal("INTERVAL '100' HOUR")?, micros(360_000_000_000));
        // The minimum of each type is reachable through the written sign.
        assert_eq!(
            literal("INTERVAL '-106751991 04:00:54.775808' DAY TO SECOND")?,
            micros(i64::MIN)
        );
        assert_eq!(
            literal("INTERVAL '-178956970-8' YEAR TO MONTH")?,
            months(i32::MIN)
        );
        assert_eq!(
            literal("INTERVAL '-9223372036854.775808' SECOND")?,
            micros(i64::MIN)
        );

        // Spark 4 refuses to mix families in a literal, so this pins Sail's
        // own behaviour, not parity.
        assert_eq!(
            literal("INTERVAL 1 MONTH 2 DAY 3 HOUR")?,
            spec::Literal::IntervalMonthDayNano {
                value: Some(spec::IntervalMonthDayNano {
                    months: 1,
                    days: 2,
                    nanoseconds: 10_800_000_000_000,
                }),
            }
        );
        Ok(())
    }

    #[test]
    fn test_multi_unit_accepted_set() {
        // Verdicts from JVM Spark's `stringToInterval`.
        let accepted = [
            "5 minutes",
            "1.5 seconds",
            "-1.5 seconds",
            "1. seconds",
            ".5 seconds",
            "-1. seconds",
            "0.000001 seconds",
            "1.1234567 seconds",
            "1 month",
            "-0 month",
            "3 years",
            "1 week",
            "-2 weeks",
            "10 milliseconds",
            "7 microseconds",
            "1 month 2 days",
            "-1 hour -2 seconds",
            "1 year 2 months 3 days 4 hours 5 minutes 6.789 seconds",
            "1 day -2 hours",
            "interval 5 minutes",
            "  5   MINUTES  ",
            "007 days",
            "2147483647 months",
            "178956970 year 7 month",
            "106751991 day 14454775807 microsecond",
            "5000000000 seconds",
            "+5 minutes",
            "- 5 minutes",
            "1 day + 2 hour",
            "1 day - 2 hour",
            // A month count that only fits once the sign is applied.
            "-2147483648 month",
            "1 day 5000000000 seconds",
            "1 day .5 seconds",
            // Java whitespace between words, `trimAll` bytes at the ends.
            "\u{b}1 day",
            "1\u{b}day",
            "1\u{1c}day",
            "1 day\u{0}",
        ];
        let refused = [
            // A dot goes with seconds and nothing else.
            "1. days",
            "1.5 days",
            "1. minute",
            // Nine fraction digits is the limit on both shapes.
            "1.1234567890 seconds",
            ".",
            // Overflow.
            "178956970 year 8 month",
            // Shapes Spark's `stringToInterval` does not read.
            "'5' minutes",
            "'1 1' day to hour",
            "5",
            "minutes",
            "5 fortnights",
            "1e3 days",
            "1day",
            "1 day+2 hour",
            // One sign per term.
            "- -5 minutes",
            "- +5 minutes",
            "1 day - -2 hour",
            "1 day + +2 hour",
            "",
        ];
        // Both entry points: a single term takes its own path, so asserting
        // one of them once hid a divergence between the two.
        for s in accepted {
            assert!(
                parse_calendar_interval_string(s).is_ok(),
                "{s:?} should be read as a calendar interval"
            );
            assert!(
                parse_unqualified_interval_string(s, false).is_ok(),
                "{s:?} should be read as an interval value"
            );
        }
        for s in refused {
            assert!(
                parse_calendar_interval_string(s).is_err(),
                "{s:?} should be refused as a calendar interval"
            );
            assert!(
                parse_unqualified_interval_string(s, false).is_err(),
                "{s:?} should be refused as an interval value"
            );
        }
    }

    #[test]
    fn test_parse_unqualified_interval_string() -> SqlResult<()> {
        assert!(parse_unqualified_interval_string("1", false).is_err());
        assert!(parse_unqualified_interval_string("1 month", false).is_ok());
        assert_eq!(
            parse_unqualified_interval_string("1 month", true)?,
            parse_unqualified_interval_string("-1 month", false)?
        );
        assert_eq!(
            parse_unqualified_interval_string("1 hour 2 seconds", false)?,
            parse_unqualified_interval_string("-1 hour -2 seconds", true)?
        );
        Ok(())
    }

    /// The unit written decides the bucket; nothing is rebucketed across the
    /// day boundary.
    #[test]
    fn test_calendar_interval_bucketing() -> SqlResult<()> {
        const HOUR: i64 = 3_600_000_000;
        for (s, months, days, micros) in [
            ("1 day", 0, 1, 0),
            ("interval 1 day", 0, 1, 0),
            ("25 hours", 0, 0, 25 * HOUR),
            ("1 day 2 hours", 0, 1, 2 * HOUR),
            ("2 weeks", 0, 14, 0),
            ("-2 days", 0, -2, 0),
            ("1 month -30 days", 1, -30, 0),
            ("1 month 25 hours", 1, 0, 25 * HOUR),
            ("1.5 seconds", 0, 0, 1_500_000),
            ("-1.5 seconds", 0, 0, -1_500_000),
            ("1 year 1 microsecond", 12, 0, 1),
        ] {
            let v = parse_calendar_interval_string(s)?;
            assert_eq!(
                (v.months, v.days, v.microseconds),
                (months, days, micros),
                "{s}"
            );
        }
        assert!(parse_calendar_interval_string("garbage").is_err());
        Ok(())
    }
}
