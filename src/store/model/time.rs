//! A UTC instant to the nanosecond, written as an RFC 3339 string: the type
//! of a decision's `created`.
//!
//! A load and a write give back the bytes earlier releases wrote with
//! chrono 0.4's `DateTime<Utc>`, so a migrated decision keeps its stamp:
//! the date and the time to the second, then the fewest of 0, 3, 6 or 9
//! fraction digits that hold the nanoseconds exactly, then `Z`
//! (`2025-06-01T10:30:00Z`, `2026-11-23T19:27:14.921666493Z`).
//!
//! Parsing accepts RFC 3339's `date-time` (section 5.6): `T` or `t`; 1 to 9
//! fraction digits; `Z`, `z` or a `+HH:MM` / `-HH:MM` offset, folded into
//! UTC. It refuses anything else, the leap second `:60` included, and any
//! instant outside the years 0000 to 9999.

use std::fmt;
use std::str::FromStr;
use std::time::{Duration, SystemTime};

use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize, Serializer};

use crate::Error;

const NANOS_PER_SECOND: u32 = 1_000_000_000;

const SECONDS_PER_DAY: i64 = 86_400;

/// 0000-01-01T00:00:00Z.
const MIN_SECS: i64 = -62_167_219_200;

/// 9999-12-31T23:59:59Z.
const MAX_SECS: i64 = 253_402_300_799;

/// A UTC instant in the years 0000 to 9999, to the nanosecond.
///
/// Held as seconds and nanoseconds since the Unix epoch, so the derived
/// order is time order and a value means the same on every platform, which
/// `SystemTime` does not promise (it resolves to 100 ns on Windows).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Timestamp {
    /// Negative before 1970, within `MIN_SECS..=MAX_SECS`.
    secs: i64,
    /// Below `NANOS_PER_SECOND`.
    nanos: u32,
}

/// The calendar date and time of day of a [`Timestamp`], to the second.
pub(crate) struct Civil {
    pub(crate) year: i64,
    pub(crate) month: i64,
    pub(crate) day: i64,
    pub(crate) hour: i64,
    pub(crate) minute: i64,
    pub(crate) second: i64,
}

