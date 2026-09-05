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
//!
//! # `Time` and `TimeDelta`
//!
//! [`Time`] is a `datetime.time`: the same wall-clock/offset fields as
//! [`DateTime`] minus the calendar date. Unlike [`DateTime`], Python never
//! reads a naive `time` as if it were UTC: `_diff_time` (the function real
//! `DeepDiff` uses for `time`, `date` *and* `timedelta` alike) is a plain
//! `!=`, with no normalization step, so [`Time`]'s own equality
//! (`times_equal`) is exactly that plain-`!=` rule — a naive value is
//! never equal to an aware one, and two aware values compare by an
//! offset-adjusted micros-of-day, both confirmed against real Python
//! (`time.__eq__`'s documented "if both are aware... adjusted by
//! subtracting their UTC offsets" rule, live-verified including negative
//! and sub-minute offsets, with no modular wraparound past midnight).
//! [`Time::isoformat`] reproduces `time.isoformat()` byte for byte, which is
//! the same `HH:MM:SS[.ffffff][±offset]` shape [`DateTime::isoformat`]'s
//! time portion uses (always-present seconds included) — the two share the
//! `render_time_fields` helper. `DeepDiff`'s `to_json()` has no
//! `datetime.time` entry either and raises the same way it does for `date`;
//! this crate again renders the same superset, `time.isoformat()`'s bytes.
//!
//! [`TimeDelta`] is a `datetime.timedelta`: an exact signed duration, stored
//! as Python's own normalized `(days, seconds, microseconds)` triple
//! (`total_seconds`/`subsecond_microseconds` — a flattened total-microsecond
//! count would overflow `i64` at Python's own extreme `days=999_999_999`,
//! see [`TimeDelta`]'s own doc); a `timedelta` always compares and hashes by
//! this exact value — no analogous naive/aware split. [`TimeDelta::python_str`]
//! reproduces `str(timedelta)` (`"[-]D day(s), H:MM:SS[.ffffff]"`, the day
//! prefix present only when non-zero); `to_json()` again has no entry for
//! `timedelta` and raises, and this crate renders the same `str()` bytes as
//! its documented superset — there being no `timedelta.isoformat()` to
//! mirror instead, `str()` is the natural, deterministic choice.
//!
//! Both hash under `ignore_order` the way real `DeepHash` does, which for
//! `time` is a genuine, confirmed quirk: `_prep_datetime` reduces a `time`
//! to `(hour*60+minute)*60+second` — dropping *both* the microsecond and any
//! offset entirely — before hashing, so two times equal only in whole
//! seconds-of-day hash-match under `ignore_order` even when plain `==` would
//! call them different (live-confirmed: a microsecond-only or an
//! offset-only difference both hash-match). `timedelta` hashes exactly (no
//! truncation) via `_prep_number`. See `crate::ignore_order::hash`'s
//! `hash_seconds_of_day` for the `time` quirk and `tests/golden/README.md`
//! for the citations.

use std::fmt::Write as _;

