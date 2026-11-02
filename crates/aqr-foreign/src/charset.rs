//! Names of no declared encoding (`docs/names-design.md`): the character sets they may be in
//! ([`Charset`]), what is asked of them (`-E`, [`Choice`]), and what an archive's names are
//! decided to be in ([`decide`]), guessed by the characters common in names of each script
//! where not given.

mod common;

/// A character set that names may be in: one of the WHATWG Encoding Standard's (those
/// compatible with ASCII), IBM code page 437 (which it has not), or on Windows any code page of
/// the system's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Charset {
    Std(&'static encoding_rs::Encoding),
    Ibm437,
    #[cfg(windows)]
    CodePage(u32),
}

/// The candidates of a guess where none are given, in order: the multi-byte sets of East Asia,
/// then code page 437 (which decodes anything: the last resort).
pub const DEFAULT: [Charset; 5] = [
    Charset::Std(encoding_rs::SHIFT_JIS),
    Charset::Std(encoding_rs::EUC_KR),
    Charset::Std(encoding_rs::GBK),
    Charset::Std(encoding_rs::BIG5),
    Charset::Ibm437,
];

pub const UTF_8: Charset = Charset::Std(encoding_rs::UTF_8);

/// Code page 437's characters of bytes 80–FF (those below as ASCII's).
const IBM437_HIGH: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}";

impl Charset {
    /// The set of label `s`: WHATWG's labels (`shift_jis`, `euc-kr`, `gbk`, `big5`, `utf-8`,
    /// ...), and `cp437`, `ibm437` or `437`, and Windows' `cp932`, `cp936`, `cp949`, `cp950`
    /// (which WHATWG's sets are). Sets not compatible with ASCII (UTF-16, ISO-2022-JP) are not.
    pub fn by_label(s: &str) -> Option<Charset> {
        let lower = s.trim().to_ascii_lowercase();
        let std = |e| Some(Charset::Std(e));
        match &*lower {
            "437" | "cp437" | "ibm437" | "ibm-437" | "oem-us" => return Some(Charset::Ibm437),
            "932" | "cp932" | "ms932" | "windows-932" | "sjis" => {
                return std(encoding_rs::SHIFT_JIS);
            }
            "936" | "cp936" => return std(encoding_rs::GBK),
            "949" | "cp949" | "uhc" | "euckr" => return std(encoding_rs::EUC_KR),
            "950" | "cp950" => return std(encoding_rs::BIG5),
            "utf8" => return Some(UTF_8),
            _ => {}
        }
        let e = encoding_rs::Encoding::for_label_no_replacement(lower.as_bytes())?;
        (e.is_ascii_compatible() && e != encoding_rs::ISO_2022_JP).then_some(Charset::Std(e))
    }

    /// Its name, as people are told it.
    pub fn name(&self) -> String {
        match self {
            Charset::Std(e) => e.name().to_ascii_lowercase(),
            Charset::Ibm437 => "ibm437".into(),
            #[cfg(windows)]
            Charset::CodePage(n) => format!("cp{n}"),
        }
    }

    /// `b` decoded: `None` where it is not of this set (or ends amid a character).
    pub fn decode(&self, b: &[u8]) -> Option<String> {
        match self {
            Charset::Std(e) => e.decode_without_bom_handling_and_without_replacement(b).map(|s| s.into_owned()),
            Charset::Ibm437 => Some(b.iter().map(|&c| ibm437(c)).collect()),
            #[cfg(windows)]
            &Charset::CodePage(n) => windows::decode(n, b),
        }
    }

    /// The set of the system's code page: the OEM one (of MS-DOS, which archivers of Windows
    /// write names in) where `oem`, else the ANSI one. Elsewhere, the locale's
    /// (`LC_ALL`, `LC_CTYPE`, `LANG`), UTF-8 where it tells none.
    pub fn system(oem: bool) -> Option<Charset> {
        #[cfg(windows)]
        return Some(windows::system(oem));
        #[cfg(not(windows))]
        {
            let _ = oem;
            let locale = locale()?;
            let set = locale.split('.').nth(1).map(|s| s.split('@').next().unwrap_or(s));
            set.map_or(Some(UTF_8), Charset::by_label)
        }
    }

