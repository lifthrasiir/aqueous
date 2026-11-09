//! Times as other formats keep them, both ways: Windows' FILETIME, and MS-DOS's local date and
//! time (taken as local time here: a local time of an unknown time zone as it is).

use aqr_formula::aqr::Time;

/// Seconds from 1601-01-01 to 1970-01-01.
const FILETIME_EPOCH: i64 = 11_644_473_600;

/// A FILETIME (100 ns since 1601-01-01 UTC); `None` where it is 0 (none).
pub fn from_filetime(t: u64) -> Option<Time> {
    (t != 0).then(|| Time::new((t / 10_000_000) as i64 - FILETIME_EPOCH, (t % 10_000_000) as u32 * 100))
}

/// `t` as a FILETIME (100 ns since 1601-01-01 UTC); 0 (none) before 1601.
pub fn to_filetime(t: Time) -> u64 {
    let s = t.secs.saturating_add(FILETIME_EPOCH);
    if s < 0 { 0 } else { (s as u64).saturating_mul(10_000_000).saturating_add(t.nanos as u64 / 100) }
}

/// The parts of a civil time: year, month, day, hour, minute, second.
type Civil = (i32, u32, u32, u32, u32, u32);

/// An MS-DOS date and time (2-second steps), as local time here: `None` where it is not a
/// date (a month or day of 0).
pub fn from_dos(date: u16, time: u16) -> Option<Time> {
    let (y, mo, d) = (1980 + (date >> 9) as i32, (date >> 5 & 15) as u32, (date & 31) as u32);
    let (h, mi, s) = ((time >> 11) as u32, (time >> 5 & 63) as u32, (time & 31) as u32 * 2);
    if !(1..=12).contains(&mo) || d == 0 || h > 23 || mi > 59 || s > 59 {
        return None;
    }
    from_local((y, mo, d, h, mi, s))
}

/// `t` as a point in time: itself where it is one, else a local time of an unknown time zone taken
/// as local time here (as [`from_dos`] and [`to_dos`] take MS-DOS's), its fraction kept. `None`
/// where the local time cannot be one here.
pub fn instant(t: Time) -> Option<Time> {
    if t.is_instant() {
        return Some(t);
    }
    let (y, mo, d, h, mi, s) = t.civil();
    let at = from_local((i32::try_from(y).ok()?, mo, d, h, mi, s))?;
    Some(Time { nanos: t.nanos, ..at })
}

/// `t` as an MS-DOS date and time (2-second steps, rounded down) in local time here (a local
/// time of an unknown time zone as it is), kept within what they can have (1980 to 2107).
pub fn to_dos(t: Time) -> (u16, u16) {
    let civil = || {
        let (y, mo, d, h, mi, s) = t.civil();
        Some((y.clamp(0, 9999) as i32, mo, d, h, mi, s))
    };
    let parts = if t.is_instant() { to_local(t.secs) } else { civil() };
    let Some((y, mo, d, h, mi, s)) = parts else {
        return (0x21, 0);
    };
    if y < 1980 {
        return (0x21, 0);
    }
    if y > 2107 {
        return (127 << 9 | 12 << 5 | 31, 23 << 11 | 59 << 5 | 29);
    }
    (((y - 1980) as u16) << 9 | (mo as u16) << 5 | d as u16, (h as u16) << 11 | (mi as u16) << 5 | (s / 2) as u16)
}

#[cfg(windows)]
fn from_local((y, mo, d, h, mi, s): Civil) -> Option<Time> {
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
    from_filetime((ft.dwHighDateTime as u64) << 32 | ft.dwLowDateTime as u64)
}

#[cfg(windows)]
fn to_local(secs: i64) -> Option<Civil> {
    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
    let ft = to_filetime(Time::new(secs, 0));
    if ft == 0 {
        return None;
    }
    let ft = FILETIME { dwLowDateTime: ft as u32, dwHighDateTime: (ft >> 32) as u32 };
    // SAFETY: plain data, written by the calls
    let (mut utc, mut st): (SYSTEMTIME, SYSTEMTIME) = unsafe { std::mem::zeroed() };
    // SAFETY: pointers to plain structures of their types
    unsafe {
        if FileTimeToSystemTime(&ft, &mut utc) == 0
            || SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut st) == 0
        {
            return None;
        }
    }
    let f = |v: u16| v as u32;
    Some((st.wYear as i32, f(st.wMonth), f(st.wDay), f(st.wHour), f(st.wMinute), f(st.wSecond)))
}

#[cfg(unix)]
fn from_local((y, mo, d, h, mi, s): Civil) -> Option<Time> {
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
    (t != -1).then(|| Time::new(t as i64, 0))
}

#[cfg(unix)]
fn to_local(secs: i64) -> Option<Civil> {
    let t = secs as libc::time_t;
    // SAFETY: plain data, written by localtime_r
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: a time and room for its parts
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return None;
    }
    let f = |v: i32| v as u32;
    Some((tm.tm_year + 1900, f(tm.tm_mon + 1), f(tm.tm_mday), f(tm.tm_hour), f(tm.tm_min), f(tm.tm_sec).min(59)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filetimes() {
        assert_eq!(to_filetime(Time::new(0, 1500)), 116_444_736_000_000_015);
        assert_eq!(from_filetime(116_444_736_000_000_015), Some(Time::new(0, 1500)));
        assert_eq!(to_filetime(Time::new(-FILETIME_EPOCH - 1, 0)), 0);
        assert_eq!(from_filetime(0), None);
    }

    #[test]
    fn dos_times() {
        // 2020-02-29 12:34:56, within a day of its UTC time wherever this runs
        let t = from_dos((40 << 9) | (2 << 5) | 29, (12 << 11) | (34 << 5) | 28).unwrap();
        assert!((t.secs - 1_582_979_696).abs() <= 14 * 3600, "{t:?}");
        assert_eq!(from_dos(0, 0), None);
        let (date, time) = to_dos(Time::new(1_582_979_696, 0));
        assert_eq!(date >> 9, 40);
        assert_eq!(time & 31, 28);
        assert_eq!(to_dos(Time::new(0, 0)).0 >> 9, 0, "1970 as 1980");
        // both ways, in local time here
        assert_eq!(to_dos(t), ((40 << 9) | (2 << 5) | 29, (12 << 11) | (34 << 5) | 28));
    }
}
