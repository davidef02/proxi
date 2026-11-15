//! ISO and it-IT local time without a date crate: `Date.toISOString` and `toLocaleString("it-IT")`.

use std::time::{SystemTime, UNIX_EPOCH};

fn unix_ms(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_millis() as i64,
        Err(e) => -(e.duration().as_millis() as i64),
    }
}

/// Howard Hinnant's days-from-civil inverse: (year, month 1-12, day, hour, min, sec) in UTC.
fn civil_from_unix(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if month <= 2 { y + 1 } else { y };
    (
        year,
        month,
        day,
        (secs_of_day / 3_600) as u32,
        ((secs_of_day % 3_600) / 60) as u32,
        (secs_of_day % 60) as u32,
    )
}

/// `Date.toISOString`, milliseconds truncated (same text for years 0..=9999).
pub fn iso_string(t: SystemTime) -> String {
    let ms = unix_ms(t);
    let secs = ms.div_euclid(1_000);
    let millis = ms.rem_euclid(1_000) as u32;
    let (year, month, day, hour, min, sec) = civil_from_unix(secs);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}.{millis:03}Z")
}

/// Local time as `toLocaleString("it-IT")` prints it: `dd/mm/yyyy, HH:MM:SS`.
pub fn it_it_local(t: SystemTime) -> String {
    let ms = unix_ms(t);
    let secs = ms.div_euclid(1_000);
    let (year, month, day, hour, min, sec) = local_civil(secs, ms.rem_euclid(1_000) as u32);
    format!("{day:02}/{month:02}/{year:04}, {hour:02}:{min:02}:{sec:02}")
}

#[cfg(unix)]
fn local_civil(secs: i64, _millis: u32) -> (i64, u32, u32, u32, u32, u32) {
    // SAFETY: `tm` is a valid zeroed struct and the pointer outlives the call; localtime_r fills it.
    unsafe {
        let time = secs as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&time, &mut tm).is_null() {
            return civil_from_unix(secs);
        }
        (
            tm.tm_year as i64 + 1900,
            (tm.tm_mon + 1) as u32,
            tm.tm_mday as u32,
            tm.tm_hour as u32,
            tm.tm_min as u32,
            tm.tm_sec as u32,
        )
    }
}

#[cfg(windows)]
fn local_civil(secs: i64, millis: u32) -> (i64, u32, u32, u32, u32, u32) {
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    use windows_sys::Win32::System::Time::SystemTimeToTzSpecificLocalTime;

    let (year, month, day, hour, min, sec) = civil_from_unix(secs);
    let utc = SYSTEMTIME {
        wYear: year as u16,
        wMonth: month as u16,
        wDayOfWeek: 0,
        wDay: day as u16,
        wHour: hour as u16,
        wMinute: min as u16,
        wSecond: sec as u16,
        wMilliseconds: millis as u16,
    };
    let mut local = SYSTEMTIME::default();
    // SAFETY: null asks for the current time zone; both SYSTEMTIME pointers are valid for the call.
    let ok = unsafe { SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) };
    if ok == 0 {
        (year, month, day, hour, min, sec)
    } else {
        (
            local.wYear as i64,
            local.wMonth as u32,
            local.wDay as u32,
            local.wHour as u32,
            local.wMinute as u32,
            local.wSecond as u32,
        )
    }
}

#[cfg(not(any(unix, windows)))]
fn local_civil(secs: i64, _millis: u32) -> (i64, u32, u32, u32, u32, u32) {
    civil_from_unix(secs)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn at(ms: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(ms)
    }

    #[test]
    fn iso_string_epoch() {
        assert_eq!(iso_string(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn iso_string_keeps_milliseconds() {
        assert_eq!(iso_string(at(1_700_000_000_123)), "2023-11-14T22:13:20.123Z");
    }

    #[test]
    fn iso_string_leap_day() {
        assert_eq!(iso_string(at(1_709_164_800_000)), "2024-02-29T00:00:00.000Z");
    }

    #[test]
    fn it_it_local_has_the_stamp_shape() {
        let text = it_it_local(SystemTime::now());
        assert_eq!(text.len(), 20, "{text}");
        let bytes = text.as_bytes();
        assert_eq!(bytes[2], b'/');
        assert_eq!(bytes[5], b'/');
        assert_eq!(&bytes[10..12], b", ");
        assert_eq!(bytes[14], b':');
        assert_eq!(bytes[17], b':');
        for (i, b) in bytes.iter().enumerate() {
            if matches!(i, 2 | 5 | 10 | 11 | 14 | 17) {
                continue;
            }
            assert!(b.is_ascii_digit(), "{text}");
        }
    }

    #[test]
    fn it_it_local_differs_one_second_apart() {
        assert_ne!(it_it_local(at(1_700_000_000_000)), it_it_local(at(1_700_000_001_000)));
    }
}
