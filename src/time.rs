//! UTC dates without a date library. The app shows every time in UTC.

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since 1970-01-01 UTC (negative before it).
pub fn unix_seconds(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

/// `YYYY-MM-DD HH:MM:SS`, the same form the database uses.
pub fn utc_datetime(time: SystemTime) -> String {
    let secs = unix_seconds(time);
    let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
    let rest = secs.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// Days since 1970-01-01 to (year, month, day). From Howard Hinnant's date algorithms.
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(11_017), (2000, 3, 1));
        assert_eq!(civil_from_days(20_413), (2025, 11, 21));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }

    #[test]
    fn datetimes() {
        assert_eq!(utc_datetime(UNIX_EPOCH), "1970-01-01 00:00:00");
        let time = UNIX_EPOCH + Duration::from_secs(20_413 * 86_400 + 18 * 3600 + 5 * 60 + 9);
        assert_eq!(utc_datetime(time), "2025-11-21 18:05:09");
        assert_eq!(
            utc_datetime(UNIX_EPOCH - Duration::from_secs(1)),
            "1969-12-31 23:59:59"
        );
    }
}
