//! Date/time canonical-unit parsing — closes the loop `forms.rs` left open
//! (see `forms.rs` module docs: "Date/time parsing into canonical units —
//! lives in `date_units` until a proper parser lands"). This is that parser.
//!
//! The HTML5 form types `date`, `time`, `week`, `month`, and
//! `datetime-local` parse into the [`DateTimeUnit`] canonical scalars that
//! `forms::step_mismatch` consumes, so `stepMismatch` is testable end-to-end
//! without dragging in a full datetime library.
//!
//! # Algorithms
//!
//! Days-since-epoch uses Howard Hinnant's `days_from_civil` (proleptic
//! Gregorian, public-domain, O(1), no overflow for any plausible year). The
//! inverse is not needed for `step` arithmetic. Reference:
//! <https://howardhinnant.github.io/date_algorithms.html>.
//!
//! Validation rules follow HTML5 § 4.10.5.1.7–4.10.5.1.13 (the "minimun
//! parser" for each type): two-digit zero-padded fields, valid ranges, no
//! trailing garbage. Leap seconds are not modelled.

#![forbid(unsafe_code)]

pub use crate::forms::DateTimeUnit;

/// `days_from_civil(y, m, d)` — days since 1970-01-01 (Hinnant). Proleptic
/// Gregorian; `m` is 1-12, `d` is 1-31. No range validation here (callers
/// validate before converting) so the algorithm stays a pure function.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

// ---------------------------------------------------------------------------
// Field parsers (HTML5 "minimum" parsers — zero-padded digits, fixed width)
// ---------------------------------------------------------------------------

/// Parse exactly `n` ASCII digits; returns `None` on any non-digit or short
/// run. HTML5 date/time fields are fixed-width and zero-padded.
fn digits(s: &str, n: usize) -> Option<(i64, &str)> {
    if s.len() < n {
        return None;
    }
    let bytes = &s.as_bytes()[..n];
    if !bytes.iter().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let v = bytes
        .iter()
        .fold(0i64, |acc, &b| acc * 10 + (b - b'0') as i64);
    Some((v, &s[n..]))
}

/// Reject if `rest` is non-empty (HTML5 parsers consume the whole string).
fn end(rest: &str) -> Option<()> {
    if rest.is_empty() { Some(()) } else { None }
}

