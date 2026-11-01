//! Times and names as archives of other formats keep them, made the tree's: Windows' FILETIME,
//! MS-DOS's local date and time, and names in the code page of the system that archived them.

use aqr_formula::aqr::Time;

/// Seconds from 1601-01-01 to 1970-01-01.
const FILETIME_EPOCH: i64 = 11_644_473_600;

/// A FILETIME (100 ns since 1601-01-01 UTC); `None` where it is 0 (none).
pub fn filetime(t: u64) -> Option<Time> {
    (t != 0).then(|| Time { secs: (t / 10_000_000) as i64 - FILETIME_EPOCH, nanos: (t % 10_000_000) as u32 * 100 })
}

/// Seconds since 1970 (UTC).
pub fn unix(secs: i64) -> Time {
    Time { secs, nanos: 0 }
}

/// An MS-DOS date and time (2-second steps), as local time here: `None` where it is not a
/// date (a month or day of 0).
pub fn dos(date: u16, time: u16) -> Option<Time> {
    let (y, mo, d) = (1980 + (date >> 9) as i32, (date >> 5 & 15) as u32, (date & 31) as u32);
    let (h, mi, s) = ((time >> 11) as u32, (time >> 5 & 63) as u32, (time & 31) as u32 * 2);
    if !(1..=12).contains(&mo) || d == 0 || h > 23 || mi > 59 || s > 59 {
        return None;
    }
    local(y, mo, d, h, mi, s)
}

#[cfg(windows)]
fn local(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> Option<Time> {
    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::System::Time::{SystemTimeToFileTime, TzSpecificLocalTimeToSystemTime};
    let st = SYSTEMTIME {
        wYear: y as u16,
        wMonth: mo as u16,
        wDayOfWeek: 0,
        wDay: d as u16,
        wHour: h as u16,
        wMinute: mi as u16,
        wSecond: s as u16,
        wMilliseconds: 0,
    };
    let mut utc = st;
    let mut ft = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    // SAFETY: pointers to plain structures of their types
    unsafe {
        if TzSpecificLocalTimeToSystemTime(std::ptr::null(), &st, &mut utc) == 0
            || SystemTimeToFileTime(&utc, &mut ft) == 0
        {
            return None;
        }
    }
    filetime((ft.dwHighDateTime as u64) << 32 | ft.dwLowDateTime as u64)
}

#[cfg(unix)]
fn local(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> Option<Time> {
    // SAFETY: plain data, zero where not set
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = y - 1900;
    tm.tm_mon = mo as i32 - 1;
    tm.tm_mday = d as i32;
    tm.tm_hour = h as i32;
    tm.tm_min = mi as i32;
    tm.tm_sec = s as i32;
    tm.tm_isdst = -1;
    // SAFETY: a time of its type
    let t = unsafe { libc::mktime(&mut tm) };
    (t != -1).then(|| unix(t as i64))
}

/// Name `b` as UTF-8, of the code page of the system's console (`CP_OEMCP`, as archivers of
/// MS-DOS and Windows write names): `None` where it is not of it, or the system has none.
#[cfg(windows)]
pub fn oem(b: &[u8]) -> Option<Vec<u8>> {
    use windows_sys::Win32::Globalization::{CP_OEMCP, MB_ERR_INVALID_CHARS, MultiByteToWideChar};
    if b.is_empty() || b.len() > i32::MAX as usize {
        return None;
    }
    let mut w = vec![0u16; b.len()];
    // SAFETY: a buffer of as many units as there are bytes (no character takes more)
    let n = unsafe {
        MultiByteToWideChar(CP_OEMCP, MB_ERR_INVALID_CHARS, b.as_ptr(), b.len() as i32, w.as_mut_ptr(), w.len() as i32)
    };
    (n > 0).then(|| String::from_utf16_lossy(&w[..n as usize]).into_bytes())
}

#[cfg(not(windows))]
pub fn oem(_: &[u8]) -> Option<Vec<u8>> {
    None
}

/// Name `b` as the archive gives it, not said to be UTF-8: as it is where it is UTF-8 (as
/// archivers of Unix write it) or ASCII, else of the system's code page where it has one (on
/// Windows), else as it is.
pub fn legacy_name(b: &[u8]) -> Vec<u8> {
    if std::str::from_utf8(b).is_ok() {
        return b.to_vec();
    }
    oem(b).unwrap_or_else(|| b.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times() {
        assert_eq!(filetime(116_444_736_000_000_000 + 15), Some(Time { secs: 0, nanos: 1500 }));
        assert_eq!(filetime(0), None);
        // 2020-02-29 12:34:56, within a day of its UTC time wherever this runs
        let t = dos((40 << 9) | (2 << 5) | 29, (12 << 11) | (34 << 5) | 28).unwrap();
        assert!((t.secs - 1_582_979_696).abs() <= 14 * 3600, "{t:?}");
        assert_eq!(dos(0, 0), None);
    }
}
