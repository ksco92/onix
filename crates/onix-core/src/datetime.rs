//! Calendar types beyond JSON's own shapes; see `docs/design/value-model.md`.
//!
//! - [`Date`]: a Python `datetime.date`.
//! - [`DateTime`]: a Python `datetime.datetime`, naive or fixed-offset.
//! - [`Time`]: a Python `datetime.time`.
//! - [`TimeDelta`]: a Python `datetime.timedelta`.

use std::fmt::Write as _;

/// Microseconds in one second.
const MICROS_PER_SECOND: i64 = 1_000_000;
/// Seconds in one day.
pub(crate) const SECONDS_PER_DAY: i64 = 86_400;
/// Days from `0001-01-01` to the Unix epoch.
const DAYS_FROM_YEAR_ONE_TO_EPOCH: i64 = 719_162;
/// The first year Python's `date`/`datetime` can represent.
const MIN_YEAR: i32 = 1;
/// The last year Python's `date`/`datetime` can represent.
const MAX_YEAR: i32 = 9999;
/// `Date::new(MIN_YEAR, 1, 1).ordinal()`, i.e. Python's `date.min.toordinal()`.
const MIN_ORDINAL: i64 = 1;
/// `Date::new(MAX_YEAR, 12, 31).ordinal()`, i.e. Python's `date.max.toordinal()`.
const MAX_ORDINAL: i64 = 3_652_059;

/// A Python `datetime.date`: a proleptic-Gregorian year in `1..=9999`, month and day.
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
    /// Builds a date, or `None` unless the fields are a real calendar date with year `1..=9999`.
    #[must_use]
    pub fn new(year: i32, month: u8, day: u8) -> Option<Self> {
        ((MIN_YEAR..=MAX_YEAR).contains(&year) && day >= 1 && day <= days_in_month(year, month))
            .then_some(Self { year, month, day })
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

    /// Days since `0001-01-01`, counting that day as `1`: Python's `date.toordinal()`.
    #[must_use]
    pub fn ordinal(self) -> i64 {
        days_from_civil(self.year, self.month, self.day) + DAYS_FROM_YEAR_ONE_TO_EPOCH + 1
    }

    /// The inverse of [`Date::ordinal`], or `None` outside `1..=3_652_059`.
    #[must_use]
    pub fn from_ordinal(ordinal: i64) -> Option<Self> {
        if !(MIN_ORDINAL..=MAX_ORDINAL).contains(&ordinal) {
            return None;
        }
        let (year, month, day) = civil_from_days(ordinal - DAYS_FROM_YEAR_ONE_TO_EPOCH - 1);

        Some(Self { year, month, day })
    }

    /// Python's `date.isoformat()`: `YYYY-MM-DD`.
    #[must_use]
    pub fn isoformat(self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    /// Python's `str(date)`, identical to [`Date::isoformat`].
    #[must_use]
    pub fn python_str(self) -> String {
        self.isoformat()
    }
}

/// A Python `datetime.datetime`: a [`Date`], a wall-clock time to the microsecond and an
/// optional fixed UTC offset in whole seconds (`None` is naive). Comparison rule:
/// `docs/design/value-model.md`, "Calendar types".
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

/// The exclusive bound on a `timezone` offset, in either direction.
const SECONDS_PER_DAY_U32: u32 = 86_400;

impl DateTime {
    /// Builds a datetime, or `None` if a time field is out of range or the offset is not
    /// strictly within ±1 day.
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

    /// Microseconds from `1970-01-01T00:00:00Z`, a naive value counted as UTC.
    #[must_use]
    pub fn instant(self) -> i64 {
        let seconds_of_day =
            i64::from(self.hour) * 3600 + i64::from(self.minute) * 60 + i64::from(self.second)
                - i64::from(self.utc_offset_seconds.unwrap_or(0));

        ((self.date.ordinal() - 1) * SECONDS_PER_DAY + seconds_of_day) * MICROS_PER_SECOND
            + i64::from(self.microsecond)
            - DAYS_FROM_YEAR_ONE_TO_EPOCH * SECONDS_PER_DAY * MICROS_PER_SECOND
    }

    /// This value normalized to UTC (aware, offset `0`), or `None` when that leaves the year
    /// range `1..=9999`; see `tests/golden/README.md`, "Datetime outside year `1..=9999`".
    #[must_use]
    pub fn to_utc(self) -> Option<Self> {
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
        Some(Self {
            date: Date::from_ordinal(days + 1)?,
            hour: (seconds_of_day / 3600) as u8,
            minute: (seconds_of_day / 60 % 60) as u8,
            second: (seconds_of_day % 60) as u8,
            microsecond: (micros_of_day % MICROS_PER_SECOND) as u32,
            utc_offset_seconds: Some(0),
        })
    }

    /// Python's `datetime.isoformat()`; see `docs/design/value-model.md`, "Calendar types".
    #[must_use]
    pub fn isoformat(self) -> String {
        self.rendered('T')
    }

    /// Python's `str(datetime)`: `isoformat(sep=" ")`. It stays distinct from `isoformat()`
    /// because `DeepDiff` coerces and hashes through `str()`.
    #[must_use]
    pub fn python_str(self) -> String {
        self.rendered(' ')
    }

    /// The rendering behind `isoformat` and `python_str`, which differ in `separator`.
    fn rendered(self, separator: char) -> String {
        let mut rendered = format!("{}{separator}", self.date.isoformat());
        render_time_fields(
            &mut rendered,
            self.hour,
            self.minute,
            self.second,
            self.microsecond,
            self.utc_offset_seconds,
        );
        rendered
    }
}

/// Writes `HH:MM:SS[.ffffff][±HH:MM[:SS]]` into `out`, shared by [`DateTime`] and [`Time`].
fn render_time_fields(
    out: &mut String,
    hour: u8,
    minute: u8,
    second: u8,
    microsecond: u32,
    utc_offset_seconds: Option<i32>,
) {
    let _ = write!(out, "{hour:02}:{minute:02}:{second:02}");

    if microsecond != 0 {
        let _ = write!(out, ".{microsecond:06}");
    }

    if let Some(offset) = utc_offset_seconds {
        let sign = if offset < 0 { '-' } else { '+' };
        let magnitude = i64::from(offset.abs());
        let _ = write!(
            out,
            "{sign}{:02}:{:02}",
            magnitude / 3600,
            magnitude / 60 % 60
        );
        if magnitude % 60 != 0 {
            let _ = write!(out, ":{:02}", magnitude % 60);
        }
    }
}

/// A Python `datetime.time`: a wall-clock time to the microsecond and an optional fixed UTC
/// offset in whole seconds (`None` is naive). Equality and hashing differ from [`DateTime`]'s:
/// `docs/design/value-model.md`, "Calendar types".
///
/// ```
/// use onix_core::datetime::Time;
///
/// let naive = Time::new(10, 0, 0, 0, None).expect("in range");
/// let aware = Time::new(12, 0, 0, 0, Some(2 * 3600)).expect("in range");
///
/// assert_eq!(naive.isoformat(), "10:00:00");
/// assert_eq!(aware.isoformat(), "12:00:00+02:00");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Time {
    hour: u8,
    minute: u8,
    second: u8,
    microsecond: u32,
    utc_offset_seconds: Option<i32>,
}

