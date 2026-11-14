//! Times printed as text by a pattern, stored as one integer: `README.md` ("Times") specifies
//! it. The decoder's side is [`valid`] and [`put`]; [`parse`] is the encoder's.

/// The shape of a valid pattern: what it holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Shape {
    year: bool,
    date: bool,
    hour: bool,
    minute: bool,
    second: bool,
    /// The digits of the fraction (0: none).
    frac: u32,
}

pub const MONTHS: [&[u8; 3]; 12] =
    [b"Jan", b"Feb", b"Mar", b"Apr", b"May", b"Jun", b"Jul", b"Aug", b"Sep", b"Oct", b"Nov", b"Dec"];
pub const WEEKDAYS: [&[u8; 3]; 7] = [b"Mon", b"Tue", b"Wed", b"Thu", b"Fri", b"Sat", b"Sun"];

/// The year of dates without one (a leap year, so that 29 February is there).
pub const NO_YEAR: i64 = 2000;

/// The longest pattern, in bytes: what it prints is at most 190 bytes.
pub const MAX_PATTERN: usize = 64;

/// One item of a pattern.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Item {
    Lit(u8),
    Year,
    Month2,
    MonthName,
    Day2,
    DayPad,
    Day,
    Weekday,
    Hour2,
    Hour,
    Minute,
    Second,
    /// A fraction: its point, digits, and whether trailing zeros are dropped.
    Frac(u8, u8, bool),
}

/// The item at the start of `p` and its length in bytes; None for what is none.
fn item(p: &[u8]) -> Option<(Item, usize)> {
    let &c = p.first()?;
    Some(match c {
        b'Y' => (Item::Year, 1),
        b'm' => (Item::Month2, 1),
        b'b' => (Item::MonthName, 1),
        b'd' => (Item::Day2, 1),
        b'e' => (Item::DayPad, 1),
        b'D' => (Item::Day, 1),
        b'a' => (Item::Weekday, 1),
        b'H' => (Item::Hour2, 1),
        b'h' => (Item::Hour, 1),
        b'M' => (Item::Minute, 1),
        b'S' => (Item::Second, 1),
        b'f' | b't' => match p {
            &[_, point @ (b'.' | b','), n @ b'1'..=b'9', ..] => (Item::Frac(point, n - b'0', c == b't'), 3),
            _ => return None,
        },
        b'\\' => (Item::Lit(*p.get(1).filter(|&&b| !matches!(b, 1..=4 | b'\n'))?), 2),
        1..=4 | b'\n' => return None,
        c if c.is_ascii_alphabetic() => return None,
        c => (Item::Lit(c), 1),
    })
}

/// The shape of `pat` if it is valid (README, "Times").
fn shape(pat: &[u8]) -> Option<Shape> {
    if pat.len() > MAX_PATTERN {
        return None;
    }
    let mut sh = Shape::default();
    let (mut month, mut day, mut wd, mut frac) = (false, false, false, false);
    let mut at = 0;
    // (an item once: a flag set twice is an error)
    let once = |flag: &mut bool| !std::mem::replace(flag, true);
    while at < pat.len() {
        let (it, n) = item(&pat[at..])?;
        at += n;
        let ok = match it {
            Item::Lit(_) => true,
            Item::Year => once(&mut sh.year),
            Item::Month2 | Item::MonthName => once(&mut month),
            Item::Day2 | Item::DayPad | Item::Day => once(&mut day),
            Item::Weekday => once(&mut wd),
            Item::Hour2 | Item::Hour => once(&mut sh.hour),
            Item::Minute => once(&mut sh.minute),
            Item::Second => once(&mut sh.second),
            Item::Frac(_, n, _) => {
                sh.frac = n as u32;
                once(&mut frac)
            }
        };
        if !ok {
            return None;
        }
    }
    sh.date = month || day || wd || sh.year;
    let date_ok = !sh.date || (month && day);
    let time_ok = (!sh.minute || sh.hour) && (!sh.second || sh.minute) && (!frac || sh.second);
    (date_ok && time_ok && (sh.date || sh.hour)).then_some(sh)
}

/// The digits of the fraction of a valid pattern (0 where it has none, or is invalid).
pub fn fraction_digits(pat: &[u8]) -> u32 {
    shape(pat).map_or(0, |sh| sh.frac)
}

/// Whether `pat` is a pattern (README, "Times"): then [`put`] can print any value or say that
/// it cannot.
pub fn valid(pat: &[u8]) -> bool {
    shape(pat).is_some()
}