fn lit(s: &str, ch: char) -> Option<&str> {
    let mut iter = s.chars();
    if iter.next() == Some(ch) {
        Some(&s[ch.len_utf8()..])
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Type parsers → DateTimeUnit
// ---------------------------------------------------------------------------

/// `<input type=date>` — `YYYY-MM-DD` → days since epoch.
/// HTML5 § 4.10.5.1.7. Year 1+, month 1-12, day valid for the month/year
/// (incl. leap years).
pub fn parse_date(s: &str) -> Option<DateTimeUnit> {
    let (year, rest) = digits(s, 4)?;
    let rest = lit(rest, '-')?;
    let (month, rest) = digits(rest, 2)?;
    let rest = lit(rest, '-')?;
    let (day, rest) = digits(rest, 2)?;
    end(rest)?;
    validate_ymd(year, month, day)?;
    Some(DateTimeUnit::Days(days_from_civil(year, month, day)))
}

/// `<input type=time>` — `HH:MM[:SS[.sss]]` → seconds since midnight.
/// HTML5 § 4.10.5.1.10. Hours 00-23, minutes/seconds 00-59. Fractional
/// seconds (`.fff`) are accepted and folded into the second count (truncated
/// to whole seconds, matching the canonical-unit contract in `forms.rs`).
pub fn parse_time(s: &str) -> Option<DateTimeUnit> {
    let (hour, rest) = digits(s, 2)?;
    let rest = lit(rest, ':')?;
    let (minute, rest) = digits(rest, 2)?;
    let mut rest = rest;
    let mut seconds = 0i64;
    if rest.starts_with(':') {
        rest = lit(rest, ':')?;
        let (sec, after) = digits(rest, 2)?;
        seconds = sec;
        rest = after;
        // Optional fractional seconds: '.' then 1-3 digits (truncated).
        if let Some(after_dot) = rest.strip_prefix('.') {
            let upto = after_dot
                .as_bytes()
                .iter()
                .take_while(|b| b.is_ascii_digit())
                .count();
            if upto == 0 {
                return None;
            }
            rest = &after_dot[upto..];
        }
    }
    end(rest)?;
    if !(0..=23).contains(&hour) || !(0..=59).contains(&minute) || !(0..=59).contains(&seconds) {
        return None;
    }
    Some(DateTimeUnit::Seconds(hour * 3600 + minute * 60 + seconds))
}

/// `<input type=week>` — `YYYY-Www` → weeks since epoch (ISO 8601 week date).
/// HTML5 § 4.10.5.1.9. Year ≥ 1, week 01-53 per ISO 8601. The week epoch is
/// the Thursday of week 1 of 1970 (1970-W01 = 1969-12-29 .. 1970-01-04),
/// i.e. days_from_civil(1969,12,29) = -3; weeks = (this_thursday_days - (-3)) / 7.
pub fn parse_week(s: &str) -> Option<DateTimeUnit> {
    let (year, rest) = digits(s, 4)?;
    let rest = lit(rest, '-')?;
    let rest = lit(rest, 'W')?;
    let (week, rest) = digits(rest, 2)?;
    end(rest)?;
    if year < 1 || !(1..=53).contains(&week) {
        return None;
    }
    // Validate the week exists for this year: a year has 53 weeks iff it
    // starts on Thursday, or (starts on Wednesday and is a leap year).
    if week == 53 && !has_iso_week_53(year) {
        return None;
    }
    // Anchor: epoch days of Monday of week 1. ISO week 1 contains the
    // year's first Thursday; Jan 4 is always in week 1. The Monday of the
    // week containing a day = day − offset, where offset is the count of days
    // *since* that Monday (0 for Mon .. 6 for Sun), i.e. `(wd + 6) % 7`.
    let jan4_days = days_from_civil(year, 1, 4);
    // jan4 weekday: 0=Sun..6=Sat (epoch 1970-01-01 was Thursday = 4).
    let jan4_wd = ((jan4_days % 7) + 4) % 7; // 0=Sun..6=Sat
    let week1_monday = jan4_days - (jan4_wd + 6) % 7;
    // Weeks since 1970-01-01 epoch Monday (1969-12-29, days=-3).
    let weeks = (week1_monday + (week - 1) * 7 - (-3)) / 7;
    Some(DateTimeUnit::Weeks(weeks))
}

fn has_iso_week_53(year: i64) -> bool {
    let jan1_days = days_from_civil(year, 1, 1);
    let jan1_wd = ((jan1_days % 7) + 4) % 7; // 0=Sun..6=Sat
    let dec31_days = days_from_civil(year, 12, 31);
    let dec31_wd = ((dec31_days % 7) + 4) % 7;
    // ISO: 53 weeks iff Jan 1 is Thu, or Dec 31 is Thu (leap-Wed years).
    jan1_wd == 4 || dec31_wd == 4
}

/// `<input type=month>` — `YYYY-MM` → months since year 0.
/// HTML5 § 4.10.5.1.8. Year ≥ 1, month 1-12. The "months since year 0" base
/// matches `forms.rs` docs (the canonical unit for `month`); year 0 is the
/// proleptic-Gregorian convention used by the algorithm.
pub fn parse_month(s: &str) -> Option<DateTimeUnit> {
    let (year, rest) = digits(s, 4)?;
    let rest = lit(rest, '-')?;
    let (month, rest) = digits(rest, 2)?;
    end(rest)?;
    if year < 1 || !(1..=12).contains(&month) {
        return None;
    }
    Some(DateTimeUnit::Months(year * 12 + (month - 1)))
}

/// `<input type=datetime-local>` — `YYYY-MM-DDTHH:MM[:SS]` → epoch seconds.
/// HTML5 § 4.10.5.1.6. A date + local-time joined by `T` (or a single space,
/// accepted leniently). Local time (no timezone); seconds optional.
pub fn parse_datetime_local(s: &str) -> Option<DateTimeUnit> {
    let (year, rest) = digits(s, 4)?;
    let rest = lit(rest, '-')?;
    let (month, rest) = digits(rest, 2)?;
    let rest = lit(rest, '-')?;
    let (day, rest) = digits(rest, 2)?;
    let rest = lit(rest, 'T').or_else(|| lit(rest, ' '))?;
    let (hour, rest) = digits(rest, 2)?;
    let rest = lit(rest, ':')?;
    let (minute, mut rest) = digits(rest, 2)?;
    let mut seconds = 0i64;
    if let Some(after) = rest.strip_prefix(':') {
        let (sec, after2) = digits(after, 2)?;
        seconds = sec;
        rest = after2;
    }
    end(rest)?;
    validate_ymd(year, month, day)?;
    if !(0..=23).contains(&hour) || !(0..=59).contains(&minute) || !(0..=59).contains(&seconds) {
        return None;
    }
    let days = days_from_civil(year, month, day);
    Some(DateTimeUnit::DateTimeSeconds(
        days * 86400 + hour * 3600 + minute * 60 + seconds,
    ))
}

fn validate_ymd(year: i64, month: i64, day: i64) -> Option<()> {
    if year < 1 || !(1..=12).contains(&month) {
        return None;
    }
    let dim = days_in_month(year, month);
    if !(1..=dim).contains(&day) {
        return None;
    }
    Some(())
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- days_from_civil anchors --------------------------------------

    #[test]
    fn epoch_is_zero() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        assert_eq!(days_from_civil(1971, 1, 1), 365); // 1970 not a leap year
    }

    #[test]
    fn known_dates() {
        // 2000-01-01 = 10957 days after epoch.
        assert_eq!(days_from_civil(2000, 1, 1), 10957);
        // 2024-03-01: 24 years (6 leap days: 2000..2020) + Jan(31) + Feb(29, leap).
        // 2000-01-01(10957) + 8766 + 60 = 19783.
        assert_eq!(days_from_civil(2024, 3, 1), 19783);
    }

    // --- date ----------------------------------------------------------

    #[test]
    fn date_valid_and_invalid() {
        assert_eq!(
            parse_date("2024-02-29").unwrap(),
            DateTimeUnit::Days(days_from_civil(2024, 2, 29)) // leap day ok
        );
        // 2023 is not a leap year → Feb 29 invalid.
        assert_eq!(parse_date("2023-02-29"), None);
        // Bad month/day.
        assert_eq!(parse_date("2024-13-01"), None);
        assert_eq!(parse_date("2024-04-31"), None); // April has 30 days
        // Wrong width / garbage.
        assert_eq!(parse_date("24-02-29"), None);
        assert_eq!(parse_date("2024-2-29"), None);
        assert_eq!(parse_date("2024-02-29 "), None);
        assert_eq!(parse_date(""), None);
    }

    #[test]
    fn date_year_zero_rejected() {
        assert_eq!(parse_date("0000-01-01"), None);
        assert_eq!(
            parse_date("0001-01-01").unwrap(),
            DateTimeUnit::Days(days_from_civil(1, 1, 1))
        );
    }

    // --- time ----------------------------------------------------------

    #[test]
    fn time_with_and_without_seconds() {
        assert_eq!(
            parse_time("13:45").unwrap(),
            DateTimeUnit::Seconds(13 * 3600 + 45 * 60)
        );
        assert_eq!(parse_time("00:00:00").unwrap(), DateTimeUnit::Seconds(0));
        assert_eq!(
            parse_time("23:59:59").unwrap(),
            DateTimeUnit::Seconds(86399)
        );
    }

    #[test]
    fn time_fractional_seconds_truncated_to_canonical_unit() {
        // forms.rs step arithmetic runs on whole seconds; fractional folds.
        assert_eq!(
            parse_time("00:00:00.500").unwrap(),
            DateTimeUnit::Seconds(0)
        );
        assert_eq!(
            parse_time("00:00:01.999").unwrap(),
            DateTimeUnit::Seconds(1)
        );
        // Dot must be followed by at least one digit.
        assert_eq!(parse_time("00:00:00."), None);
    }

    #[test]
    fn time_invalid_ranges_and_format() {
        assert_eq!(parse_time("24:00"), None);
        assert_eq!(parse_time("12:60"), None);
        assert_eq!(parse_time("12:00:60"), None);
        assert_eq!(parse_time("1:00"), None); // not zero-padded
        assert_eq!(parse_time("12-00"), None);
    }

    // --- month ---------------------------------------------------------

    #[test]
    fn month_canonical_units() {
        // Year 0 = month 0; year 1 month 1 = month 12.
        assert_eq!(parse_month("0001-01").unwrap(), DateTimeUnit::Months(12));
        assert_eq!(
            parse_month("1970-01").unwrap(),
            DateTimeUnit::Months(1970 * 12)
        );
        assert_eq!(
            parse_month("2024-12").unwrap(),
            DateTimeUnit::Months(2024 * 12 + 11)
        );
        assert_eq!(parse_month("2024-00"), None);
        assert_eq!(parse_month("2024-13"), None);
    }

    // --- week ----------------------------------------------------------

    #[test]
    fn week_basic_and_invalid() {
        // 1970-W01 corresponds to the epoch week (1969-12-29 .. 1970-01-04).
        assert_eq!(parse_week("1970-W01").unwrap(), DateTimeUnit::Weeks(0));
        // Week 00 never exists; week 54 never exists.
        assert_eq!(parse_week("2024-W00"), None);
        assert_eq!(parse_week("2024-W54"), None);
        // Format.
        assert_eq!(parse_week("2024W01"), None);
        assert_eq!(parse_week("2024-w01"), None); // uppercase W only
    }

    #[test]
    fn week_53_only_when_iso_allows() {
        // 2020 has 53 weeks (Jan 1 was Wednesday, leap year).
        assert!(parse_week("2020-W53").is_some());
        // 2021 has only 52 weeks (Jan 1 was Friday).
        assert_eq!(parse_week("2021-W53"), None);
        // 2026 has 53 weeks (Jan 1 is Thursday).
        assert!(parse_week("2026-W53").is_some());
    }

    // --- datetime-local -----------------------------------------------

    #[test]
    fn datetime_local_with_and_without_seconds() {
        let epoch_day = days_from_civil(1970, 1, 1);
        assert_eq!(
            parse_datetime_local("1970-01-01T00:00").unwrap(),
            DateTimeUnit::DateTimeSeconds(epoch_day * 86400)
        );
        assert_eq!(
            parse_datetime_local("1970-01-01T01:02:03").unwrap(),
            DateTimeUnit::DateTimeSeconds(epoch_day * 86400 + 3723)
        );
        // Space separator accepted leniently.
        assert!(parse_datetime_local("1970-01-01 01:02").is_some());
    }

    #[test]
    fn datetime_local_invalid() {
        assert_eq!(parse_datetime_local("1970-01-01"), None); // missing time
        assert_eq!(parse_datetime_local("1970-13-01T00:00"), None);
        assert_eq!(parse_datetime_local("1970-01-01T25:00"), None);
    }

    // --- step integration (closes the forms.rs loop) ------------------

    #[test]
    fn step_arithmetic_on_parsed_canonical_units() {
        // step=1 day from base 1970-01-01: 1970-01-02 ok, 1970-01-02 12:00
        // has no time component in `date` so always lands on a day boundary.
        let base = parse_date("1970-01-01").unwrap().as_scalar();
        let v = parse_date("1970-01-02").unwrap().as_scalar();
        assert!(!step_mismatch(base, v, 1));
        // For `time`, default step is 60s: 00:01 ok, 00:00:30 mismatch.
        let tbase = parse_time("00:00").unwrap().as_scalar();
        assert!(!step_mismatch(
            tbase,
            parse_time("00:01").unwrap().as_scalar(),
            60
        ));
        assert!(step_mismatch(
            tbase,
            parse_time("00:00:30").unwrap().as_scalar(),
            60
        ));
    }

    // Re-export step_mismatch from forms for the integration test so the
    // module under test only names its own symbols.
    use crate::forms::step_mismatch;
}
