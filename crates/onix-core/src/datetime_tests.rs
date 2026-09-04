//! Unit tests for [`super::Date`]/[`super::DateTime`]. Every expected
//! literal here was read off real `CPython` 3.13 (`toordinal()`,
//! `isoformat()`, `astimezone(timezone.utc)`, `timestamp()`), not derived
//! from this module's own arithmetic.

use super::{Date, DateTime};

/// A date that is known-valid, for tests whose subject is not the
/// constructor.
fn date(year: i32, month: u8, day: u8) -> Date {
    Date::new(year, month, day).expect("test date is a real calendar date")
}

/// A datetime that is known-valid, for tests whose subject is not the
/// constructor.
#[allow(clippy::too_many_arguments, reason = "one argument per datetime field")]
fn dt(
    year: i32,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
    microsecond: u32,
    offset: Option<i32>,
) -> DateTime {
    DateTime::new(
        date(year, month, day),
        hour,
        minute,
        second,
        microsecond,
        offset,
    )
    .expect("test datetime fields are in range")
}

#[test]
fn date_new_rejects_impossible_calendar_dates() {
    assert!(Date::new(2024, 2, 29).is_some());
    assert!(Date::new(2023, 2, 29).is_none());
    assert!(Date::new(1900, 2, 29).is_none());
    assert!(Date::new(2000, 2, 29).is_some());
    assert!(Date::new(2024, 4, 31).is_none());
    assert!(Date::new(2024, 0, 1).is_none());
    assert!(Date::new(2024, 13, 1).is_none());
    assert!(Date::new(2024, 1, 0).is_none());
}

#[test]
fn date_new_accepts_every_months_real_last_day() {
    // The rejection boundary above (day 31 in a 30-day month, day 29 in a
    // non-leap February) is satisfied whether `days_in_month` returns its
    // real value or 0 for that month, so it alone cannot tell a deleted
    // match arm from a correct one; each month's real last day itself must
    // be accepted.
    for month in [4, 6, 9, 11] {
        assert!(
            Date::new(2024, month, 30).is_some(),
            "month {month}, day 30"
        );
    }
    for month in [1, 3, 5, 7, 8, 10, 12] {
        assert!(
            Date::new(2024, month, 31).is_some(),
            "month {month}, day 31"
        );
    }
    // The plain (non-leap-year) `2 => 28` arm, distinct from the
    // `is_leap_year` guarded one just above it.
    assert!(Date::new(2023, 2, 28).is_some());
}

#[test]
fn date_accessors_return_the_constructed_fields() {
    let value = date(2024, 3, 17);

    assert_eq!(value.year(), 2024);
    assert_eq!(value.month(), 3);
    assert_eq!(value.day(), 17);
}

#[test]
fn date_ordinal_matches_python_toordinal() {
    assert_eq!(date(1, 1, 1).ordinal(), 1);
    assert_eq!(date(1970, 1, 1).ordinal(), 719_163);
    assert_eq!(date(2024, 1, 1).ordinal(), 738_886);
    assert_eq!(date(9999, 12, 31).ordinal(), 3_652_059);
}

#[test]
fn date_from_ordinal_inverts_ordinal_across_the_python_range() {
    for ordinal in [1_i64, 719_163, 738_886, 3_652_059] {
        assert_eq!(
            Date::from_ordinal(ordinal).expect("in range").ordinal(),
            ordinal
        );
    }
    assert_eq!(Date::from_ordinal(738_886), Some(date(2024, 1, 1)));
    assert_eq!(Date::from_ordinal(1), Some(date(1, 1, 1)));
}

#[test]
fn date_from_ordinal_inverts_every_ordinal_in_the_python_range() {
    // The four hand-picked ordinals above all land in the first ~100 years
    // of their 400-year "era" (Hinnant's `civil_from_days` correction
    // terms), where the era's `/36_524`/`/146_096` century/era corrections
    // are `0` regardless of whether they are added or subtracted -- mutation
    // testing found this leaves an `+`/`-` flip on either correction term
    // invisible. Exhaustive coverage catches it (and anything else) at
    // every era position, including the one day in 400 years the
    // `/146_096` term is actually non-zero.
    for ordinal in 1_i64..=3_652_059 {
        assert_eq!(
            Date::from_ordinal(ordinal).expect("in range").ordinal(),
            ordinal,
            "ordinal {ordinal} did not round-trip"
        );
    }
}