/// Days from 1970-01-01 of the proleptic Gregorian date (month 1 to 12, day 1 to 31).
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = y - (m <= 2) as i64;
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// The year, month and day of `z` days from 1970-01-01.
pub fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + (m <= 2) as i64, m, d)
}

/// The days of the first and last of the dates of years 0 and 9999 (and of [`NO_YEAR`]).
const MIN_DAYS: i64 = -719528;
const MAX_DAYS: i64 = 2932896;

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        2 if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// The weekday of `days` (0: Monday; 1970-01-01 was a Thursday).
fn weekday(days: i64) -> usize {
    (days + 3).rem_euclid(7) as usize
}

fn two(out: &mut Vec<u8>, x: i64) {
    out.extend_from_slice(&[b'0' + (x / 10) as u8, b'0' + (x % 10) as u8]);
}

/// Appends the text of `pat` (valid) for the integer `v`; false, with `out` as it was, if `v` is
/// not of a time the pattern can print: more than a day for a pattern of times of day, a year
/// outside 0 to 9999 (or not `NO_YEAR` where the pattern has none), or a pattern that is not
/// valid.
pub fn put(out: &mut Vec<u8>, pat: &[u8], v: i64) -> bool {
    let Some(sh) = shape(pat) else { return false };
    let len = out.len();
    if !print(out, pat, v, sh) {
        out.truncate(len);
        return false;
    }
    true
}

fn print(out: &mut Vec<u8>, pat: &[u8], v: i64, sh: Shape) -> bool {
    let mut x = v as i128;
    let pow = 10i128.pow(sh.frac);
    let frac = x.rem_euclid(pow) as i64;
    x = x.div_euclid(pow);
    let (mut s, mut m, mut h) = (0, 0, 0);
    if sh.second {
        s = x.rem_euclid(60) as i64;
        x = x.div_euclid(60);
    }
    if sh.minute {
        m = x.rem_euclid(60) as i64;
        x = x.div_euclid(60);
    }
    if sh.hour {
        h = x.rem_euclid(24) as i64;
        x = x.div_euclid(24);
    }
    let (mut y, mut mo, mut d, mut wd) = (0, 0, 0, 0);
    if sh.date {
        let Ok(days) = i64::try_from(x) else { return false };
        if !(MIN_DAYS..=MAX_DAYS).contains(&days) {
            return false;
        }
        (y, mo, d) = civil_from_days(days);
        if sh.year {
            if !(0..=9999).contains(&y) {
                return false;
            }
        } else if y != NO_YEAR {
            return false;
        }
        wd = weekday(days);
    } else if x != 0 {
        return false;
    }
    let mut at = 0;
    while at < pat.len() {
        let Some((it, n)) = item(&pat[at..]) else { return false };
        at += n;
        match it {
            Item::Lit(c) => out.push(c),
            Item::Year => {
                two(out, y / 100);
                two(out, y % 100);
            }
            Item::Month2 => two(out, mo),
            Item::MonthName => out.extend_from_slice(MONTHS[mo as usize - 1]),
            Item::Day2 => two(out, d),
            Item::DayPad => {
                if d < 10 {
                    out.push(b' ');
                    out.push(b'0' + d as u8);
                } else {
                    two(out, d);
                }
            }
            Item::Day => {
                if d >= 10 {
                    out.push(b'0' + (d / 10) as u8);
                }
                out.push(b'0' + (d % 10) as u8);
            }
            Item::Weekday => out.extend_from_slice(WEEKDAYS[wd]),
            Item::Hour2 => two(out, h),
            Item::Hour => {
                if h >= 10 {
                    out.push(b'0' + (h / 10) as u8);
                }
                out.push(b'0' + (h % 10) as u8);
            }
            Item::Minute => two(out, m),
            Item::Second => two(out, s),
            Item::Frac(point, n, trim) => {
                let mut digits = [b'0'; 9];
                let mut f = frac;
                for i in (0..n as usize).rev() {
                    digits[i] = b'0' + (f % 10) as u8;
                    f /= 10;
                }
                let mut k = n as usize;
                if trim {
                    while k > 0 && digits[k - 1] == b'0' {
                        k -= 1;
                    }
                }
                if !trim || k > 0 {
                    out.push(point);
                    out.extend_from_slice(&digits[..k]);
                }
            }
        }
    }
    true
}

