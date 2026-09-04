//! The two calendar values [`crate::Value`] carries beyond JSON's own
//! shapes: [`Date`] (a Python `datetime.date`) and [`DateTime`] (a Python
//! `datetime.datetime`, naive or with a fixed UTC offset).
//!
//! # Representation
//!
//! Both hold plain wall-clock fields — a [`Date`] is a year/month/day, a
//! [`DateTime`] adds hour/minute/second/microsecond plus an optional UTC
//! offset in whole seconds (`None` is Python's *naive* datetime). Nothing
//! here needs a calendar crate: the only arithmetic the engine performs is
//! civil-date to day-number conversion in both directions
//! ([`Date::ordinal`] and [`Date::from_ordinal`], Howard Hinnant's
//! `days_from_civil`/`civil_from_days`), which is a few lines of integer
//! math and no lookup tables.
//!
//! # Comparison: by instant, naive as UTC
//!
//! `DeepDiff` compares two datetimes by *instant*, after normalizing each
//! through `helper.py::datetime_normalize`: an aware value is converted with
//! `astimezone(timezone.utc)`, a naive one is *stamped* with UTC
//! (`replace(tzinfo=utc)`) rather than interpreted in local time. So
//! `datetime(2024, 1, 1, 10)` and `datetime(2024, 1, 1, 10, tzinfo=utc)` are
//! one instant, and `10:00+00:00` equals `12:00+02:00`. [`DateTime::instant`]
//! is that rule as one integer, and [`DateTime::to_utc`] is
//! `datetime_normalize` itself.
//!
//! Two [`Date`]s compare by value, and a [`Date`] never equals a
//! [`DateTime`] — matching Python, where `date(2024, 1, 1) ==
//! datetime(2024, 1, 1)` is `False` in both directions
//! (`datetime.__eq__` returns `False`, not `NotImplemented`, for a plain
//! `date`, and its subclass position gives it first refusal).
//!
//! # Rendering
//!
//! [`Date::isoformat`] and [`DateTime::isoformat`] reproduce Python's own
//! `isoformat()` byte for byte: microseconds only when non-zero, an offset
//! suffix only when the value is aware, and that suffix widening from
//! `+HH:MM` to `+HH:MM:SS` when the offset is not a whole number of minutes.
//! `DeepDiff`'s `to_json()` renders a datetime through exactly this method
//! (`serialization.py`'s `JSON_CONVERTOR` maps `datetime.datetime` to
//! `lambda x: x.isoformat()`); it has no entry for `date` at all and raises
//! `TypeError` on one, which this crate renders as `YYYY-MM-DD` instead — a
//! documented superset, see `tests/golden/README.md`.

use std::fmt::Write as _;

/// Microseconds in one second.
const MICROS_PER_SECOND: i64 = 1_000_000;
/// Seconds in one day.
const SECONDS_PER_DAY: i64 = 86_400;
/// Days from `0001-01-01` to the Unix epoch — the shift between Python's
/// `date.toordinal()` origin and this module's civil-date arithmetic.
const DAYS_FROM_YEAR_ONE_TO_EPOCH: i64 = 719_162;

/// A Python `datetime.date`: a proleptic-Gregorian year, month and day.
///
/// # Examples
///
/// ```
/// use onix_core::datetime::Date;
///
/// let date = Date::new(2024, 2, 29).expect("2024 is a leap year");
/// assert_eq!(date.isoformat(), "2024-02-29");
/// assert!(Date::new(2023, 2, 29).is_none());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    year: i32,
    month: u8,
    day: u8,
}

impl Date {
    /// Builds a date, returning `None` if `month`/`day` are not a real
    /// calendar date (leap years included). An out-of-range month has no days
    /// at all (`days_in_month` returns `0` for one), so the single `day`
    /// bound below rejects it too.
    ///
    /// `year` is not range-checked. Python's own `date` spans years `1`
    /// through `9999`, and every date this crate sees comes from a real
    /// Python object, so that bound is enforced upstream; normalizing an
    /// extreme datetime to UTC can also legitimately land one day outside it
    /// (see [`DateTime::to_utc`]).
    #[must_use]
    pub fn new(year: i32, month: u8, day: u8) -> Option<Self> {
        (day >= 1 && day <= days_in_month(year, month)).then_some(Self { year, month, day })
    }

    /// The year.
    #[must_use]
    pub fn year(self) -> i32 {
        self.year
    }

    /// The month, `1..=12`.
    #[must_use]
    pub fn month(self) -> u8 {
        self.month
    }

