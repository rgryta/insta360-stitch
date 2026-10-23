//! This module uses the camera clock.
//! The container dates of a video are in UTC. The trailer contains the time on the camera clock.
//! The difference between the two is the time zone of the camera.

/// Gives the seconds since the Unix epoch for `YYYYMMDDhhmmss` digits in UTC.
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
    // Days from a civil date (the algorithm of Howard Hinnant).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hour * 3600 + minute * 60 + second)
}

/// Gives the UTC offset of the camera in minutes.
/// It uses the camera clock (`YYYYMMDDhhmmss`) and the same time in UTC.
/// It gives `None` if the difference is more than 2 minutes from a real quarter-hour offset (-12:00 to +14:00).
/// In that condition, the camera clock is wrong, and the difference is not a time zone.
pub fn camera_utc_offset_minutes(local_time: &str, utc_unix_seconds: i64) -> Option<i32> {
    let exact = (wall_clock_seconds(local_time)? - utc_unix_seconds) as f64 / 60.0;
    let minutes = (exact / 15.0).round() * 15.0;
    if (exact - minutes).abs() > 2.0 || !(-12.0 * 60.0..=14.0 * 60.0).contains(&minutes) {
        return None;
    }
    Some(minutes as i32)
}
