use std::fmt::Debug;
use std::sync::Arc;

use datafusion::arrow::datatypes::{
    ArrowPrimitiveType, DataType, DurationMicrosecondType, IntervalMonthDayNano, IntervalUnit,
    IntervalYearMonthType, TimeUnit,
};
use datafusion_common::arrow::array::{AsArray, PrimitiveArray};
use datafusion_common::arrow::datatypes::IntervalMonthDayNanoType;
use datafusion_common::cast::{as_large_string_array, as_string_array, as_string_view_array};
use datafusion_common::types::logical_string;
use datafusion_common::{Result, ScalarValue, exec_datafusion_err, exec_err};
use datafusion_expr::{ColumnarValue, ScalarFunctionArgs, ScalarUDFImpl, Signature, Volatility};
use datafusion_expr_common::signature::{Coercion, TypeSignatureClass};
use sail_common::spec;
use sail_common_datafusion::utils::items::ItemTaker;
use sail_sql_analyzer::literal::interval::{IntervalValue, parse_calendar_interval_string};
use sail_sql_analyzer::parser::{parse_day_time_interval_cast, parse_year_month_interval_cast};

use crate::functions_utils::StrMemo;

/// Parses interval strings, each distinct value once per batch; under
/// `is_try` the memo keeps `Option`, so an unreadable string is remembered
/// as NULL instead of being re-read for every row.
fn parse_memoized<'a, P, F>(
    values: impl Iterator<Item = Option<&'a str>>,
    parse: F,
    is_try: bool,
) -> Result<PrimitiveArray<P>>
where
    P: ArrowPrimitiveType,
    F: Fn(&str) -> Result<P::Native>,
{
    if is_try {
        let mut memo: StrMemo<'a, Option<P::Native>> = StrMemo::new();
        return values
            .map(|value| {
                value
                    .map(|s| {
                        memo.get_or_try_insert_ref(s, |s| Ok(parse(s).ok()))
                            .copied()
                    })
                    .transpose()
                    .map(Option::flatten)
            })
            .collect();
    }
    let mut memo: StrMemo<'a, P::Native> = StrMemo::new();
    values
        .map(|value| {
            value
                .map(|s| memo.get_or_try_insert_ref(s, &parse).copied())
                .transpose()
        })
        .collect()
}

macro_rules! define_interval_udf {
    (
        $udf:ident,
        $name:expr_2021,
        $return_type:expr_2021,
        $primitive_type:ty,
        $scalar:expr_2021,
        { $($field:ident: $ftype:ty),* $(,)? },
        $make_parse:expr_2021 $(,)?
    ) => {
        #[derive(Debug, PartialEq, Eq, Hash)]
        pub struct $udf {
            signature: Signature,
            is_try: bool,
            $($field: $ftype,)*
        }

        impl $udf {
            pub fn new(is_try: bool $(, $field: $ftype)*) -> Self {
                Self {
                    is_try,
                    $($field,)*
                    signature: Signature::coercible(
                        vec![Coercion::new_exact(TypeSignatureClass::Native(
                            logical_string(),
                        ))],
                        Volatility::Immutable,
                    ),
                }
            }
        }

        impl $udf {
            /// `TRY_CAST` yields NULL where `CAST` raises, as Spark does for
            /// every interval target, and a plain cast to the calendar type
            /// does the same because Spark reads it with `safeStringToInterval`.
            pub fn is_try(&self) -> bool {
                self.is_try
            }

            $(
                pub fn $field(&self) -> $ftype {
                    self.$field
                }
            )*
        }

        impl ScalarUDFImpl for $udf {
            fn name(&self) -> &str {
                $name
            }

            fn signature(&self) -> &Signature {
                &self.signature
            }

            fn return_type(&self, _arg_types: &[DataType]) -> Result<DataType> {
                Ok($return_type)
            }

            fn invoke_with_args(&self, args: ScalarFunctionArgs) -> Result<ColumnarValue> {
                let ScalarFunctionArgs { args, .. } = args;
                let arg = args.one()?;
                let is_try = self.is_try;
                let parse = ($make_parse)(self);
                match arg {
                    ColumnarValue::Array(array) => {
                        let array: PrimitiveArray<$primitive_type> = match array.data_type() {
                            DataType::Utf8 => {
                                parse_memoized(as_string_array(&array)?.iter(), &parse, is_try)?
                            }
                            DataType::LargeUtf8 => parse_memoized(
                                as_large_string_array(&array)?.iter(),
                                &parse,
                                is_try,
                            )?,
                            DataType::Utf8View => {
                                parse_memoized(as_string_view_array(&array)?.iter(), &parse, is_try)?
                            }
                            _ => return exec_err!("expected string array for intervals"),
                        };
                        Ok(ColumnarValue::Array(Arc::new(array)))
                    }
                    ColumnarValue::Scalar(scalar) => {
                        let value = match scalar.try_as_str() {
                            Some(x) => match x.map(|x| parse(x)) {
                                Some(Err(_)) if is_try => None,
                                other => other.transpose()?,
                            },
                            _ => return exec_err!("expected string scalar for intervals"),
                        };
                        Ok(ColumnarValue::Scalar($scalar(value)))
                    }
                }
            }
        }
    };
}