impl Time {
    /// Builds a time, or `None` under the bounds [`DateTime::new`] enforces.
    #[must_use]
    pub fn new(
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
            hour,
            minute,
            second,
            microsecond,
            utc_offset_seconds,
        })
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

    /// Wall-clock microseconds since midnight, ignoring any offset.
    fn wall_micros_of_day(self) -> i64 {
        (i64::from(self.hour) * 3600 + i64::from(self.minute) * 60 + i64::from(self.second))
            * MICROS_PER_SECOND
            + i64::from(self.microsecond)
    }

    /// [`Time::wall_micros_of_day`] shifted by the UTC offset (`0` if naive), not reduced
    /// modulo a day.
    fn adjusted_micros_of_day(self) -> i64 {
        self.wall_micros_of_day()
            - i64::from(self.utc_offset_seconds.unwrap_or(0)) * MICROS_PER_SECOND
    }

    /// The quantity [`times_equal`] compares by within one awareness bucket, which
    /// `value::canonical_cmp` orders by.
    #[must_use]
    pub(crate) fn sort_instant(self) -> i64 {
        if self.utc_offset_seconds.is_some() {
            self.adjusted_micros_of_day()
        } else {
            self.wall_micros_of_day()
        }
    }

    /// Whole seconds since midnight, dropping the microsecond and the offset: `DeepHash`'s
    /// `time_to_seconds`, which `ignore_order` hashes a `time` by.
    #[must_use]
    pub fn hash_seconds_of_day(self) -> i64 {
        (i64::from(self.hour) * 60 + i64::from(self.minute)) * 60 + i64::from(self.second)
    }

    /// Python's `time.isoformat()`, which is also `str(time)`.
    #[must_use]
    pub fn isoformat(self) -> String {
        let mut rendered = String::new();
        render_time_fields(
            &mut rendered,
            self.hour,
            self.minute,
            self.second,
            self.microsecond,
            self.utc_offset_seconds,
        );
        rendered
    }

    /// Python's `str(time)`, identical to [`Time::isoformat`].
    #[must_use]
    pub fn python_str(self) -> String {
        self.isoformat()
    }
}