    /// The set that names are more likely in where guessed, by the system's language: of
    /// Japanese, Korean, Chinese (simplified or traditional).
    pub fn likely() -> Option<Charset> {
        #[cfg(windows)]
        return Charset::system(false).filter(|c| DEFAULT.contains(c));
        #[cfg(not(windows))]
        {
            let l = locale()?;
            let lang = l.split(['.', '@']).next().unwrap_or("");
            let e = match lang.split(['_', '-']).next().unwrap_or("") {
                "ja" => encoding_rs::SHIFT_JIS,
                "ko" => encoding_rs::EUC_KR,
                "zh" if ["_TW", "_HK", "_MO", "-Hant"].iter().any(|r| lang.contains(r)) => encoding_rs::BIG5,
                "zh" => encoding_rs::GBK,
                _ => return None,
            };
            Some(Charset::Std(e))
        }
    }
}

/// The locale of the environment (`LC_ALL`, `LC_CTYPE`, `LANG`, the first set).
#[cfg(not(windows))]
fn locale() -> Option<String> {
    ["LC_ALL", "LC_CTYPE", "LANG"].iter().filter_map(|v| std::env::var(v).ok()).find(|v| !v.is_empty())
}

fn ibm437(c: u8) -> char {
    match c {
        0..0x80 => c as char,
        _ => IBM437_HIGH.chars().nth((c - 0x80) as usize).unwrap(),
    }
}

#[cfg(windows)]
mod windows {
    use windows_sys::Win32::Globalization::{GetACP, GetOEMCP, MB_ERR_INVALID_CHARS, MultiByteToWideChar};

    use super::Charset;

    pub fn system(oem: bool) -> Charset {
        // SAFETY: no arguments
        let cp = unsafe { if oem { GetOEMCP() } else { GetACP() } };
        let std = |e| Charset::Std(e);
        match cp {
            437 => Charset::Ibm437,
            932 => std(encoding_rs::SHIFT_JIS),
            936 => std(encoding_rs::GBK),
            949 => std(encoding_rs::EUC_KR),
            950 => std(encoding_rs::BIG5),
            65001 => super::UTF_8,
            n => Charset::CodePage(n),
        }
    }

    /// `b` of code page `cp` decoded: `None` where it is not of it.
    pub fn decode(cp: u32, b: &[u8]) -> Option<String> {
        if b.is_empty() {
            return Some(String::new());
        }
        let len = i32::try_from(b.len()).ok()?;
        let mut w = vec![0u16; b.len()];
        // SAFETY: a buffer of as many units as there are bytes (no character takes more)
        let n = unsafe { MultiByteToWideChar(cp, MB_ERR_INVALID_CHARS, b.as_ptr(), len, w.as_mut_ptr(), len) };
        (n > 0).then(|| String::from_utf16(&w[..n as usize]).ok()).flatten()
    }
}

/// What is asked of names of no declared encoding (`-E`).
#[derive(Clone, PartialEq, Debug, Default)]
pub enum Choice {
    /// Decided by the names (a set guessed where they are not UTF-8).
    #[default]
    Auto,
    /// Kept as they are.
    Raw,
    /// The system's code page ([`Charset::system`]).
    System,
    /// In this set; or guessed of these sets where more than one.
    Of(Vec<Charset>),
}

impl Choice {
    /// The choice of `s`: `auto`, `raw`, `system`, or labels ([`Charset::by_label`]) separated
    /// by commas.
    pub fn parse(s: &str) -> Result<Choice, String> {
        match s {
            "auto" => return Ok(Choice::Auto),
            "raw" => return Ok(Choice::Raw),
            "system" => return Ok(Choice::System),
            _ => {}
        }
        let sets = s.split(',').map(|l| Charset::by_label(l).ok_or_else(|| format!("{l}: not a character set known")));
        Ok(Choice::Of(sets.collect::<Result<_, _>>()?))
    }
}

/// What names of no declared encoding were decided to be ([`decide`]).
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Decision {
    /// The set they are decoded from (`None`: kept as they are).
    pub charset: Option<Charset>,
    /// The set whose characters a path kept as it is is split between (`None`: none known; at
    /// ASCII's then), where not `charset`.
    pub bounds: Option<Charset>,
    /// Whether the set was guessed, not sure: the others that could be, the likelier first.
    pub guessed: bool,
    pub others: Vec<Charset>,
    /// Whether a name that is UTF-8 is taken as UTF-8 before the set (of names of other sets
    /// beside some of UTF-8: those of a set not ASCII's are rarely UTF-8).
    pub utf8_first: bool,
}

impl Decision {
    /// Kept as they are, split between the characters of `bounds`.
    pub fn raw(bounds: Option<Charset>) -> Decision {
        Decision { bounds, ..Decision::default() }
    }

    /// Decoded from `cs`, as given.
    pub fn of(cs: Charset) -> Decision {
        Decision { charset: Some(cs), bounds: Some(cs), ..Decision::default() }
    }