/// Why a string is not a [`Timestamp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TimestampError {
    #[error(
        "not an RFC 3339 date-time such as 2025-06-01T10:30:00Z: expected {expected} at byte {at}"
    )]
    Syntax { expected: &'static str, at: usize },

    #[error("more than 9 fraction digits; a timestamp holds nanoseconds")]
    FractionTooLong,

    #[error("{field} {value:02} is out of range")]
    Field { field: &'static str, value: i64 },

    #[error("{year:04}-{month:02} has no day {day:02}")]
    NoSuchDay { year: i64, month: i64, day: i64 },

    #[error("second 60 is a leap second, which a timestamp cannot hold")]
    LeapSecond,

    #[error("the instant is outside the years 0000 to 9999 UTC")]
    OutOfRange,
}

impl Timestamp {
    /// 1970-01-01T00:00:00Z.
    pub(in crate::store) const UNIX_EPOCH: Self = Self { secs: 0, nanos: 0 };

    /// The system clock's reading: the one place the crate reads it, as the
    /// `disallowed-methods` lint on `SystemTime::now` enforces. Fails when
    /// the clock reads a time outside the years 0000 to 9999.
    pub(crate) fn now() -> crate::Result<Self> {
        #[expect(clippy::disallowed_methods, reason = "the crate's one clock read")]
        let reading = SystemTime::now();
        Self::try_from(reading).map_err(|source| Error::Clock { source })
    }

    /// `None` past 9999-12-31T23:59:59.999999999Z.
    #[must_use]
    pub(in crate::store) fn checked_add(self, duration: Duration) -> Option<Self> {
        let mut secs = self
            .secs
            .checked_add(i64::try_from(duration.as_secs()).ok()?)?;
        let mut nanos = self.nanos + duration.subsec_nanos();
        if nanos >= NANOS_PER_SECOND {
            nanos -= NANOS_PER_SECOND;
            secs = secs.checked_add(1)?;
        }
        Self::new(secs, nanos)
    }

    /// `None` before 0000-01-01T00:00:00Z.
    #[must_use]
    fn checked_sub(self, duration: Duration) -> Option<Self> {
        let mut secs = self
            .secs
            .checked_sub(i64::try_from(duration.as_secs()).ok()?)?;
        let mut nanos = self.nanos;
        if nanos < duration.subsec_nanos() {
            nanos += NANOS_PER_SECOND;
            secs = secs.checked_sub(1)?;
        }
        Self::new(secs, nanos - duration.subsec_nanos())
    }

    pub(crate) fn civil(self) -> Civil {
        let second_of_day = self.secs.rem_euclid(SECONDS_PER_DAY);
        let (year, month, day) = civil_from_days(self.secs.div_euclid(SECONDS_PER_DAY));
        Civil {
            year,
            month,
            day,
            hour: second_of_day / 3600,
            minute: second_of_day / 60 % 60,
            second: second_of_day % 60,
        }
    }

    /// `nanos` is below `NANOS_PER_SECOND`; `None` when `secs` is out of range.
    fn new(secs: i64, nanos: u32) -> Option<Self> {
        (MIN_SECS..=MAX_SECS)
            .contains(&secs)
            .then_some(Self { secs, nanos })
    }
}

impl fmt::Display for Timestamp {
    /// The on-disk form the module doc gives. The year has four digits
    /// because the range ends at 9999.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Civil {
            year,
            month,
            day,
            hour,
            minute,
            second,
        } = self.civil();
        write!(
            f,
            "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}"
        )?;
        match self.nanos {
            0 => {}
            nanos if nanos.is_multiple_of(1_000_000) => write!(f, ".{:03}", nanos / 1_000_000)?,
            nanos if nanos.is_multiple_of(1_000) => write!(f, ".{:06}", nanos / 1_000)?,
            nanos => write!(f, ".{nanos:09}")?,
        }
        f.write_str("Z")
    }
}

impl fmt::Debug for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl FromStr for Timestamp {
    type Err = TimestampError;

    fn from_str(text: &str) -> Result<Self, TimestampError> {
        let mut cursor = Cursor {
            bytes: text.as_bytes(),
            at: 0,
        };
        let year = cursor.number(4)?;
        cursor.literal(b"-", "`-`")?;
        let month = cursor.number(2)?;
        cursor.literal(b"-", "`-`")?;
        let day = cursor.number(2)?;
        cursor.literal(b"Tt", "`T`")?;
        let hour = cursor.number(2)?;
        cursor.literal(b":", "`:`")?;
        let minute = cursor.number(2)?;
        cursor.literal(b":", "`:`")?;
        let second = cursor.number(2)?;
        let nanos = cursor.fraction()?;
        let east = cursor.offset()?;
        if cursor.peek().is_some() {
            return Err(cursor.expected("the end"));
        }
        let days = days_from_date(year, month, day)?;
        let secs = days * SECONDS_PER_DAY + second_of_day(hour, minute, second)? - east;
        Self::new(secs, nanos).ok_or(TimestampError::OutOfRange)
    }
}

impl TryFrom<SystemTime> for Timestamp {
    type Error = TimestampError;

    fn try_from(time: SystemTime) -> Result<Self, TimestampError> {
        match time.duration_since(SystemTime::UNIX_EPOCH) {
            Ok(after) => Self::UNIX_EPOCH.checked_add(after),
            Err(before) => Self::UNIX_EPOCH.checked_sub(before.duration()),
        }
        .ok_or(TimestampError::OutOfRange)
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_str(TimestampVisitor)
    }
}

struct TimestampVisitor;

impl Visitor<'_> for TimestampVisitor {
    type Value = Timestamp;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an RFC 3339 date-time string such as 2025-06-01T10:30:00Z")
    }

    fn visit_str<E: de::Error>(self, text: &str) -> Result<Timestamp, E> {
        text.parse().map_err(E::custom)
    }
}