define_interval_udf!(
    SparkYearMonthInterval,
    "spark_year_month_interval",
    DataType::Interval(IntervalUnit::YearMonth),
    IntervalYearMonthType,
    ScalarValue::IntervalYearMonth,
    {
        start: spec::YearMonthIntervalField,
        end: spec::YearMonthIntervalField,
    },
    |udf: &SparkYearMonthInterval| {
        let (start, end) = (udf.start, udf.end);
        move |s: &str| string_to_year_month_interval(s, start, end)
    },
);

define_interval_udf!(
    SparkDayTimeInterval,
    "spark_day_time_interval",
    DataType::Duration(TimeUnit::Microsecond),
    DurationMicrosecondType,
    ScalarValue::DurationMicrosecond,
    {
        start: spec::DayTimeIntervalField,
        end: spec::DayTimeIntervalField,
    },
    |udf: &SparkDayTimeInterval| {
        let (start, end) = (udf.start, udf.end);
        move |s: &str| string_to_day_time_interval(s, start, end)
    },
);

define_interval_udf!(
    SparkCalendarInterval,
    "spark_calendar_interval",
    DataType::Interval(IntervalUnit::MonthDayNano),
    IntervalMonthDayNanoType,
    ScalarValue::IntervalMonthDayNano,
    {},
    |_: &SparkCalendarInterval| string_to_calendar_interval,
);

#[derive(Debug, PartialEq, Eq, Hash)]
pub struct SparkDayTimeIntervalToCalendarInterval {
    signature: Signature,
}

impl Default for SparkDayTimeIntervalToCalendarInterval {
    fn default() -> Self {
        Self::new()
    }
}

impl SparkDayTimeIntervalToCalendarInterval {
    pub fn new() -> Self {
        Self {
            signature: Signature::exact(
                vec![DataType::Duration(TimeUnit::Microsecond)],
                Volatility::Immutable,
            ),
        }
    }
}

impl ScalarUDFImpl for SparkDayTimeIntervalToCalendarInterval {
    fn name(&self) -> &str {
        "spark_day_time_interval_to_calendar_interval"
    }

    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn return_type(&self, _arg_types: &[DataType]) -> Result<DataType> {
        Ok(DataType::Interval(IntervalUnit::MonthDayNano))
    }

    fn invoke_with_args(&self, args: ScalarFunctionArgs) -> Result<ColumnarValue> {
        let ScalarFunctionArgs { args, .. } = args;
        let arg = args.one()?;
        match arg {
            ColumnarValue::Array(array) => {
                let array = match array.data_type() {
                    DataType::Duration(TimeUnit::Microsecond) => array
                        .as_primitive::<DurationMicrosecondType>()
                        .iter()
                        .map(|value| {
                            value
                                .map(day_time_interval_to_calendar_interval)
                                .transpose()
                        })
                        .collect::<Result<PrimitiveArray<IntervalMonthDayNanoType>>>()?,
                    data_type => {
                        return exec_err!(
                            "expected microsecond day-time interval, got {data_type}"
                        );
                    }
                };
                Ok(ColumnarValue::Array(Arc::new(array)))
            }
            ColumnarValue::Scalar(ScalarValue::DurationMicrosecond(value)) => {
                let value = value
                    .map(day_time_interval_to_calendar_interval)
                    .transpose()?;
                Ok(ColumnarValue::Scalar(ScalarValue::IntervalMonthDayNano(
                    value,
                )))
            }
            value => exec_err!("expected microsecond day-time interval, got {value:?}"),
        }
    }
}

// The target qualifier travels with the UDF, so a string is read in exactly
// the shape of its target even though Arrow keeps one physical type per
// family; the declared type still erases the fields.

fn string_to_year_month_interval(
    value: &str,
    start: spec::YearMonthIntervalField,
    end: spec::YearMonthIntervalField,
) -> Result<i32> {
    let interval = parse_year_month_interval_cast(value, start, end)
        .map_err(|e| exec_datafusion_err!("{e}"))?;
    match interval {
        IntervalValue::YearMonth { months } => Ok(months),
        IntervalValue::Microsecond { .. } | IntervalValue::MonthDayNanosecond { .. } => {
            exec_err!("expected year month interval, but got: {value}")
        }
    }
}

fn string_to_day_time_interval(
    value: &str,
    start: spec::DayTimeIntervalField,
    end: spec::DayTimeIntervalField,
) -> Result<i64> {
    let interval =
        parse_day_time_interval_cast(value, start, end).map_err(|e| exec_datafusion_err!("{e}"))?;
    match interval {
        IntervalValue::Microsecond { microseconds } => Ok(microseconds),
        IntervalValue::YearMonth { .. } | IntervalValue::MonthDayNanosecond { .. } => {
            exec_err!("expected day time interval, but got: {value}")
        }
    }
}