    /// `b` decoded, where it is of the set decided (or UTF-8 taken first).
    pub fn decode(&self, b: &[u8]) -> Option<String> {
        match std::str::from_utf8(b) {
            Ok(s) if self.utf8_first && self.charset.is_some() => Some(s.to_owned()),
            _ => self.charset?.decode(b),
        }
    }

    /// Whether byte `at` of `path` (not decoded) begins a character, by the set its characters
    /// are split between; where none is known, whether all bytes before it are ASCII.
    pub fn starts(&self, path: &[u8], at: usize) -> bool {
        if self.utf8_first && std::str::from_utf8(path).is_ok() {
            return std::str::from_utf8(&path[..at]).is_ok();
        }
        match self.bounds.or(self.charset) {
            Some(cs) if cs.decode(path).is_some() => cs.decode(&path[..at]).is_some(),
            _ => path[..at].is_ascii(),
        }
    }

    /// The decision told: `N names of no declared encoding read as X (guessed; also possible:
    /// Y)`, of `n` names; `None` where nothing was decided of them (all ASCII, or kept as they
    /// are, or UTF-8 found).
    pub fn told(&self, n: usize) -> Option<String> {
        let cs = self.charset?;
        if !self.guessed || cs == UTF_8 {
            return None;
        }
        let names = if n == 1 { "name" } else { "names" };
        let others: Vec<String> = self.others.iter().map(Charset::name).collect();
        let also = if others.is_empty() { String::new() } else { format!("; also possible: {}", others.join(", ")) };
        Some(format!("{n} {names} of no declared encoding read as {} (guessed{also}; -E to choose)", cs.name()))
    }
}

/// What `names` (all of an archive's of no declared encoding) are decided to be, as `choice`
/// asks: where it is [`Choice::Auto`], UTF-8 where they all are, else (each of them UTF-8 as
/// UTF-8) decoded from the set guessed of the others where `decode`, else kept, split as that
/// set has them. `system` and `likely` are [`Charset::system`] and [`Charset::likely`] (given,
/// as they are the system's).
pub fn decide(
    choice: &Choice,
    names: &[&[u8]],
    decode: bool,
    system: Option<Charset>,
    likely: Option<Charset>,
) -> Decision {
    let all_utf8 = || names.iter().all(|n| std::str::from_utf8(n).is_ok());
    let others: Vec<&[u8]> = names.iter().copied().filter(|n| std::str::from_utf8(n).is_err()).collect();
    match choice {
        Choice::Raw if all_utf8() => Decision::raw(Some(UTF_8)),
        Choice::Raw => Decision { utf8_first: true, ..Decision::raw(guess(&others, &DEFAULT, likely).best) },
        Choice::System => system.map_or_else(|| Decision::raw(None), Decision::of),
        Choice::Of(sets) if sets.len() == 1 => Decision::of(sets[0]),
        Choice::Of(sets) => guessed(names, sets, likely, decode),
        Choice::Auto if all_utf8() => Decision { charset: decode.then_some(UTF_8), ..Decision::raw(Some(UTF_8)) },
        Choice::Auto => {
            let mut sets = DEFAULT.to_vec();
            if let Some(s) = system.filter(|s| !sets.contains(s) && *s != UTF_8) {
                sets.insert(sets.len() - 1, s);
            }
            Decision { utf8_first: true, ..guessed(&others, &sets, likely, decode) }
        }
    }
}

fn guessed(names: &[&[u8]], sets: &[Charset], likely: Option<Charset>, decode: bool) -> Decision {
    let g = guess(names, sets, likely);
    match decode {
        true => Decision { charset: g.best, bounds: g.best, guessed: !g.sure, others: g.others, utf8_first: false },
        false => Decision::raw(g.best),
    }
}

/// A guess of the set of names ([`guess`]).
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Guess {
    /// The likeliest (`None`: no candidate decodes a name).
    pub best: Option<Charset>,
    /// Whether it decodes all the names, far likelier than any other.
    pub sure: bool,
    /// The other candidates nearly as likely, the likelier first.
    pub others: Vec<Charset>,
}