#[test]
fn date_rejects_years_and_ordinals_outside_pythons_own_range() {
    // Python's `date` spans years 1..=9999, i.e. ordinals 1..=3_652_059
    // (`date.min.toordinal()` / `date.max.toordinal()`).
    assert!(Date::new(0, 12, 31).is_none());
    assert!(Date::new(10_000, 1, 1).is_none());
    assert!(Date::new(-1, 1, 1).is_none());
    assert!(Date::new(1, 1, 1).is_some());
    assert!(Date::new(9999, 12, 31).is_some());

    assert_eq!(Date::from_ordinal(0), None);
    assert_eq!(Date::from_ordinal(3_652_060), None);
    assert_eq!(Date::from_ordinal(-1), None);
}

#[test]
fn date_isoformat_matches_python() {
    assert_eq!(date(2024, 1, 1).isoformat(), "2024-01-01");
    assert_eq!(date(1, 1, 1).isoformat(), "0001-01-01");
    assert_eq!(date(9999, 12, 31).isoformat(), "9999-12-31");
}

#[test]
fn datetime_new_rejects_out_of_range_fields_and_offsets() {
    let day = date(2024, 1, 1);

    assert!(DateTime::new(day, 24, 0, 0, 0, None).is_none());
    assert!(DateTime::new(day, 0, 60, 0, 0, None).is_none());
    assert!(DateTime::new(day, 0, 0, 60, 0, None).is_none());
    assert!(DateTime::new(day, 0, 0, 0, 1_000_000, None).is_none());
    assert!(DateTime::new(day, 0, 0, 0, 0, Some(86_400)).is_none());
    assert!(DateTime::new(day, 0, 0, 0, 0, Some(-86_400)).is_none());
    assert!(DateTime::new(day, 23, 59, 59, 999_999, Some(86_399)).is_some());
    assert!(DateTime::new(day, 0, 0, 0, 0, Some(-86_399)).is_some());
}

#[test]
fn datetime_accessors_return_the_constructed_fields() {
    let value = dt(2024, 3, 17, 4, 5, 6, 7, Some(-18_000));

    assert_eq!(value.date(), date(2024, 3, 17));
    assert_eq!(value.hour(), 4);
    assert_eq!(value.minute(), 5);
    assert_eq!(value.second(), 6);
    assert_eq!(value.microsecond(), 7);
    assert_eq!(value.utc_offset_seconds(), Some(-18_000));
}

#[test]
fn instant_matches_python_timestamp_in_microseconds() {
    // datetime(2024, 1, 1, tzinfo=utc).timestamp() == 1704067200.0
    assert_eq!(
        dt(2024, 1, 1, 0, 0, 0, 0, Some(0)).instant(),
        1_704_067_200_000_000
    );
    // A naive value counts as UTC, so it lands on the same instant.
    assert_eq!(
        dt(2024, 1, 1, 0, 0, 0, 0, None).instant(),
        1_704_067_200_000_000
    );
    assert_eq!(dt(1969, 12, 31, 23, 59, 59, 999_999, Some(0)).instant(), -1);
    assert_eq!(
        dt(1, 1, 1, 0, 0, 0, 0, Some(0)).instant(),
        -62_135_596_800_000_000
    );
    assert_eq!(
        dt(9999, 12, 31, 23, 59, 59, 999_999, Some(0)).instant(),
        253_402_300_799_999_999
    );
}

#[test]
fn instant_is_equal_for_the_same_moment_written_at_different_offsets() {
    let utc = dt(2024, 1, 1, 10, 0, 0, 0, Some(0));
    let plus_two = dt(2024, 1, 1, 12, 0, 0, 0, Some(2 * 3600));
    let minus_five = dt(2024, 1, 1, 5, 0, 0, 0, Some(-5 * 3600));
    let naive = dt(2024, 1, 1, 10, 0, 0, 0, None);

    assert_eq!(utc.instant(), plus_two.instant());
    assert_eq!(utc.instant(), minus_five.instant());
    assert_eq!(utc.instant(), naive.instant());
}

#[test]
fn to_utc_rejects_a_value_python_astimezone_would_overflow_on() {
    // Real `astimezone(timezone.utc)` raises `OverflowError: date value out
    // of range` for both of these, so there is no normalized value to report.
    let last_day = Date::new(9999, 12, 31).expect("a real date");
    let first_day = Date::new(1, 1, 1).expect("a real date");

    assert_eq!(
        DateTime::new(last_day, 23, 0, 0, 0, Some(-3600))
            .expect("in range")
            .to_utc(),
        None
    );
    assert_eq!(
        DateTime::new(first_day, 0, 0, 0, 0, Some(5 * 3600))
            .expect("in range")
            .to_utc(),
        None
    );
    // One second inside the boundary on each side still normalizes.
    assert!(
        DateTime::new(last_day, 22, 59, 59, 0, Some(-3600))
            .expect("in range")
            .to_utc()
            .is_some()
    );
    assert!(
        DateTime::new(first_day, 5, 0, 0, 0, Some(5 * 3600))
            .expect("in range")
            .to_utc()
            .is_some()
    );
}