    /// The day of the month, `1..=31`.
    #[must_use]
    pub fn day(self) -> u8 {
        self.day
    }

    /// Days since `0001-01-01`, counting that day as `1` — Python's
    /// `date.toordinal()`, which is what `DeepDiff` measures a date-pair
    /// distance with (`distance.py::_get_date_distance`).
    #[must_use]
    pub fn ordinal(self) -> i64 {
        days_from_civil(self.year, self.month, self.day) + DAYS_FROM_YEAR_ONE_TO_EPOCH + 1
    }

    /// The inverse of [`Date::ordinal`].
    #[must_use]
    pub fn from_ordinal(ordinal: i64) -> Self {
        let (year, month, day) = civil_from_days(ordinal - DAYS_FROM_YEAR_ONE_TO_EPOCH - 1);
        Self { year, month, day }
    }

    /// Python's `date.isoformat()`: `YYYY-MM-DD`.
    #[must_use]
    pub fn isoformat(self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

/// A Python `datetime.datetime`: a [`Date`] plus a wall-clock time to
/// microsecond precision, and an optional fixed UTC offset in whole seconds
/// (`None` is a *naive* datetime).
///
/// See the [module documentation](self) for the instant-comparison rule and
/// the exact `isoformat()` reproduction.
///
/// # Examples
///
/// ```
/// use onix_core::datetime::{Date, DateTime};
///
/// let date = Date::new(2024, 1, 1).expect("a real date");
/// let naive = DateTime::new(date, 10, 0, 0, 0, None).expect("in range");
/// let aware = DateTime::new(date, 12, 0, 0, 0, Some(2 * 3600)).expect("in range");
///
/// assert_eq!(naive.isoformat(), "2024-01-01T10:00:00");
/// assert_eq!(aware.isoformat(), "2024-01-01T12:00:00+02:00");
/// // Naive counts as UTC, so these are the same instant.
/// assert_eq!(naive.instant(), aware.instant());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DateTime {
    date: Date,
    hour: u8,
    minute: u8,
    second: u8,
    microsecond: u32,
    utc_offset_seconds: Option<i32>,
}

/// The exclusive bound Python puts on a `timezone` offset: strictly less
/// than one day, in either direction.
const SECONDS_PER_DAY_U32: u32 = 86_400;

impl DateTime {
    /// Builds a datetime, returning `None` if the time fields are out of
    /// range (`hour <= 23`, `minute`/`second <= 59`, `microsecond <=
    /// 999_999`) or the offset is not strictly within ±1 day — the same
    /// bounds Python's own `datetime`/`timezone` constructors enforce.
    #[must_use]
    pub fn new(
        date: Date,
        hour: u8,
        minute: u8,
        second: u8,
        microsecond: u32,
        utc_offset_seconds: Option<i32>,
    ) -> Option<Self> {
        let in_range = hour <= 23
            && minute <= 59
            && second <= 59
            && microsecond <= 999_999
            && utc_offset_seconds.is_none_or(|offset| offset.unsigned_abs() < SECONDS_PER_DAY_U32);

        in_range.then_some(Self {
            date,
            hour,
            minute,
            second,
            microsecond,
            utc_offset_seconds,
        })
    }

    /// The calendar date part.
    #[must_use]
    pub fn date(self) -> Date {
        self.date
    }

    /// The hour, `0..=23`.
    #[must_use]
    pub fn hour(self) -> u8 {
        self.hour
    }

    /// The minute, `0..=59`.
    #[must_use]
    pub fn minute(self) -> u8 {
        self.minute
    }

    /// The second, `0..=59`.
    #[must_use]
    pub fn second(self) -> u8 {
        self.second
    }

    /// The microsecond, `0..=999_999`.
    #[must_use]
    pub fn microsecond(self) -> u32 {
        self.microsecond
    }

    /// The fixed UTC offset in whole seconds, or `None` for a naive value.
    #[must_use]
    pub fn utc_offset_seconds(self) -> Option<i32> {
        self.utc_offset_seconds
    }

    /// This value's instant, as microseconds from `1970-01-01T00:00:00Z`,
    /// with a naive value counted as UTC — `DeepDiff`'s comparison key (see
    /// the [module documentation](self)).
    ///
    /// Exact across the whole Python-representable range: year `9999`'s
    /// microsecond count is under `2.6e17`, three orders of magnitude inside
    /// [`i64`].
    #[must_use]
    pub fn instant(self) -> i64 {
        let seconds_of_day =
            i64::from(self.hour) * 3600 + i64::from(self.minute) * 60 + i64::from(self.second)
                - i64::from(self.utc_offset_seconds.unwrap_or(0));

        ((self.date.ordinal() - 1) * SECONDS_PER_DAY + seconds_of_day) * MICROS_PER_SECOND
            + i64::from(self.microsecond)
            - DAYS_FROM_YEAR_ONE_TO_EPOCH * SECONDS_PER_DAY * MICROS_PER_SECOND
    }