/// Microseconds in one second.
const MICROS_PER_SECOND: i64 = 1_000_000;
/// Seconds in one day.
pub(crate) const SECONDS_PER_DAY: i64 = 86_400;
/// Days from `0001-01-01` to the Unix epoch — the shift between Python's
/// `date.toordinal()` origin and this module's civil-date arithmetic.
const DAYS_FROM_YEAR_ONE_TO_EPOCH: i64 = 719_162;
/// The first year Python's `date`/`datetime` can represent.
const MIN_YEAR: i32 = 1;
/// The last year Python's `date`/`datetime` can represent.
const MAX_YEAR: i32 = 9999;
/// `Date::new(MIN_YEAR, 1, 1).ordinal()`, i.e. Python's `date.min.toordinal()`.
const MIN_ORDINAL: i64 = 1;
/// `Date::new(MAX_YEAR, 12, 31).ordinal()`, i.e. Python's `date.max.toordinal()`.
const MAX_ORDINAL: i64 = 3_652_059;

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
    /// Builds a date, returning `None` unless `year`/`month`/`day` are a real
    /// calendar date (leap years included) inside Python's own year range,
    /// `1..=9999`. An out-of-range month has no days at all
    /// (`days_in_month` returns `0` for one), so the `day` bound rejects it
    /// without a separate month check.
    ///
    /// Enforcing the year range here is what lets every other method on this
    /// type be total: the ordinal arithmetic stays far inside [`i64`], and
    /// the field widths in [`Date::from_ordinal`] are guaranteed.
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

    /// Days since `0001-01-01`, counting that day as `1` — Python's
    /// `date.toordinal()`, which is what `DeepDiff` measures a date-pair
    /// distance with (`distance.py::_get_date_distance`).
    #[must_use]
    pub fn ordinal(self) -> i64 {
        days_from_civil(self.year, self.month, self.day) + DAYS_FROM_YEAR_ONE_TO_EPOCH + 1
    }

    /// The inverse of [`Date::ordinal`], or `None` for an ordinal outside
    /// the representable range (`1..=3_652_059`, Python's
    /// `date.min`/`date.max`).
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

    /// Python's `str(date)`, which for a date is exactly its
    /// [`isoformat`](Date::isoformat) — the two differ only for a datetime.
    /// See [`DateTime::python_str`] for why `str()` is worth a method of its
    /// own at all.
    #[must_use]
    pub fn python_str(self) -> String {
        self.isoformat()
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
    /// are equal exactly when the originals are the same instant.
    ///
    /// Returns `None` for the one case that has no answer: an extreme aware
    /// value whose UTC wall clock falls outside Python's own `1..=9999` year
    /// range, by at most one day (`9999-12-31T23:00-01:00`, say). Real
    /// `astimezone(timezone.utc)` raises `OverflowError: date value out of
    /// range` there, and so `DeepDiff` raises rather than reporting anything.
    ///
    /// *When* each tool reaches that point differs, verified live. On the
    /// ordered path only `_diff_datetime` normalizes, so both raise only when
    /// two datetimes are actually compared. Under `ignore_order`,
    /// `deephash.py::_prep_datetime` normalizes every datetime it hashes, so
    /// real `DeepDiff` raises for such a value even when it is merely added,
    /// removed, or shuffled, where onix hashes by instant (see
    /// `crate::ignore_order`) and reports it raw.
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

    /// Python's `datetime.isoformat()`: `YYYY-MM-DDTHH:MM:SS`, plus
    /// `.ffffff` when the microsecond is non-zero and an offset suffix when
    /// the value is aware — see the [module documentation](self).
    #[must_use]
    pub fn isoformat(self) -> String {
        self.rendered('T')
    }

    /// Python's `str(datetime)`, which is `isoformat(sep=" ")` — the same
    /// rendering with a space where the `T` goes.
    ///
    /// This is the one place the `str()`-versus-`isoformat()` distinction is
    /// explained, for both calendar types. They are kept apart because they
    /// have different jobs: `isoformat()` is what `to_json()` prints, while
    /// `str()` is what `DeepDiff` reproduces when it tests whether a
    /// `type_changes` pair's new value is reachable by coercion
    /// (`model.py`'s `new_t1 = new_type(change.t1)`), and what `DeepHash`
    /// embeds in a `frozenset` member's digest.
    #[must_use]
    pub fn python_str(self) -> String {
        self.rendered(' ')
    }

    /// The shared rendering behind [`isoformat`](DateTime::isoformat) and
    /// [`python_str`](DateTime::python_str), which differ only in the
    /// separator between the date and the time.
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

/// Writes `HH:MM:SS[.ffffff][±offset]` into `out` — the time-of-day
/// rendering [`DateTime::rendered`] and [`Time::isoformat`] share, since a
/// `time.isoformat()` is byte-for-byte the same shape as a
/// `datetime.isoformat()`'s own time portion (seconds always present,
/// microseconds only when non-zero, and the offset suffix widening from
/// `+HH:MM` to `+HH:MM:SS` when it is not a whole number of minutes) —
/// confirmed against real Python for both types.
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

/// A Python `datetime.time`: a wall-clock time to microsecond precision plus
/// an optional fixed UTC offset in whole seconds (`None` is naive) — see the
/// [module documentation](self) for how its equality and hashing genuinely
/// diverge from [`DateTime`]'s.
///
/// # Examples
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
    /// Builds a time, returning `None` if any field is out of range — the
    /// same bounds [`DateTime::new`] enforces on its own time fields.
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

    /// Wall-clock microseconds since midnight, ignoring any offset — the
    /// quantity two *naive* values compare by, and the base
    /// [`Time::adjusted_micros_of_day`] adjusts for an aware one.
    fn wall_micros_of_day(self) -> i64 {
        (i64::from(self.hour) * 3600 + i64::from(self.minute) * 60 + i64::from(self.second))
            * MICROS_PER_SECOND
            + i64::from(self.microsecond)
    }

    /// [`Time::wall_micros_of_day`] shifted by this value's own UTC offset (a
    /// naive value's offset is `0`) — the quantity [`times_equal`] compares
    /// two *aware* values by, deliberately not reduced modulo a day (real
    /// Python does not wrap either; a large offset difference simply never
    /// compares equal, live-confirmed).
    fn adjusted_micros_of_day(self) -> i64 {
        self.wall_micros_of_day()
            - i64::from(self.utc_offset_seconds.unwrap_or(0)) * MICROS_PER_SECOND
    }

    /// The quantity two same-awareness values order by in the crate's
    /// canonical set order (`value::canonical_cmp`): [`Time::wall_micros_of_day`]
    /// for a naive value, [`Time::adjusted_micros_of_day`] for an aware one —
    /// i.e. exactly the quantity [`times_equal`] compares by within one
    /// awareness bucket, so two values with equal `sort_instant`s (and equal
    /// awareness, and equal raw offset) are exactly the values `times_equal`
    /// calls equal.
    #[must_use]
    pub(crate) fn sort_instant(self) -> i64 {
        if self.utc_offset_seconds.is_some() {
            self.adjusted_micros_of_day()
        } else {
            self.wall_micros_of_day()
        }
    }

    /// The whole seconds-of-day `(hour*60+minute)*60+second`, dropping the
    /// microsecond and any offset entirely — real `DeepHash`'s own
    /// `time_to_seconds`, the quantity a `time` hashes and is ranked by
    /// under `ignore_order` (see the [module documentation](self) and
    /// `crate::ignore_order::hash`).
    #[must_use]
    pub fn hash_seconds_of_day(self) -> i64 {
        (i64::from(self.hour) * 60 + i64::from(self.minute)) * 60 + i64::from(self.second)
    }

    /// Python's `time.isoformat()`, which is also `str(time)` — the same
    /// `render_time_fields` rendering [`DateTime::isoformat`]'s time portion
    /// shares (see the [module documentation](self)).
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

    /// Python's `str(time)`, identical to [`Time::isoformat`] — kept as its
    /// own method for symmetry with [`Date::python_str`]/
    /// [`DateTime::python_str`], the call sites that need "the `str()` form"
    /// by name rather than "the `isoformat()` form".
    #[must_use]
    pub fn python_str(self) -> String {
        self.isoformat()
    }
}

