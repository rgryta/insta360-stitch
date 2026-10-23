//! The camera clock. A video's container dates are UTC, while the trailer keeps the camera's
//! own wall clock; the difference is the time zone the camera was set to.

/// Seconds since the Unix epoch for `YYYYMMDDhhmmss` digits read as UTC.
pub fn wall_clock_seconds(digits: &str) -> Option<i64> {
    if digits.len() != 14 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let field = |range: std::ops::Range<usize>| digits[range].parse::<i64>().ok();
    let (year, month, day) = (field(0..4)?, field(4..6)?, field(6..8)?);
    let (hour, minute, second) = (field(8..10)?, field(10..12)?, field(12..14)?);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    // days from civil (Howard Hinnant's algorithm)
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hour * 3600 + minute * 60 + second)
}

/// The camera's UTC offset in minutes, from its wall clock (`YYYYMMDDhhmmss`) and the same
/// moment in UTC. `None` when the difference isn't within 2 minutes of a real quarter-hour
/// offset (-12:00 to +14:00): a drifted clock, not a zone.
pub fn camera_utc_offset_minutes(local_time: &str, utc_unix_seconds: i64) -> Option<i32> {
    let exact = (wall_clock_seconds(local_time)? - utc_unix_seconds) as f64 / 60.0;
    let minutes = (exact / 15.0).round() * 15.0;
    if (exact - minutes).abs() > 2.0 || !(-12.0 * 60.0..=14.0 * 60.0).contains(&minutes) {
        return None;
    }
    Some(minutes as i32)
}
