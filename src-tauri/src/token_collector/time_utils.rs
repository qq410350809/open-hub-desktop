use std::time::UNIX_EPOCH;

pub fn update_bounds(first: &mut String, last: &mut String, timestamp: &str) {
    if timestamp.is_empty() {
        return;
    }
    if first.is_empty() || timestamp < first.as_str() {
        *first = timestamp.to_string();
    }
    if last.is_empty() || timestamp > last.as_str() {
        *last = timestamp.to_string();
    }
}

pub fn half_hour_key(timestamp: &str) -> Option<String> {
    let value = timestamp.trim();
    if value.len() < 16 {
        return None;
    }
    let prefix = value.get(..13)?;
    if prefix.as_bytes().get(4) != Some(&b'-')
        || prefix.as_bytes().get(7) != Some(&b'-')
        || prefix.as_bytes().get(10) != Some(&b'T')
    {
        return None;
    }
    let minute = value.get(14..16)?.parse::<u32>().ok()?;
    let offset_secs = tz_offset_secs(value);
    if offset_secs == 0 {
        return Some(format!(
            "{prefix}:{:02}:00.000Z",
            if minute < 30 { 0 } else { 30 }
        ));
    }
    let year: i64 = value.get(0..4)?.parse().ok()?;
    let month: i64 = value.get(5..7)?.parse().ok()?;
    let day: i64 = value.get(8..10)?.parse().ok()?;
    let hour: i64 = value.get(11..13)?.parse().ok()?;
    let days = days_from_civil(year, month, day);
    let utc_secs = days * 86_400 + hour * 3_600 + i64::from(minute) * 60 - offset_secs;
    let tod = utc_secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(utc_secs.div_euclid(86_400));
    Some(format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:00.000Z",
        tod / 3_600,
        if (tod % 3_600) / 60 < 30 { 0 } else { 30 }
    ))
}

pub fn tz_offset_secs(ts: &str) -> i64 {
    let Some(t_index) = ts.find('T') else {
        return 0;
    };
    let Some(zone_start) = ts[t_index..]
        .find(['Z', 'z', '+', '-'])
        .map(|i| t_index + i)
    else {
        return 0;
    };
    match ts.as_bytes()[zone_start] {
        b'Z' | b'z' => 0,
        sign => {
            let positive = sign != b'-';
            let digits: String = ts[zone_start + 1..]
                .chars()
                .filter(|ch| ch.is_ascii_digit())
                .collect();
            let (hours, minutes) = match digits.len() {
                4 => (
                    digits[0..2].parse::<i64>().unwrap_or(0),
                    digits[2..4].parse::<i64>().unwrap_or(0),
                ),
                2 => (digits[0..2].parse::<i64>().unwrap_or(0), 0),
                1 => (digits[0..1].parse::<i64>().unwrap_or(0), 0),
                _ => (0, 0),
            };
            let magnitude = hours * 3_600 + minutes * 60;
            if positive {
                magnitude
            } else {
                -magnitude
            }
        }
    }
}

pub fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let month_shifted = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * month_shifted + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}

pub fn iso_from_millis(ms: i64) -> String {
    if ms <= 0 {
        return String::new();
    }
    let seconds = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let time = seconds.rem_euclid(86_400);
    let hour = time / 3600;
    let minute = (time % 3600) / 60;
    let second = time % 60;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

pub fn now_iso() -> String {
    let millis = UNIX_EPOCH
        .elapsed()
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    iso_from_millis(millis)
}