    /// This value normalized to UTC — `helper.py::datetime_normalize` with
    /// the default `default_timezone=timezone.utc`, i.e. the values
    /// `DeepDiff` puts in a `values_changed` entry for a datetime pair.
    ///
    /// The result is always aware with offset `0`, so two normalized values
    /// are equal exactly when the originals are the same instant. Converting
    /// an extreme aware value can land the result outside Python's own
    /// year `1..=9999` range by at most one day, where real `astimezone`
    /// raises `OverflowError`; this returns the shifted value rather than
    /// failing, since there is no `DeepDiff` behavior to match there.
    #[must_use]
    pub fn to_utc(self) -> Self {
        let instant =
            self.instant() + DAYS_FROM_YEAR_ONE_TO_EPOCH * SECONDS_PER_DAY * MICROS_PER_SECOND;
        let (days, micros_of_day) = div_rem_euclid(instant, SECONDS_PER_DAY * MICROS_PER_SECOND);
        let seconds_of_day = micros_of_day / MICROS_PER_SECOND;

        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "`div_rem_euclid` makes `micros_of_day` non-negative and strictly under one \
                      day, so every component below is non-negative and inside its own field"
        )]
        Self {
            date: Date::from_ordinal(days + 1),
            hour: (seconds_of_day / 3600) as u8,
            minute: (seconds_of_day / 60 % 60) as u8,
            second: (seconds_of_day % 60) as u8,
            microsecond: (micros_of_day % MICROS_PER_SECOND) as u32,
            utc_offset_seconds: Some(0),
        }
    }

    /// Python's `datetime.isoformat()`: `YYYY-MM-DDTHH:MM:SS`, plus
    /// `.ffffff` when the microsecond is non-zero and an offset suffix when
    /// the value is aware — see the [module documentation](self).
    #[must_use]
    pub fn isoformat(self) -> String {
        let mut rendered = format!(
            "{}T{:02}:{:02}:{:02}",
            self.date.isoformat(),
            self.hour,
            self.minute,
            self.second
        );

        if self.microsecond != 0 {
            let _ = write!(rendered, ".{:06}", self.microsecond);
        }

        if let Some(offset) = self.utc_offset_seconds {
            let sign = if offset < 0 { '-' } else { '+' };
            let magnitude = i64::from(offset.abs());
            let _ = write!(
                rendered,
                "{sign}{:02}:{:02}",
                magnitude / 3600,
                magnitude / 60 % 60
            );
            if magnitude % 60 != 0 {
                let _ = write!(rendered, ":{:02}", magnitude % 60);
            }
        }

        rendered
    }
}

/// Floored division and its remainder, both taken toward negative infinity —
/// the split [`DateTime::to_utc`] needs to turn a possibly-negative
/// microsecond count into a whole day plus a non-negative offset into it.
fn div_rem_euclid(value: i64, divisor: i64) -> (i64, i64) {
    (value.div_euclid(divisor), value.rem_euclid(divisor))
}

/// Days from `1970-01-01` to `year-month-day`, negative before the epoch —
/// Howard Hinnant's `days_from_civil`, valid for any proleptic-Gregorian
/// date.
fn days_from_civil(year: i32, month: u8, day: u8) -> i64 {
    let year = i64::from(year) - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;

    era * 146_097 + day_of_era - 719_468
}

/// The inverse of [`days_from_civil`] — Hinnant's `civil_from_days`.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the day count this crate reaches spans years 1..=9999, so the year fits an i32 \
              and the month and day are always positive and inside a u8"
)]
fn civil_from_days(days: i64) -> (i32, u8, u8) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_position = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_position + 2) / 5 + 1;
    let month = month_position + if month_position < 10 { 3 } else { -9 };

    (
        (year + i64::from(month <= 2)) as i32,
        month as u8,
        day as u8,
    )
}

/// The number of days in `month` of `year`, or `0` if `month` is not a real
/// month — which is what makes it [`Date::new`]'s only bound.
fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Whether `year` is a proleptic-Gregorian leap year.
fn is_leap_year(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

#[cfg(test)]
#[path = "datetime_tests.rs"]
mod tests;