/// A read position in the text being parsed.
struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Cursor<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn expected(&self, expected: &'static str) -> TimestampError {
        TimestampError::Syntax {
            expected,
            at: self.at,
        }
    }

    /// Exactly `digits` ASCII digits, so the number is below `10^digits`.
    fn number(&mut self, digits: usize) -> Result<i64, TimestampError> {
        let mut number = 0;
        for _ in 0..digits {
            let Some(digit @ b'0'..=b'9') = self.peek() else {
                return Err(self.expected("a digit"));
            };
            number = number * 10 + i64::from(digit - b'0');
            self.at += 1;
        }
        Ok(number)
    }

    /// One of `accepted`, which `expected` describes.
    fn literal(&mut self, accepted: &[u8], expected: &'static str) -> Result<u8, TimestampError> {
        match self.peek() {
            Some(byte) if accepted.contains(&byte) => {
                self.at += 1;
                Ok(byte)
            }
            _ => Err(self.expected(expected)),
        }
    }

    /// An optional `.` and 1 to 9 digits, as nanoseconds.
    fn fraction(&mut self) -> Result<u32, TimestampError> {
        if self.peek() != Some(b'.') {
            return Ok(0);
        }
        self.at += 1;
        let mut nanos = 0;
        let mut scale = NANOS_PER_SECOND;
        while let Some(digit @ b'0'..=b'9') = self.peek() {
            if scale == 1 {
                return Err(TimestampError::FractionTooLong);
            }
            scale /= 10;
            nanos += u32::from(digit - b'0') * scale;
            self.at += 1;
        }
        if scale == NANOS_PER_SECOND {
            return Err(self.expected("a digit"));
        }
        Ok(nanos)
    }

    /// `Z`, `z`, `+HH:MM` or `-HH:MM`, as seconds east of UTC.
    fn offset(&mut self) -> Result<i64, TimestampError> {
        let sign = self.literal(b"Zz+-", "`Z` or a UTC offset")?;
        if sign.eq_ignore_ascii_case(&b'z') {
            return Ok(0);
        }
        let hours = self.number(2)?;
        self.literal(b":", "`:`")?;
        let minutes = self.number(2)?;
        in_range("offset hour", hours, 23)?;
        in_range("offset minute", minutes, 59)?;
        let east = hours * 3600 + minutes * 60;
        Ok(if sign == b'-' { -east } else { east })
    }
}

fn in_range(field: &'static str, value: i64, max: i64) -> Result<(), TimestampError> {
    if value > max {
        return Err(TimestampError::Field { field, value });
    }
    Ok(())
}

