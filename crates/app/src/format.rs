//! Text for durations, dates and counts.

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// `3:07`, or `1:02:07` from an hour up.
pub fn duration(ms: u32) -> String {
    let seconds = ms / 1000;
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// `41 min left` or `1 hr 5 min left`; a part-started minute counts whole.
pub fn time_left(ms: u32) -> String {
    let minutes = ms.div_ceil(60_000).max(1);
    match (minutes / 60, minutes % 60) {
        (0, minutes) => format!("{minutes} min left"),
        (hours, 0) => format!("{hours} hr left"),
        (hours, minutes) => format!("{hours} hr {minutes} min left"),
    }
}

/// `Oct 3, 2026` for a Unix timestamp in seconds; empty when unknown.
pub fn date(unix_seconds: i64) -> String {
    if unix_seconds <= 0 {
        return String::new();
    }
    // Civil-from-days, after Howard Hinnant's `days_from_civil` inverse.
    let days = unix_seconds.div_euclid(86_400) + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{} {day}, {year}", MONTHS[month as usize - 1])
}

/// `1 song`, `187 songs`.
pub fn count(amount: usize, noun: &str) -> String {
    format!("{amount} {noun}{}", if amount == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_durations() {
        assert_eq!(duration(0), "0:00");
        assert_eq!(duration(187_999), "3:07");
        assert_eq!(duration(3_727_000), "1:02:07");
    }

    #[test]
    fn formats_time_left() {
        assert_eq!(time_left(1), "1 min left");
        assert_eq!(time_left(41 * 60_000 - 500), "41 min left");
        assert_eq!(time_left(60 * 60_000), "1 hr left");
        assert_eq!(time_left(80 * 60_000 + 1), "1 hr 21 min left");
    }

    #[test]
    fn formats_dates_across_leap_years_and_month_ends() {
        assert_eq!(date(0), "");
        assert_eq!(date(86_400), "Jan 2, 1970");
        assert_eq!(date(951_782_400), "Feb 29, 2000");
        assert_eq!(date(1_709_251_199), "Feb 29, 2024");
        assert_eq!(date(1_709_251_200), "Mar 1, 2024");
        assert_eq!(date(1_785_772_760), "Aug 3, 2026");
        assert_eq!(date(1_798_761_599), "Dec 31, 2026");
    }

    #[test]
    fn pluralises_counts() {
        assert_eq!(count(1, "song"), "1 song");
        assert_eq!(count(0, "song"), "0 songs");
        assert_eq!(count(187, "song"), "187 songs");
    }
}