fn string_to_calendar_interval(value: &str) -> Result<IntervalMonthDayNano> {
    // Spark bucketing: the unit the user wrote decides the bucket; sub-day
    // amounts stay absolute microseconds and are never rebucketed into days.
    let interval =
        parse_calendar_interval_string(value).map_err(|e| exec_datafusion_err!("{e}"))?;
    let (days, nanoseconds) = interval
        .days_and_nanoseconds()
        .ok_or_else(|| exec_datafusion_err!("interval out of range: {value:?}"))?;
    Ok(IntervalMonthDayNano {
        months: interval.months,
        days,
        nanoseconds,
    })
}

fn day_time_interval_to_calendar_interval(microseconds: i64) -> Result<IntervalMonthDayNano> {
    const MICROSECONDS_PER_DAY: i64 = 24 * 60 * 60 * 1_000_000;

    let days = i32::try_from(microseconds / MICROSECONDS_PER_DAY).map_err(|_| {
        exec_datafusion_err!("microseconds overflow for calendar interval: {microseconds}")
    })?;
    Ok(IntervalMonthDayNano {
        months: 0,
        days,
        nanoseconds: microseconds % MICROSECONDS_PER_DAY * 1_000,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_memoized_computes_distinct_values_and_propagates_errors() -> Result<()> {
        use datafusion_common::arrow::array::Array;

        let values = [Some("5 minutes"), None, Some("5 minutes"), Some("1 month")];
        let array: PrimitiveArray<IntervalMonthDayNanoType> =
            parse_memoized(values.into_iter(), string_to_calendar_interval, false)?;
        assert_eq!(array.len(), 4);
        assert_eq!(
            array.value(0),
            IntervalMonthDayNano::new(0, 0, 300_000_000_000)
        );
        assert!(array.is_null(1));
        assert_eq!(array.value(2), array.value(0));
        assert_eq!(array.value(3), IntervalMonthDayNano::new(1, 0, 0));

        let invalid: Result<PrimitiveArray<IntervalMonthDayNanoType>> = parse_memoized(
            [Some("### nonsense")].into_iter(),
            string_to_calendar_interval,
            false,
        );
        assert!(invalid.is_err());

        // The same string under `TRY_CAST` is NULL rather than an error, and
        // the memo keeps that answer like any other.
        let lenient: PrimitiveArray<IntervalMonthDayNanoType> = parse_memoized(
            [
                Some("### nonsense"),
                Some("5 minutes"),
                Some("### nonsense"),
            ]
            .into_iter(),
            string_to_calendar_interval,
            true,
        )?;
        assert!(lenient.is_null(0));
        assert_eq!(
            lenient.value(1),
            IntervalMonthDayNano::new(0, 0, 300_000_000_000)
        );
        assert!(lenient.is_null(2));
        Ok(())
    }

    /// A sub-day amount too large for i64 nanoseconds splits whole days out
    /// (Spark's microsecond-based CalendarInterval still represents it);
    /// below that bound the absolute bucket is preserved exactly.
    #[test]
    fn calendar_interval_nanosecond_overflow_splits_days() -> Result<()> {
        // 3000000 hours = 125000 days; fits i64 µs but not ns.
        let v = string_to_calendar_interval("3000000 hours")?;
        assert_eq!((v.months, v.days, v.nanoseconds), (0, 125_000, 0));
        // 2000000 hours fits ns: stays absolute, no day splitting.
        let v = string_to_calendar_interval("2000000 hours")?;
        assert_eq!(
            (v.months, v.days, v.nanoseconds),
            (0, 0, 2_000_000i64 * 3_600 * 1_000_000_000)
        );
        Ok(())
    }

    #[test]
    fn string_parsers_read_exactly_the_target_shape() -> Result<()> {
        use spec::{DayTimeIntervalField as Dt, YearMonthIntervalField as Ym};
        assert_eq!(
            string_to_year_month_interval("2-0", Ym::Year, Ym::Month)?,
            24
        );
        // The multi-unit language belongs to the calendar type only.
        assert!(string_to_year_month_interval("2 years", Ym::Year, Ym::Month).is_err());
        assert_eq!(
            string_to_day_time_interval("5", Dt::Minute, Dt::Minute)?,
            300_000_000
        );
        assert!(string_to_day_time_interval("5 minutes", Dt::Day, Dt::Second).is_err());
        assert_eq!(
            string_to_calendar_interval("1 month 2 days")?,
            IntervalMonthDayNano::new(1, 2, 0)
        );
        Ok(())
    }

    #[test]
    fn day_time_interval_preserves_calendar_days_and_microsecond_remainder() -> Result<()> {
        const MICROSECONDS_PER_DAY: i64 = 24 * 60 * 60 * 1_000_000;

        assert_eq!(
            day_time_interval_to_calendar_interval(MICROSECONDS_PER_DAY + 5)?,
            IntervalMonthDayNano::new(0, 1, 5_000)
        );
        assert_eq!(
            day_time_interval_to_calendar_interval(-MICROSECONDS_PER_DAY - 5)?,
            IntervalMonthDayNano::new(0, -1, -5_000)
        );
        Ok(())
    }
}