/// Days since 1970-01-01 of a valid date of the proleptic Gregorian calendar.
fn days_from_date(year: i64, month: i64, day: i64) -> Result<i64, TimestampError> {
    if !(1..=12).contains(&month) {
        return Err(TimestampError::Field {
            field: "month",
            value: month,
        });
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if !(1..=days_in_month).contains(&day) {
        return Err(TimestampError::NoSuchDay { year, month, day });
    }
    Ok(days_from_civil(year, month, day))
}

fn second_of_day(hour: i64, minute: i64, second: i64) -> Result<i64, TimestampError> {
    if second == 60 {
        return Err(TimestampError::LeapSecond);
    }
    in_range("hour", hour, 23)?;
    in_range("minute", minute, 59)?;
    in_range("second", second, 59)?;
    Ok(hour * 3600 + minute * 60 + second)
}

// `days_from_civil` and `civil_from_days` are Howard Hinnant's algorithms
// from "chrono-Compatible Low-Level Date Algorithms"
// (https://howardhinnant.github.io/date_algorithms.html), with Euclidean
// division in place of the branches for negative eras. Each counts a year
// from March 1, so the leap day closes it.

/// Days since 1970-01-01 of a date with `month` in 1..=12 and `day` in
/// 1..=31.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// `(year, month, day)` of the date `days` after 1970-01-01.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = if month_from_march < 10 {
        month_from_march + 3
    } else {
        month_from_march - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::seeded::SplitMix64;

    fn at(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    /// `(input, as written, Unix seconds, nanoseconds)`. The seconds are
    /// GNU `date -u -d <input> +%s`, independent of the code under test.
    const ACCEPTED: &[(&str, &str, i64, u32)] = &[
        ("1970-01-01T00:00:00Z", "1970-01-01T00:00:00Z", 0, 0),
        ("0000-01-01T00:00:00Z", "0000-01-01T00:00:00Z", MIN_SECS, 0),
        (
            "9999-12-31T23:59:59.999999999Z",
            "9999-12-31T23:59:59.999999999Z",
            MAX_SECS,
            999_999_999,
        ),
        (
            "2024-02-29T00:00:00Z",
            "2024-02-29T00:00:00Z",
            1_709_164_800,
            0,
        ),
        (
            "2000-03-01T00:00:00Z",
            "2000-03-01T00:00:00Z",
            951_868_800,
            0,
        ),
        (
            "1900-03-01T00:00:00Z",
            "1900-03-01T00:00:00Z",
            -2_203_891_200,
            0,
        ),
        (
            "1600-02-29T00:00:00Z",
            "1600-02-29T00:00:00Z",
            -11_670_998_400,
            0,
        ),
        (
            "0001-01-01T00:00:00Z",
            "0001-01-01T00:00:00Z",
            -62_135_596_800,
            0,
        ),
        (
            "0000-02-29T00:00:00Z",
            "0000-02-29T00:00:00Z",
            -62_162_121_600,
            0,
        ),
        (
            "1969-12-31T23:59:59.5Z",
            "1969-12-31T23:59:59.500Z",
            -1,
            500_000_000,
        ),
        // Fractions: the fewest of 0, 3, 6 or 9 digits that hold the value.
        (
            "2025-06-01T10:30:00.0Z",
            "2025-06-01T10:30:00Z",
            1_748_773_800,
            0,
        ),
        (
            "2025-06-01T10:30:00.000000000Z",
            "2025-06-01T10:30:00Z",
            1_748_773_800,
            0,
        ),
        (
            "2025-06-01T10:30:00.5Z",
            "2025-06-01T10:30:00.500Z",
            1_748_773_800,
            500_000_000,
        ),
        (
            "2025-06-01T10:30:00.120Z",
            "2025-06-01T10:30:00.120Z",
            1_748_773_800,
            120_000_000,
        ),
        (
            "2025-06-01T10:30:00.1234Z",
            "2025-06-01T10:30:00.123400Z",
            1_748_773_800,
            123_400_000,
        ),
        (
            "2025-06-01T10:30:00.100000Z",
            "2025-06-01T10:30:00.100Z",
            1_748_773_800,
            100_000_000,
        ),
        (
            "2025-06-01T10:30:00.000001Z",
            "2025-06-01T10:30:00.000001Z",
            1_748_773_800,
            1_000,
        ),
        (
            "2025-06-01T10:30:00.0000001Z",
            "2025-06-01T10:30:00.000000100Z",
            1_748_773_800,
            100,
        ),
        (
            "2025-06-01T10:30:00.000000001Z",
            "2025-06-01T10:30:00.000000001Z",
            1_748_773_800,
            1,
        ),
        (
            "2026-11-23T19:27:14.921666493Z",
            "2026-11-23T19:27:14.921666493Z",
            1_795_462_034,
            921_666_493,
        ),
        // Lowercase separators and offsets, folded into UTC.
        (
            "2025-06-01t10:30:00z",
            "2025-06-01T10:30:00Z",
            1_748_773_800,
            0,
        ),
        (
            "2025-06-01T12:30:00+02:00",
            "2025-06-01T10:30:00Z",
            1_748_773_800,
            0,
        ),
        (
            "2025-06-01T09:00:00-01:30",
            "2025-06-01T10:30:00Z",
            1_748_773_800,
            0,
        ),
        (
            "2025-06-01T10:30:00-00:00",
            "2025-06-01T10:30:00Z",
            1_748_773_800,
            0,
        ),
        (
            "2025-01-01T00:30:00+01:00",
            "2024-12-31T23:30:00Z",
            1_735_687_800,
            0,
        ),
        (
            "2024-02-28T23:30:00-01:00",
            "2024-02-29T00:30:00Z",
            1_709_166_600,
            0,
        ),
    ];

    #[test]
    fn accepted_forms_parse_to_their_instant_and_print_in_the_on_disk_form() {
        for &(input, written, secs, nanos) in ACCEPTED {
            let parsed = at(input);
            assert_eq!((parsed.secs, parsed.nanos), (secs, nanos), "{input}");
            assert_eq!(parsed.to_string(), written, "{input}");
            assert_eq!(at(written), parsed, "{written}");
        }
    }

    #[test]
    fn malformed_and_impossible_instants_are_refused() {
        use TimestampError::{FractionTooLong, LeapSecond, NoSuchDay, OutOfRange};
        let syntax = |expected, at| TimestampError::Syntax { expected, at };
        let field = |field, value| TimestampError::Field { field, value };
        let day = |year, month, day| NoSuchDay { year, month, day };
        let cases: &[(&str, TimestampError)] = &[
            ("", syntax("a digit", 0)),
            ("2025-06-01", syntax("`T`", 10)),
            ("2025-06-01 10:30:00Z", syntax("`T`", 10)),
            ("2025-6-01T10:30:00Z", syntax("a digit", 6)),
            ("+2025-06-01T10:30:00Z", syntax("a digit", 0)),
            ("-0001-06-01T10:30:00Z", syntax("a digit", 0)),
            ("12025-06-01T10:30:00Z", syntax("`-`", 4)),
            ("2025-06-01T10:30Z", syntax("`:`", 16)),
            ("2025-06-01T10:30:00", syntax("`Z` or a UTC offset", 19)),
            ("2025-06-01T10:30:00 Z", syntax("`Z` or a UTC offset", 19)),
            ("2025-06-01T10:30:00UTC", syntax("`Z` or a UTC offset", 19)),
            ("2025-06-01T10:30:00ZZ", syntax("the end", 20)),
            ("2025-06-01T10:30:00Z ", syntax("the end", 20)),
            ("2025-06-01T10:30:00.Z", syntax("a digit", 20)),
            ("2025-06-01T10:30:00,5Z", syntax("`Z` or a UTC offset", 19)),
            ("2025-06-01T10:30:00+0200", syntax("`:`", 22)),
            ("2025-06-01T10:30:00+02", syntax("`:`", 22)),
            ("2025-06-01T10:30:00+02:00:00", syntax("the end", 25)),
            (
                "2025-06-01T10:30:00\u{ff3a}",
                syntax("`Z` or a UTC offset", 19),
            ),
            ("2025-06-01T10:30:00.1234567890Z", FractionTooLong),
            ("2025-13-01T10:30:00Z", field("month", 13)),
            ("2025-00-01T10:30:00Z", field("month", 0)),
            ("2025-04-31T10:30:00Z", day(2025, 4, 31)),
            ("2025-01-00T10:30:00Z", day(2025, 1, 0)),
            ("2025-02-29T10:30:00Z", day(2025, 2, 29)),
            ("2100-02-29T00:00:00Z", day(2100, 2, 29)),
            ("2025-06-01T24:00:00Z", field("hour", 24)),
            ("2025-06-01T10:60:00Z", field("minute", 60)),
            ("2025-06-01T10:30:61Z", field("second", 61)),
            ("2016-12-31T23:59:60Z", LeapSecond),
            ("2025-06-01T10:30:00+24:00", field("offset hour", 24)),
            ("2025-06-01T10:30:00+01:60", field("offset minute", 60)),
            ("9999-12-31T23:59:59-00:01", OutOfRange),
            ("0000-01-01T00:00:00+00:01", OutOfRange),
        ];
        for (input, refusal) in cases {
            assert_eq!(input.parse::<Timestamp>(), Err(*refusal), "{input}");
        }
    }

    #[test]
    fn random_instants_round_trip_through_their_string() {
        let mut rng = SplitMix64::new(0x7469_6d65);
        let span = usize::try_from(MAX_SECS - MIN_SECS).unwrap();
        for _ in 0..20_000 {
            let secs = MIN_SECS + i64::try_from(rng.below(span + 1)).unwrap();
            // Whole seconds, milli-, micro- and nanoseconds alike.
            let unit = [NANOS_PER_SECOND, 1_000_000, 1_000, 1][rng.below(4)];
            let steps = usize::try_from(NANOS_PER_SECOND / unit).unwrap();
            let nanos = u32::try_from(rng.below(steps)).unwrap() * unit;
            let instant = Timestamp { secs, nanos };
            let written = instant.to_string();
            assert_eq!(at(&written), instant, "{written}");
            assert!([20, 24, 27, 30].contains(&written.len()), "{written}");
        }
    }

    #[test]
    fn consecutive_days_are_consecutive_dates() {
        let mut rng = SplitMix64::new(0x6461_7973);
        let first = MIN_SECS.div_euclid(SECONDS_PER_DAY);
        let last = MAX_SECS.div_euclid(SECONDS_PER_DAY);
        let span = usize::try_from(last - first).unwrap();
        for _ in 0..20_000 {
            let days = first + i64::try_from(rng.below(span)).unwrap();
            let (year, month, day) = civil_from_days(days);
            let next = civil_from_days(days + 1);
            assert!(
                next == (year, month, day + 1)
                    || next == (year, month + 1, 1)
                    || next == (year + 1, 1, 1),
                "{days}: {year}-{month}-{day} then {next:?}"
            );
            assert_eq!(days_from_date(year, month, day), Ok(days));
        }
    }

    #[test]
    fn arithmetic_stops_at_the_ends_of_the_range() {
        let max = at("9999-12-31T23:59:59.999999999Z");
        let min = at("0000-01-01T00:00:00Z");
        let nano = Duration::from_nanos(1);
        assert_eq!(max.checked_add(nano), None);
        assert_eq!(min.checked_sub(nano), None);
        assert_eq!(min.checked_add(Duration::MAX), None);
        assert_eq!(max.checked_sub(Duration::MAX), None);
        assert_eq!(
            at("2025-06-01T23:59:59.75Z").checked_add(Duration::from_millis(500)),
            Some(at("2025-06-02T00:00:00.25Z"))
        );
        assert_eq!(
            at("2025-06-02T00:00:00.25Z").checked_sub(Duration::from_millis(500)),
            Some(at("2025-06-01T23:59:59.75Z"))
        );
    }

    #[test]
    fn system_time_converts_on_both_sides_of_the_epoch() {
        let epoch = SystemTime::UNIX_EPOCH;
        assert_eq!(
            Timestamp::try_from(epoch + Duration::new(1, 5)),
            Ok(at("1970-01-01T00:00:01.000000005Z"))
        );
        assert_eq!(
            Timestamp::try_from(epoch - Duration::from_nanos(1)),
            Ok(at("1969-12-31T23:59:59.999999999Z"))
        );
        assert_eq!(
            Timestamp::try_from(epoch + Duration::from_secs(253_402_300_800)),
            Err(TimestampError::OutOfRange)
        );
    }

    #[test]
    fn a_toml_value_that_is_not_a_timestamp_string_is_refused_with_the_fault() {
        #[derive(Debug, Deserialize, Serialize)]
        struct Stamped {
            at: Timestamp,
        }
        let written = toml::to_string(&Stamped {
            at: at("2025-06-01T10:30:00.5Z"),
        })
        .unwrap();
        assert_eq!(written, "at = \"2025-06-01T10:30:00.500Z\"\n");

        let impossible = toml::from_str::<Stamped>("at = \"2100-02-29T00:00:00Z\"").unwrap_err();
        assert!(
            impossible.to_string().contains("2100-02 has no day 29"),
            "{impossible}"
        );
        // A TOML datetime literal is not a string, and 0.3 releases refused it too.
        assert!(toml::from_str::<Stamped>("at = 2025-06-01T10:30:00Z").is_err());
        assert!(toml::from_str::<Stamped>("at = 1748773800").is_err());
    }
}
