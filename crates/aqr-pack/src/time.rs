//! Times as other formats keep them: MS-DOS's local date and time, Windows' FILETIME.

use aqr_formula::aqr::Time;

/// Seconds from 1601-01-01 to 1970-01-01.
const FILETIME_EPOCH: i64 = 11_644_473_600;

/// `t` as a FILETIME (100 ns since 1601-01-01 UTC); 0 (none) before 1601.
pub fn filetime(t: Time) -> u64 {
    let s = t.secs.saturating_add(FILETIME_EPOCH);
    if s < 0 { 0 } else { (s as u64).saturating_mul(10_000_000).saturating_add(t.nanos as u64 / 100) }
}

/// `t` as an MS-DOS date and time (2-second steps, rounded down) in local time here (a local
/// time of an unknown time zone as it is), kept
/// within what they can have (1980 to 2107).
pub fn dos(t: Time) -> (u16, u16) {
    // (a local time of an unknown time zone as it is)
    let civil = || {
        let (y, mo, d, h, mi, s) = t.civil();
        Some((y.clamp(0, 9999) as i32, mo, d, h, mi, s))
    };
    let parts = if t.is_instant() { local(t.secs) } else { civil() };
    let Some((y, mo, d, h, mi, s)) = parts else { return (0x21, 0) };
    if y < 1980 {
        return (0x21, 0);
    }
    if y > 2107 {
        return (127 << 9 | 12 << 5 | 31, 23 << 11 | 59 << 5 | 29);
    }
    (((y - 1980) as u16) << 9 | (mo as u16) << 5 | d as u16, (h as u16) << 11 | (mi as u16) << 5 | (s / 2) as u16)
}

#[cfg(windows)]
fn local(secs: i64) -> Option<(i32, u32, u32, u32, u32, u32)> {
    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
    let ft = filetime(Time::new(secs, 0));
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
fn local(secs: i64) -> Option<(i32, u32, u32, u32, u32, u32)> {
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
    fn times() {
        assert_eq!(filetime(Time::new(0, 1500)), 116_444_736_000_000_015);
        assert_eq!(filetime(Time::new(-FILETIME_EPOCH - 1, 0)), 0);
        // 2020-02-29 12:34:56 UTC, within a day of it wherever this runs
        let (date, time) = dos(Time::new(1_582_979_696, 0));
        assert_eq!(date >> 9, 40);
        assert_eq!(time & 31, 28);
        assert_eq!(dos(Time::new(0, 0)).0 >> 9, 0, "1970 as 1980");
    }
}