/// `time.__eq__`'s exact rule (see the [module documentation](self)): a
/// naive value is never equal to an aware one; two naive values compare by
/// wall-clock fields; two aware values compare by their offset-adjusted
/// micros-of-day. This is the *only* equality [`Time`] has — unlike
/// [`DateTime`], nothing here treats a naive value as if it carried an
/// implicit UTC offset, because real `_diff_time` never normalizes at all.
#[must_use]
pub(crate) fn times_equal(a: Time, b: Time) -> bool {
    match (a.utc_offset_seconds, b.utc_offset_seconds) {
        (None, None) => a.wall_micros_of_day() == b.wall_micros_of_day(),
        (Some(_), Some(_)) => a.adjusted_micros_of_day() == b.adjusted_micros_of_day(),
        _ => false,
    }
}

/// A Python `datetime.timedelta`: an exact signed duration, stored as
/// [`TimeDelta::new`]'s `(days, seconds, microseconds)` triple already
/// combined into `total_seconds` (`days*86_400 + seconds`, always exactly
/// representable — Python's own extreme `days=±999_999_999` keeps this far
/// inside [`i64`], where the *microsecond* count of the same extreme does
/// not: `total_seconds` avoids ever forming that overflowing product) plus
/// the separate non-negative `subsecond_microseconds` field. This is exactly
/// Python's own internal normalized form (`timedelta.days`/`.seconds`/
/// `.microseconds`, `.seconds` and `.microseconds` both folded to be
/// non-negative, every sign living in `days`/`total_seconds`), so both `==`
/// and `total_seconds()` read it directly with no re-derivation (see the
/// [module documentation](self)).
///
/// # Examples
///
/// ```
/// use onix_core::datetime::TimeDelta;
///
/// let value = TimeDelta::new(1, 3600, 0).expect("in range");
/// assert_eq!(value.python_str(), "1 day, 1:00:00");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TimeDelta {
    /// `days*86_400 + seconds` (Python's own `.days`/`.seconds`, combined) —
    /// ordering/equality by `(total_seconds, subsecond_microseconds)` is
    /// exactly Python's own `(days, seconds, microseconds)` lexicographic
    /// comparison, since `total_seconds` alone already carries the same
    /// information as `(days, seconds)` together.
    total_seconds: i64,
    /// Python's `timedelta.microseconds`, always `0..=999_999` regardless of
    /// `total_seconds`'s sign.
    subsecond_microseconds: u32,
}

