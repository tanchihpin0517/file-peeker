//! How a size and a modification time read on screen.
//!
//! Here rather than in either front end because both show the same values, and a
//! terminal cell and a painted galley must not disagree about how big a file is.

// `format_modified` and its calendar arithmetic serve the desktop front end only, so
// a terminal-only build reaches the size half of this module and not the time half.
#![cfg_attr(not(feature = "gui"), allow(dead_code))]

use std::time::{SystemTime, UNIX_EPOCH};

const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];

/// A size in as few characters as it can be read in.
///
/// Powers of 1024, and one decimal only below 10 of a unit, where it is the
/// difference between `1K` and `1.5K`. Above that the decimal is noise in a column
/// this narrow.
///
/// All integer arithmetic: a `u64` size does not fit an `f64` mantissa, and the one
/// decimal is a remainder rather than a rounding.
#[must_use]
pub(crate) fn format_size(bytes: u64) -> String {
    let mut unit: u32 = 0;
    let mut whole = bytes;
    while whole >= 1024 && (unit as usize) + 1 < UNITS.len() {
        whole /= 1024;
        unit += 1;
    }
    let suffix = UNITS[unit as usize];
    if unit == 0 {
        return format!("{bytes} {suffix}");
    }
    if whole < 10 {
        let divisor = 1024_u64.pow(unit);
        let tenths = (bytes % divisor) * 10 / divisor;
        format!("{whole}.{tenths} {suffix}")
    } else {
        format!("{whole} {suffix}")
    }
}

/// A modification time as `YYYY-MM-DD HH:MM`.
///
/// **In UTC.** Local time needs a timezone database, which this project does not
/// depend on and should not grow one for a column; showing the host's own offset
/// would need the server to report it. So this is deliberately the one unambiguous
/// reading rather than a guess at the viewer's.
#[must_use]
pub(crate) fn format_modified(time: SystemTime) -> String {
    // Floored, so a time before 1970 lands on the earlier day with a positive
    // remainder rather than truncating towards the epoch.
    let seconds = match time.duration_since(UNIX_EPOCH) {
        Ok(since) => i64::try_from(since.as_secs()).unwrap_or(i64::MAX),
        Err(before) => -i64::try_from(before.duration().as_secs()).unwrap_or(i64::MAX),
    };
    let days = seconds.div_euclid(86_400);
    let within_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute) = (within_day / 3600, within_day % 3600 / 60);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}")
}

/// Splits a count of days since 1970-01-01 into a civil year, month and day.
///
/// Hinnant's algorithm. It shifts the year to start in March so the leap day falls at
/// the end of it, which is what removes February's special case and leaves the whole
/// thing as division.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    // Re-based on 0000-03-01, which is 719_468 days before the epoch.
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = u32::try_from(day_of_year - (153 * shifted_month + 2) / 5 + 1).unwrap_or(1);
    let month = u32::try_from(if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    })
    .unwrap_or(1);
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{UNIX_EPOCH, civil_from_days, format_modified, format_size};

    #[test]
    fn sizes_below_a_kilobyte_are_exact() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(7), "7 B");
        assert_eq!(format_size(1023), "1023 B");
    }

    #[test]
    fn sizes_carry_one_decimal_only_where_it_says_something() {
        assert_eq!(format_size(1024), "1.0 K");
        assert_eq!(format_size(1536), "1.5 K");
        assert_eq!(format_size(10 * 1024), "10 K");
        assert_eq!(format_size(1024 * 1024), "1.0 M");
        assert_eq!(format_size(15 * 1024 * 1024), "15 M");
    }

    /// The largest unit is the last one, so a huge file grows its number rather than
    /// running off the end of the table.
    #[test]
    fn sizes_stop_at_the_largest_unit() {
        assert_eq!(format_size(u64::MAX), "16777215 T");
    }

    #[test]
    fn known_days_convert_to_known_dates() {
        for (days, expected) in [
            (0, (1970, 1, 1)),
            (1, (1970, 1, 2)),
            (59, (1970, 3, 1)),
            (365, (1971, 1, 1)),
            // 2000 was a leap year and 1900 was not, which is where a naive rule
            // goes wrong.
            (11_016, (2000, 2, 29)),
            (20_679, (2026, 8, 14)),
            (-1, (1969, 12, 31)),
            (-719_468, (0, 3, 1)),
        ] {
            assert_eq!(
                civil_from_days(days),
                expected,
                "{days} days from the epoch"
            );
        }
    }

    #[test]
    fn the_epoch_formats_as_itself() {
        assert_eq!(format_modified(UNIX_EPOCH), "1970-01-01 00:00");
    }

    #[test]
    fn times_format_to_the_minute() {
        let time = UNIX_EPOCH + Duration::from_secs(1_786_000_000);
        assert_eq!(format_modified(time), "2026-08-06 07:06");
    }

    /// A time before 1970 lands on the earlier day rather than truncating towards the
    /// epoch, which is what floored division buys.
    #[test]
    fn times_before_the_epoch_land_on_the_earlier_day() {
        let time = UNIX_EPOCH - Duration::from_secs(1);
        assert_eq!(format_modified(time), "1969-12-31 23:59");
    }
}