/// The likeliest of `candidates` to be the set of `names`: the one decoding them into
/// characters common in names ([`score`]), less for each name it does not decode, `likely`
/// taken before others nearly as likely.
pub fn guess(names: &[&[u8]], candidates: &[Charset], likely: Option<Charset>) -> Guess {
    // (by candidate: names decoded, the score per character not ASCII, less for names not)
    let mut rated: Vec<(Charset, usize, f64)> = Vec::new();
    for &cs in candidates {
        let (mut decoded, mut sum, mut chars) = (0usize, 0.0, 0usize);
        for n in names {
            let Some(s) = cs.decode(n) else { continue };
            decoded += 1;
            for c in s.chars().filter(|c| !c.is_ascii()) {
                sum += score(cs, c);
                chars += 1;
            }
        }
        let mut avg = if chars == 0 { 0.0 } else { sum / chars as f64 };
        if Some(cs) == likely {
            avg += LIKELY;
        }
        avg -= FAILED * (names.len() - decoded) as f64 / names.len().max(1) as f64;
        rated.push((cs, decoded, avg));
    }
    rated.sort_by(|a, b| b.2.total_cmp(&a.2));
    let Some(&(best, decoded, avg)) = rated.first().filter(|r| r.1 > 0) else {
        return Guess::default();
    };
    let near: Vec<Charset> = rated[1..].iter().filter(|r| r.1 > 0 && avg - r.2 < MARGIN).map(|r| r.0).collect();
    Guess { best: Some(best), sure: decoded == names.len() && avg >= SURE && near.is_empty(), others: near }
}

/// What a guess takes as sure: the score per character of the best at least, and its lead.
const SURE: f64 = 1.5;
const MARGIN: f64 = 0.75;
/// The score per character added to the set of the system's language ([`Charset::likely`]).
const LIKELY: f64 = 0.3;
/// The score per character taken from a set for its names not decoded, all of them (in part,
/// in proportion).
const FAILED: f64 = 4.0;