/// Python's `timedelta.min` is `timedelta(days=-999_999_999)`.
const TIMEDELTA_MIN_DAYS: i64 = -999_999_999;
/// Python's `timedelta.max` is `timedelta(days=999_999_999, hours=23,
/// minutes=59, seconds=59, microseconds=999_999)`.
const TIMEDELTA_MAX_DAYS: i64 = 999_999_999;

impl TimeDelta {
    /// Builds a duration from Python's own already-normalized
    /// `(days, seconds, microseconds)` triple — exactly what reading a real
    /// `timedelta` object's `.days`/`.seconds`/`.microseconds` attributes
    /// gives, so a caller never has to normalize a raw, possibly negative or
    /// out-of-component-range combination itself.
    ///
    /// Returns `None` if `seconds`/`microseconds` are outside their
    /// documented `0..86_400`/`0..1_000_000` component ranges, or the
    /// resulting duration falls outside Python's own
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

    /// Python's `str(timedelta)`: `"[-]D day(s), H:MM:SS[.ffffff]"`, the day
    /// prefix present only when non-zero (singular "day" at magnitude `1`,
    /// "days" otherwise, sign included), the hour unpadded, and the
    /// microsecond suffix only when non-zero — verified against real Python
    /// across zero, negative, sub-day and multi-day durations. There is no
    /// `timedelta.isoformat()` to mirror for `to_json()`, so this is the
    /// chosen documented superset (see the [module documentation](self)).
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

/// Floored division and its remainder, both taken toward negative infinity —
/// the split [`DateTime::to_utc`] needs to turn a possibly-negative
/// microsecond count into a whole day plus a non-negative offset into it.
pub(crate) fn div_rem_euclid(value: i64, divisor: i64) -> (i64, i64) {
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