/// `time.__eq__`: a naive value never equals an aware one, naive values compare by wall clock
/// and aware ones by offset-adjusted micros-of-day. Unlike [`DateTime`], a naive value is never
/// read as UTC.
#[must_use]
pub(crate) fn times_equal(a: Time, b: Time) -> bool {
    match (a.utc_offset_seconds, b.utc_offset_seconds) {
        (None, None) => a.wall_micros_of_day() == b.wall_micros_of_day(),
        (Some(_), Some(_)) => a.adjusted_micros_of_day() == b.adjusted_micros_of_day(),
        _ => false,
    }
}

/// A Python `datetime.timedelta` in Python's normalized form, `days*86_400 + seconds` plus a
/// non-negative microsecond part, within `timedelta.min..=timedelta.max`; see
/// `docs/design/value-model.md`, "Calendar types".
///
/// ```
/// use onix_core::datetime::TimeDelta;
///
/// let value = TimeDelta::new(1, 3600, 0).expect("in range");
/// assert_eq!(value.python_str(), "1 day, 1:00:00");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TimeDelta {
    /// `days*86_400 + seconds`; the derived ordering matches Python's.
    total_seconds: i64,
    /// Python's `timedelta.microseconds`, `0..=999_999`.
    subsecond_microseconds: u32,
}

/// `timedelta.min`, in days.
const TIMEDELTA_MIN_DAYS: i64 = -999_999_999;
/// `timedelta.max`, in days.
const TIMEDELTA_MAX_DAYS: i64 = 999_999_999;

impl TimeDelta {
    /// Builds a duration from a normalized `(days, seconds, microseconds)` triple, or `None`
    /// if `seconds` or `microseconds` leave `0..86_400` or `0..1_000_000` or the result leaves
    /// `timedelta.min..=timedelta.max`.
    #[must_use]
    pub fn new(days: i64, seconds: i64, microseconds: i64) -> Option<Self> {
        if !(0..SECONDS_PER_DAY).contains(&seconds)
            || !(0..MICROS_PER_SECOND).contains(&microseconds)
        {
            return None;
        }
        if !(TIMEDELTA_MIN_DAYS..=TIMEDELTA_MAX_DAYS).contains(&days) {
            return None;
        }

        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "microseconds is range-checked above to 0..1_000_000, which fits a u32 \
                      with no sign to lose"
        )]
        Some(Self {
            total_seconds: days * SECONDS_PER_DAY + seconds,
            subsecond_microseconds: microseconds as u32,
        })
    }

    /// Python's `timedelta.days`.
    #[must_use]
    pub fn days(self) -> i64 {
        self.total_seconds.div_euclid(SECONDS_PER_DAY)
    }

    /// Python's `timedelta.seconds`, `0..86_400`.
    #[must_use]
    pub fn seconds(self) -> i64 {
        self.total_seconds.rem_euclid(SECONDS_PER_DAY)
    }

    /// Python's `timedelta.microseconds`, `0..1_000_000`.
    #[must_use]
    pub fn microseconds(self) -> i64 {
        i64::from(self.subsecond_microseconds)
    }

    /// Python's `timedelta.total_seconds()`.
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "mirrors Python's own total_seconds(), an inexact float division for any \
                  duration whose microsecond count exceeds f64's exact-integer range"
    )]
    pub fn total_seconds(self) -> f64 {
        self.total_seconds as f64
            + f64::from(self.subsecond_microseconds) / MICROS_PER_SECOND as f64
    }

    /// Python's `str(timedelta)`: `"[-]D day(s), H:MM:SS[.ffffff]"`. `to_json()` renders this,
    /// as `timedelta` has no `isoformat()`.
    #[must_use]
    pub fn python_str(self) -> String {
        let (days, seconds, microseconds) = (self.days(), self.seconds(), self.microseconds());
        let mut out = String::new();

        if days != 0 {
            let unit = if days.abs() == 1 { "day" } else { "days" };
            let _ = write!(out, "{days} {unit}, ");
        }

        let _ = write!(
            out,
            "{}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        );

        if microseconds != 0 {
            let _ = write!(out, ".{microseconds:06}");
        }

        out
    }
}

/// Floored division and its non-negative remainder.
pub(crate) fn div_rem_euclid(value: i64, divisor: i64) -> (i64, i64) {
    (value.div_euclid(divisor), value.rem_euclid(divisor))
}

/// Days from `1970-01-01` to the date, negative before the epoch (Hinnant's `days_from_civil`).
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

/// The days in `month` of `year`, or `0` for a month that does not exist.
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