/// How likely `c` (not ASCII), decoded from `cs`, is in a name: of the characters common in
/// names of the script that `cs` is of (kana, Hangul, Hanzi, Kanji) highest, of others of the
/// script lower, and of characters seen of bytes decoded from a wrong set (halfwidth katakana,
/// private use, box drawing, symbols) below 0.
pub fn score(cs: Charset, c: char) -> f64 {
    let common = |list: &[char]| list.binary_search(&c).is_ok();
    let list: Option<&[char]> = match cs {
        Charset::Std(e) if e == encoding_rs::SHIFT_JIS || e == encoding_rs::EUC_JP => Some(common::KANJI),
        Charset::Std(e) if e == encoding_rs::GBK || e == encoding_rs::GB18030 => Some(common::HANS),
        Charset::Std(e) if e == encoding_rs::BIG5 => Some(common::HANT),
        _ => None,
    };
    match c as u32 {
        // kana (but halfwidth), the prolonged sound mark
        0x3041..=0x3096 | 0x30a1..=0x30fa | 0x30fc => 3.0,
        0xac00..=0xd7a3 if common(common::HANGUL) => 3.0,
        0xac00..=0xd7a3 => 0.5,
        0x4e00..=0x9fff => match list {
            Some(l) if common(l) => 3.0,
            Some(_) => 0.5,
            None => 0.0,
        },
        // CJK punctuation, fullwidth ASCII
        0x3000..=0x303f | 0xff01..=0xff5e => 1.0,
        // Latin letters of Latin-1 and Extended-A, Greek, Cyrillic
        0xc0..=0x17f | 0x370..=0x3ff | 0x400..=0x4ff => 1.0,
        // halfwidth katakana
        0xff61..=0xff9f => -2.0,
        // box drawing, blocks
        0x2500..=0x259f => -2.0,
        // private use
        0xe000..=0xf8ff => -3.0,
        // symbols of Latin-1, general punctuation, technical and mathematical symbols
        0x80..=0xbf | 0x2000..=0x2bff => -1.0,
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(cs: &'static encoding_rs::Encoding, s: &str) -> Vec<u8> {
        let (b, _, bad) = cs.encode(s);
        assert!(!bad, "{s} not of {}", cs.name());
        b.into_owned()
    }

    #[test]
    fn tables() {
        assert_eq!(IBM437_HIGH.chars().count(), 128);
        for list in [common::HANGUL, common::HANS, common::HANT, common::KANJI] {
            assert!(list.windows(2).all(|w| w[0] < w[1]));
        }
        assert_eq!(Charset::Ibm437.decode(b"a\x82\xc4").as_deref(), Some("aé─"));
    }

    #[test]
    fn labels() {
        assert_eq!(Charset::by_label("Shift_JIS"), Some(Charset::Std(encoding_rs::SHIFT_JIS)));
        assert_eq!(Charset::by_label("cp949"), Some(Charset::Std(encoding_rs::EUC_KR)));
        assert_eq!(Charset::by_label("cp437"), Some(Charset::Ibm437));
        assert_eq!(Charset::by_label("utf-16le"), None);
        assert_eq!(Charset::by_label("iso-2022-jp"), None);
        assert_eq!(Charset::by_label("nonsense"), None);
        assert_eq!(Choice::parse("auto"), Ok(Choice::Auto));
        assert_eq!(
            Choice::parse("sjis,euc-kr"),
            Ok(Choice::Of(vec![Charset::Std(encoding_rs::SHIFT_JIS), Charset::Std(encoding_rs::EUC_KR)]))
        );
        assert!(Choice::parse("sjis,x").is_err());
    }

    /// The names of a few archives each of a language, encoded as its system would.
    const JA: &[&str] =
        &["写真/夏休み.jpg", "資料/会議の議事録.docx", "新しいフォルダー/表示.txt", "ソフト/説明書.pdf"];
    const KO: &[&str] = &["사진/여름 휴가.jpg", "자료/회의록 최종.hwp", "새 폴더/보고서.docx", "프로젝트/설명서.pdf"];
    const HANS: &[&str] = &["照片/暑假.jpg", "资料/会议记录.docx", "新建文件夹/报告.txt", "软件/说明书.pdf"];
    const HANT: &[&str] = &["照片/暑假.jpg", "資料/會議記錄.docx", "新增資料夾/報告.txt", "軟體/說明書.pdf"];

    fn guessed(cs: &'static encoding_rs::Encoding, names: &[&str]) -> Guess {
        let b: Vec<Vec<u8>> = names.iter().map(|n| enc(cs, n)).collect();
        let refs: Vec<&[u8]> = b.iter().map(|n| &n[..]).collect();
        guess(&refs, &DEFAULT, None)
    }

    #[test]
    fn guesses() {
        for (cs, names) in [
            (encoding_rs::SHIFT_JIS, JA),
            (encoding_rs::EUC_KR, KO),
            (encoding_rs::GBK, HANS),
            (encoding_rs::BIG5, HANT),
        ] {
            let g = guessed(cs, names);
            assert_eq!(g.best, Some(Charset::Std(cs)), "{}: {g:?}", cs.name());
            assert!(g.sure, "{}: {g:?}", cs.name());
        }
    }

    #[test]
    fn a_name_broken() {
        // (one name not of Shift_JIS among those that are: Shift_JIS still, not code page 437,
        // which decodes anything)
        let mut b: Vec<Vec<u8>> = JA.iter().map(|n| enc(encoding_rs::SHIFT_JIS, n)).collect();
        b.push(b"broken\x82\x20.txt".to_vec());
        let refs: Vec<&[u8]> = b.iter().map(|n| &n[..]).collect();
        let g = guess(&refs, &DEFAULT, None);
        assert_eq!(g.best, Some(Charset::Std(encoding_rs::SHIFT_JIS)), "{g:?}");
        assert!(!g.sure);
    }

    #[test]
    fn a_name_alone() {
        // (a short name of Korean, whose bytes are Hanzi of GB 2312 as well: still Korean)
        let g = guessed(encoding_rs::EUC_KR, &["보고서.hwp"]);
        assert_eq!(g.best, Some(Charset::Std(encoding_rs::EUC_KR)), "{g:?}");
        // (a name of Latin-1 letters: code page 437's)
        let g = guess(&[b"caf\x82.txt"], &DEFAULT, None);
        assert_eq!(g.best, Some(Charset::Ibm437), "{g:?}");
    }

    #[test]
    fn decisions() {
        let sjis = enc(encoding_rs::SHIFT_JIS, "表\\ソ.txt");
        let names: &[&[u8]] = &[&sjis];
        let d = decide(&Choice::Auto, names, true, None, None);
        assert_eq!(d.charset, Some(Charset::Std(encoding_rs::SHIFT_JIS)));
        assert_eq!(d.decode(&sjis).as_deref(), Some("表\\ソ.txt"));
        // (kept, split as Shift_JIS has its characters: the 5C of 表 (95 5C) not one)
        let d = decide(&Choice::Auto, names, false, None, None);
        assert_eq!(d.charset, None);
        assert!(!d.starts(&sjis, 1) && d.starts(&sjis, 2) && !d.starts(&sjis, 4));
        assert_eq!(decide(&Choice::Auto, &[b"plain", "名前".as_bytes()], true, None, None).charset, Some(UTF_8));
        let only = Choice::Of(vec![Charset::Std(encoding_rs::EUC_KR)]);
        assert_eq!(decide(&only, names, true, None, None).charset, Some(Charset::Std(encoding_rs::EUC_KR)));
    }
}