#[test]
fn python_str_is_isoformat_with_a_space_separator() {
    // `str(datetime)` is `isoformat(sep=" ")`; `str(date)` is `isoformat()`.
    assert_eq!(
        dt(2024, 1, 1, 10, 0, 0, 0, None).python_str(),
        "2024-01-01 10:00:00"
    );
    assert_eq!(
        dt(2024, 1, 1, 10, 0, 0, 123_456, None).python_str(),
        "2024-01-01 10:00:00.123456"
    );
    assert_eq!(
        dt(2024, 1, 1, 10, 0, 0, 0, Some(1830)).python_str(),
        "2024-01-01 10:00:00+00:30:30"
    );
    assert_eq!(
        dt(1, 1, 1, 0, 0, 0, 0, None).python_str(),
        "0001-01-01 00:00:00"
    );
    assert_eq!(date(2024, 1, 1).python_str(), "2024-01-01");
    assert_eq!(date(1, 1, 1).python_str(), "0001-01-01");
}

#[test]
fn to_utc_matches_python_astimezone_utc() {
    // datetime(2024, 1, 1, 12, tzinfo=+02:00).astimezone(utc)
    assert_eq!(
        dt(2024, 1, 1, 12, 0, 0, 0, Some(2 * 3600)).to_utc(),
        Some(dt(2024, 1, 1, 10, 0, 0, 0, Some(0)))
    );
    // A naive value is stamped with UTC, its wall clock untouched.
    assert_eq!(
        dt(2024, 1, 1, 10, 0, 0, 123_456, None).to_utc(),
        Some(dt(2024, 1, 1, 10, 0, 0, 123_456, Some(0)))
    );
    // Crossing back over midnight into the previous day.
    assert_eq!(
        dt(2024, 1, 1, 1, 0, 0, 0, Some(2 * 3600)).to_utc(),
        Some(dt(2023, 12, 31, 23, 0, 0, 0, Some(0)))
    );
    // An offset with seconds in it (Python allows any whole-second offset).
    assert_eq!(
        dt(2024, 1, 1, 10, 0, 0, 0, Some(1830)).to_utc(),
        Some(dt(2024, 1, 1, 9, 29, 30, 0, Some(0)))
    );
}

#[test]
fn to_utc_is_idempotent_and_always_aware() {
    let normalized = dt(2024, 6, 5, 4, 3, 2, 1, Some(-18_000))
        .to_utc()
        .expect("well inside the representable range");

    assert_eq!(normalized.utc_offset_seconds(), Some(0));
    assert_eq!(normalized.to_utc(), Some(normalized));
}

#[test]
fn datetime_isoformat_matches_python() {
    assert_eq!(
        dt(2024, 1, 1, 10, 0, 0, 0, None).isoformat(),
        "2024-01-01T10:00:00"
    );
    assert_eq!(
        dt(2024, 1, 1, 10, 0, 0, 123_456, None).isoformat(),
        "2024-01-01T10:00:00.123456"
    );
    assert_eq!(
        dt(2024, 1, 1, 10, 0, 0, 1, None).isoformat(),
        "2024-01-01T10:00:00.000001"
    );
    assert_eq!(
        dt(2024, 1, 1, 10, 0, 0, 0, Some(0)).isoformat(),
        "2024-01-01T10:00:00+00:00"
    );
    assert_eq!(
        dt(2024, 1, 1, 10, 0, 0, 0, Some(-5 * 3600 - 1800)).isoformat(),
        "2024-01-01T10:00:00-05:30"
    );
    assert_eq!(
        dt(2024, 1, 1, 10, 0, 0, 0, Some(1830)).isoformat(),
        "2024-01-01T10:00:00+00:30:30"
    );
    assert_eq!(
        dt(2024, 1, 1, 10, 0, 0, 0, Some(-1830)).isoformat(),
        "2024-01-01T10:00:00-00:30:30"
    );
    assert_eq!(
        dt(9999, 12, 31, 23, 59, 59, 999_999, Some(0)).isoformat(),
        "9999-12-31T23:59:59.999999+00:00"
    );
    assert_eq!(
        dt(1, 1, 1, 0, 0, 0, 0, None).isoformat(),
        "0001-01-01T00:00:00"
    );
}