/// The integer of the time at the start of `t` printed as `pat` (valid), and its length in
/// bytes: None if `t` does not start so, or it is not a date, or its weekday is not the one of
/// its date, or the integer is more than 63 bits. What it takes may be printed otherwise by
/// [`put`] (a trimmed fraction with trailing zeros, a day padded by zeros): the encoder
/// checks.
pub fn parse(pat: &[u8], t: &[u8]) -> Option<(i64, usize)> {
    let sh = shape(pat)?;
    let (mut y, mut mo, mut d, mut wd) = (if sh.year { 0 } else { NO_YEAR }, 0, 0, None);
    let (mut h, mut mi, mut s, mut frac) = (0i64, 0i64, 0i64, 0i64);
    let (mut at, mut ti) = (0, 0);
    let digits = |t: &[u8], at: usize, n: usize| -> Option<i64> {
        let b = t.get(at..at + n)?;
        b.iter().all(u8::is_ascii_digit).then(|| b.iter().fold(0, |a, &c| a * 10 + (c - b'0') as i64))
    };
    // One or two digits, as many as there are.
    let unpadded = |t: &[u8], at: usize| -> Option<(i64, usize)> {
        let a = digits(t, at, 1)?;
        match digits(t, at + 1, 1) {
            Some(b) => Some((a * 10 + b, 2)),
            None => Some((a, 1)),
        }
    };
    while at < pat.len() {
        let (it, n) = item(&pat[at..])?;
        at += n;
        match it {
            Item::Lit(c) => {
                if t.get(ti) != Some(&c) {
                    return None;
                }
                ti += 1;
            }
            Item::Year => {
                y = digits(t, ti, 4)?;
                ti += 4;
            }
            Item::Month2 => {
                mo = digits(t, ti, 2)?;
                ti += 2;
            }
            Item::MonthName => {
                let b = t.get(ti..ti + 3)?;
                mo = MONTHS.iter().position(|m| m[..] == *b)? as i64 + 1;
                ti += 3;
            }
            Item::Day2 => {
                d = digits(t, ti, 2)?;
                ti += 2;
            }
            Item::DayPad => {
                if t.get(ti) == Some(&b' ') {
                    d = digits(t, ti + 1, 1)?;
                } else {
                    d = digits(t, ti, 2)?;
                }
                ti += 2;
            }
            Item::Day => {
                let (v, n) = unpadded(t, ti)?;
                (d, ti) = (v, ti + n);
            }
            Item::Weekday => {
                let b = t.get(ti..ti + 3)?;
                wd = Some(WEEKDAYS.iter().position(|m| m[..] == *b)?);
                ti += 3;
            }
            Item::Hour2 => {
                h = digits(t, ti, 2)?;
                ti += 2;
            }
            Item::Hour => {
                let (v, n) = unpadded(t, ti)?;
                (h, ti) = (v, ti + n);
            }
            Item::Minute => {
                mi = digits(t, ti, 2)?;
                ti += 2;
            }
            Item::Second => {
                s = digits(t, ti, 2)?;
                ti += 2;
            }
            Item::Frac(point, n, trim) => {
                let n = n as usize;
                if trim {
                    if t.get(ti) == Some(&point) {
                        let k = t[ti + 1..].iter().take_while(|b| b.is_ascii_digit()).count();
                        if !(1..=n).contains(&k) {
                            return None;
                        }
                        frac = digits(t, ti + 1, k)? * 10i64.pow((n - k) as u32);
                        ti += 1 + k;
                    }
                } else {
                    if t.get(ti) != Some(&point) {
                        return None;
                    }
                    frac = digits(t, ti + 1, n)?;
                    ti += 1 + n;
                }
            }
        }
    }
    if h > 23 || mi > 59 || s > 59 {
        return None;
    }
    let mut x = 0i128;
    if sh.date {
        if !(1..=12).contains(&mo) || !(1..=days_in_month(y, mo)).contains(&d) {
            return None;
        }
        let days = days_from_civil(y, mo, d);
        if wd.is_some_and(|w| w != weekday(days)) {
            return None;
        }
        x = days as i128;
    }
    if sh.hour {
        x = x * 24 + h as i128;
    }
    if sh.minute {
        x = x * 60 + mi as i128;
    }
    if sh.second {
        x = x * 60 + s as i128;
    }
    x = x * 10i128.pow(sh.frac) + frac as i128;
    Some((i64::try_from(x).ok()?, ti))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn print_of(pat: &str, v: i64) -> Option<String> {
        let mut o = b"x".to_vec();
        put(&mut o, pat.as_bytes(), v).then(|| String::from_utf8(o[1..].to_vec()).unwrap())
    }

    #[test]
    fn dates() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11017);
        assert_eq!(days_from_civil(0, 1, 1), MIN_DAYS);
        assert_eq!(days_from_civil(9999, 12, 31), MAX_DAYS);
        for z in (MIN_DAYS..=MAX_DAYS).step_by(7) {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
            assert!((1..=days_in_month(y, m)).contains(&d));
        }
        // 1970-01-01 a Thursday, 2005-06-09 too
        assert_eq!(WEEKDAYS[weekday(0)], b"Thu");
        assert_eq!(WEEKDAYS[weekday(days_from_civil(2005, 6, 9))], b"Thu");
    }

    #[test]
    fn patterns() {
        for (pat, text) in [
            ("Y-m-d H:M:S", "2004-02-28 00:59:16"),
            ("Y-m-d\\TH:M:S\\Z", "2023-10-12T08:30:00Z"),
            ("Y-m-d H:M:St.6", "2004-02-28 00:59:16.02785"),
            ("Y-m-d H:M:St.6", "2004-02-28 00:59:16"),
            ("Y-m-d H:M:Sf,3", "2024-01-01 10:00:00,123"),
            ("Y-m-d H:M", "2023-10-12 00:00"),
            ("H:M:St.6", "00:59:16.02785"),
            ("h:M:S", "9:05:03"),
            ("d/b/Y:H:M:S", "01/Jul/1995:00:00:01"),
            ("a b d H:M:S Y", "Thu Jun 09 06:07:04 2005"),
            ("a, d b Y H:M:S", "Thu, 09 Jun 2005 06:07:04"),
            ("b e H:M:S", "Dec 10 06:55:46"),
            ("b e H:M:S", "Dec  1 06:55:46"),
            ("b D H:M:S", "Dec 1 06:55:46"),
            ("b e H:M:S", "Feb 29 00:00:00"),
            ("d/m/Y", "16/12/2006"),
            ("d.m.Y H:M", "01.01.2019 01:15"),
            ("Ymd", "18690101"),
            ("Y/m/d", "0000/01/01"),
            ("H", "23"),
            ("Y-m-d H", "1999-12-31 23"),
        ] {
            let (v, n) = parse(pat.as_bytes(), text.as_bytes()).unwrap_or_else(|| panic!("{pat} {text}"));
            assert_eq!(n, text.len(), "{pat} {text}");
            assert_eq!(print_of(pat, v).as_deref(), Some(text), "{pat}");
        }
        // values
        assert_eq!(parse(b"Y-m-d", b"1970-01-02"), Some((1, 10)));
        assert_eq!(parse(b"Y-m-d H:M:S", b"1970-01-01 00:01:02"), Some((62, 19)));
        assert_eq!(parse(b"Y-m-d H:M", b"1970-01-01 01:02"), Some((62, 16)));
        assert_eq!(parse(b"H:M:S", b"01:00:01"), Some((3601, 8)));
        assert_eq!(parse(b"H:M:Sf.3", b"00:00:01.250"), Some((1250, 12)));
        assert_eq!(parse(b"b e H:M:S", b"Jan  1 00:00:00").unwrap().0, 10957 * 86400);
        assert_eq!(parse(b"b e H:M:S", b"Dec 31 00:00:00").unwrap().0, (10957 + 365) * 86400);
        // not times
        for (pat, text) in [
            ("Y-m-d", "2023-02-29"),
            ("Y-m-d", "2023-13-01"),
            ("Y-m-d", "2023-1-01"),
            ("Y-m-d H:M:S", "2023-01-01 24:00:00"),
            ("Y-m-d H:M:S", "2023-01-01 00:60:00"),
            ("Y-m-d H:M:S", "2023-01-01 00:00:60"),
            ("a b d H:M:S Y", "Fri Jun 09 06:07:04 2005"),
            ("b e H:M:S", "Foo 10 06:55:46"),
            ("Y-m-d H:M:Sf.3", "2023-01-01 00:00:00.12"),
            ("Y-m-d H:M:St.3", "2023-01-01 00:00:00.1234"),
            ("Y-m-d", "2023-01-0"),
        ] {
            assert_eq!(parse(pat.as_bytes(), text.as_bytes()), None, "{pat} {text}");
        }
        // what is printed otherwise than it was: the caller compares
        let (v, _) = parse(b"H:M:St.3", b"00:00:01.50").unwrap();
        assert_eq!(print_of("H:M:St.3", v).unwrap(), "00:00:01.5");
        let (v, _) = parse(b"Y-m-d H:M:S", b"2023-01-01 00:00:00").unwrap();
        assert_eq!(print_of("Y-m-d H:M:S", v).unwrap(), "2023-01-01 00:00:00");
    }

    #[test]
    fn invalid_patterns() {
        for pat in [
            "",
            "x",
            "-",
            "Y",
            "Ym",
            "Y-m",
            "YY-m-d",
            "m-d-d",
            "Y-m-d M",
            "Y-m-d H:S",
            "Y-m-d H:M:Sf.3 f.3",
            "f",
            "f.0",
            "f.3",
            "H:Mf.3",
            "H:M:Sf:3",
            "\\",
            "a",
            "a H",
            "H\x01",
            "H\n",
            "H:M:S\x04",
            "b b d",
            "d d m",
        ] {
            assert!(!valid(pat.as_bytes()), "{pat:?}");
        }
        for pat in
            ["H", "H:M", "Y-m-d", "b e H:M:S", "\\x H \\H\\:M", "H:M:St,9", "a b d H:M:S Y", "H:M:S\x05\x7f\u{e9}"]
        {
            assert!(valid(pat.as_bytes()), "{pat:?}");
        }
        assert!(valid(&[b'-'; MAX_PATTERN - 1].iter().chain(b"H").copied().collect::<Vec<_>>()));
        assert!(!valid(&[b'-'; MAX_PATTERN].iter().chain(b"H").copied().collect::<Vec<_>>()));
        // a letter that is no item is an error, escaped it is the byte
        assert_eq!(print_of("\\x H", 3).as_deref(), Some("x 03"));
    }

    #[test]
    fn out_of_range() {
        assert_eq!(print_of("H:M:S", 86400), None);
        assert_eq!(print_of("H:M:S", -1), None);
        assert_eq!(print_of("H:M:S", 86399).as_deref(), Some("23:59:59"));
        assert_eq!(print_of("Y-m-d", MAX_DAYS + 1), None);
        assert_eq!(print_of("Y-m-d", MIN_DAYS - 1), None);
        assert_eq!(print_of("Y-m-d", i64::MAX), None);
        // (the fraction of nine digits reaches 1677 and 2262 only)
        assert_eq!(print_of("Y-m-d H:M:Sf.9", i64::MAX).as_deref(), Some("2262-04-11 23:47:16.854775807"));
        assert_eq!(print_of("b e H:M:S", 0), None);
        assert_eq!(print_of("b e H:M:S", (10957 + 366) * 86400), None);
        assert_eq!(print_of("b e H:M:S", 10957 * 86400 - 1), None);
        // (a pattern of no valid shape prints nothing, and what was is left)
        let mut o = b"ab".to_vec();
        assert!(!put(&mut o, b"Y", 1) && o == b"ab");
        assert!(!put(&mut o, b"H:M", 24 * 60) && o == b"ab");
    }

    #[test]
    fn every_value_prints_or_fails() {
        let mut r = 0x9E3779B97F4A7C15u64;
        let mut next = || {
            r ^= r << 13;
            r ^= r >> 7;
            r ^= r << 17;
            r
        };
        let pats =
            ["Y-m-d H:M:S", "Y-m-d H:M:St.6", "b e H:M:S", "H:M:Sf.9", "a b d H:M:S Y", "d/m/Y", "h:M", "Y-m-d H"];
        let mut printed = 0;
        for i in 0..200000 {
            let x = next();
            let v = match i % 4 {
                0 => x as i64,
                1 => (x as i64) >> (x % 60),
                2 => (x % 4_000_000_000) as i64 * 86400 / 1000,
                _ => (x % 10_000_000) as i64 - 100,
            };
            for p in pats {
                let mut o = vec![];
                if put(&mut o, p.as_bytes(), v) {
                    printed += 1;
                    // (what it prints it parses back to the same integer)
                    let (w, n) =
                        parse(p.as_bytes(), &o).unwrap_or_else(|| panic!("{p} {v} {}", String::from_utf8_lossy(&o)));
                    assert_eq!((w, n), (v, o.len()), "{p} {}", String::from_utf8_lossy(&o));
                } else {
                    assert!(o.is_empty());
                }
            }
        }
        assert!(printed > 100000, "{printed}");
    }
}
