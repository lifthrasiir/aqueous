//! Round-trip and compression benchmark over large PE files available on the local machine.
//!
//! usage: bench [-l LEVEL] [--no-zstd] [NAME|PATH ...]
//!
//! Without arguments every corpus entry that exists is run; a NAME selects corpus entries
//! by substring, and an existing PATH is run as is. Each executable section is filtered,
//! verified to round-trip, and compressed with zstd before and after filtering.
//! Raw compressed sizes don't depend on the filter and are cached under `target/`.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Instant, UNIX_EPOCH};

use aqr_exe::arch::x86::{decode, encode};
use aqr_exe::format::pe::locate_code;

// Picked to cover different toolchains; path components may contain `*`
// (the lexicographically last match wins) and a leading `~` is the user profile.
const CORPUS: &[(&str, &str)] = &[
    // MinGW GCC
    ("ffmpeg", r"C:\Program Files\ImageMagick-*\ffmpeg.exe"),
    // clang-cl
    (
        "chrome",
        r"C:\Program Files (x86)\Google\Chrome\Application\*\chrome.dll",
    ),
    ("xul", r"C:\Program Files (x86)\Mozilla Firefox\xul.dll"),
    // clang-cl, x86-32
    (
        "libcef32",
        r"C:\Program Files (x86)\Epic Games\Launcher\Portal\Extras\Overlay\Win32\libcef.dll",
    ),
    // MSVC
    ("amd_comgr", r"C:\Windows\System32\amd_comgr_2.dll"),
    (
        "tensorflow",
        r"C:\Program Files (x86)\Microsoft Visual Studio\*\Community\Common7\IDE\CommonExtensions\Microsoft\ModelBuilder\AutoMLService\tensorflow.dll",
    ),
    (
        "qtwebengine",
        r"C:\Program Files (x86)\Ubisoft\Ubisoft Game Launcher\shareplay\Qt5WebEngineCore.dll",
    ),
    (
        "node",
        r"C:\Program Files\LM Studio\resources\app\.webpack\bin\node.exe",
    ),
    // MSVC, x86-32
    (
        "comgr32",
        r"C:\Windows\System32\DriverStore\FileRepository\amdocl.inf_amd64_*\amd_comgr32.dll",
    ),
    // rustc (LLVM)
    (
        "rustc_driver",
        r"~\.rustup\toolchains\*\bin\rustc_driver-*.dll",
    ),
    // Go
    ("go", r"D:\Software\Dev\Go\bin\go.exe"),
];

fn wildcard_match(pat: &str, s: &str) -> bool {
    let pat = pat.to_ascii_lowercase();
    let s = s.to_ascii_lowercase();
    let mut parts = pat.split('*');
    let first = parts.next().unwrap();
    let Some(mut rest) = s.strip_prefix(first) else {
        return false;
    };
    let mut parts: Vec<&str> = parts.collect();
    let Some(last) = parts.pop() else {
        return rest.is_empty();
    };
    for part in parts {
        let Some(pos) = rest.find(part) else {
            return false;
        };
        rest = &rest[pos + part.len()..];
    }
    rest.len() >= last.len() && rest.ends_with(last)
}

fn resolve(pattern: &str) -> Option<PathBuf> {
    let pattern = match pattern.strip_prefix('~') {
        Some(rest) => format!("{}{rest}", std::env::var("USERPROFILE").ok()?),
        None => pattern.to_owned(),
    };
    let comps: Vec<&str> = pattern.split('\\').collect();
    resolve_from(PathBuf::from(format!("{}\\", comps[0])), &comps[1..])
}

fn resolve_from(path: PathBuf, comps: &[&str]) -> Option<PathBuf> {
    let Some((comp, rest)) = comps.split_first() else {
        return path.exists().then_some(path);
    };
    if !comp.contains('*') {
        return resolve_from(path.join(comp), rest);
    }
    let mut names: Vec<String> = fs::read_dir(&path)
        .ok()?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|name| wildcard_match(comp, name))
        .collect();
    names.sort();
    names
        .iter()
        .rev()
        .find_map(|name| resolve_from(path.join(name), rest))
}

/// Raw zstd sizes keyed by `path|len|mtime|level|va`.
struct RawCache {
    path: PathBuf,
    entries: HashMap<String, u64>,
}

impl RawCache {
    fn load() -> Self {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/bench-raw-zstd.tsv");
        let entries = fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| {
                let (k, v) = line.rsplit_once('\t')?;
                Some((k.to_owned(), v.parse().ok()?))
            })
            .collect();
        Self { path, entries }
    }

    fn get_or(&mut self, key: String, f: impl FnOnce() -> u64) -> u64 {
        if let Some(&v) = self.entries.get(&key) {
            return v;
        }
        let v = f();
        self.entries.insert(key.clone(), v);
        if let Ok(mut f) = File::options().create(true).append(true).open(&self.path) {
            let _ = writeln!(f, "{key}\t{v}");
        }
        v
    }
}

struct Counter(u64);

impl Write for Counter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0 += buf.len() as u64;
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn zstd_size(data: &[u8], level: i32) -> u64 {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()) as u32;
    let mut enc = zstd::stream::write::Encoder::new(Counter(0), level).unwrap();
    enc.multithread(threads).unwrap();
    enc.include_checksum(false).unwrap();
    enc.write_all(data).unwrap();
    enc.finish().unwrap().0
}

#[derive(Default)]
struct Totals {
    size: u64,
    enc_secs: f64,
    dec_secs: f64,
    raw_z: u64,
    filt_z: u64,
}

fn mb(n: u64) -> f64 {
    n as f64 / 1e6
}

fn report(name: &str, bits: &str, t: &Totals, zstd: bool) {
    print!(
        "{name:<14} {bits:>3} {:8.1} MB  enc {:6.1} MB/s  dec {:6.1} MB/s",
        mb(t.size),
        mb(t.size) / t.enc_secs,
        mb(t.size) / t.dec_secs,
    );
    if zstd {
        print!(
            "  zstd {:8.2} -> {:8.2} MB ({:+.2}%)",
            mb(t.raw_z),
            mb(t.filt_z),
            (t.filt_z as f64 / t.raw_z as f64 - 1.0) * 100.0,
        );
    }
    println!();
}

fn run(
    name: &str,
    path: &Path,
    level: Option<i32>,
    cache: &mut RawCache,
    all: &mut Totals,
) -> io::Result<bool> {
    let mut f = File::open(path)?;
    let meta = f.metadata()?;
    let mtime = meta
        .modified()?
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let code = locate_code(&mut f)?;

    let mut t = Totals::default();
    for &(va, offset, size) in &code.sections {
        f.seek(SeekFrom::Start(offset))?;
        let mut input = vec![0u8; size];
        f.read_exact(&mut input)?;

        let start = Instant::now();
        let Some(streams) = encode(input.clone(), va, code.is64) else {
            eprintln!("{name}: failed to encode section at {va:#x}");
            return Ok(false);
        };
        t.enc_secs += start.elapsed().as_secs_f64();

        let start = Instant::now();
        let recons = decode(&(&streams).into(), code.is64);
        t.dec_secs += start.elapsed().as_secs_f64();
        if recons.as_deref() != Some(&input[..]) {
            match recons {
                None => eprintln!("{name}: failed to decode section at {va:#x}"),
                Some(recons) => {
                    let pos = std::iter::zip(&input, &recons)
                        .position(|(a, b)| a != b)
                        .unwrap_or(input.len().min(recons.len()));
                    let lo = pos.saturating_sub(15);
                    eprintln!(
                        "{name}: mismatch at {va:#x}+{pos:#x} (input {}, recons {} bytes)\n    \
                         input:  {:02X?}\n    recons: {:02X?}",
                        input.len(),
                        recons.len(),
                        &input[lo..(pos + 15).min(input.len())],
                        &recons[lo..(pos + 15).min(recons.len())],
                    );
                }
            }
            return Ok(false);
        }

        if let Some(level) = level {
            let key = format!("{}|{}|{mtime}|{level}|{va:#x}", path.display(), meta.len());
            t.raw_z += cache.get_or(key, || zstd_size(&input, level));
            let mut filtered = Vec::new();
            streams.write_to(&mut filtered)?;
            t.filt_z += zstd_size(&filtered, level);
        }
        t.size += size as u64;
    }

    report(
        name,
        if code.is64 { "x64" } else { "x86" },
        &t,
        level.is_some(),
    );
    all.size += t.size;
    all.enc_secs += t.enc_secs;
    all.dec_secs += t.dec_secs;
    all.raw_z += t.raw_z;
    all.filt_z += t.filt_z;
    Ok(true)
}

fn main() {
    let mut level = Some(9);
    let mut selectors = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match &*arg {
            "-l" => level = Some(args.next().and_then(|s| s.parse().ok()).expect("-l LEVEL")),
            "--no-zstd" => level = None,
            _ => selectors.push(arg),
        }
    }

    let mut targets = Vec::new();
    for (name, pattern) in CORPUS {
        if selectors.is_empty() || selectors.iter().any(|s| name.contains(&**s)) {
            match resolve(pattern) {
                Some(path) => targets.push((name.to_string(), path)),
                None => eprintln!("{name}: not found, skipped ({pattern})"),
            }
        }
    }
    for s in &selectors {
        let path = Path::new(s);
        if path.is_file() {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            targets.push((name, path.to_owned()));
        }
    }

    let mut cache = RawCache::load();
    let mut all = Totals::default();
    let mut failed = 0;
    for (name, path) in &targets {
        match run(name, path, level, &mut cache, &mut all) {
            Ok(true) => {}
            Ok(false) => failed += 1,
            Err(e) => {
                eprintln!("{name}: {e}");
                failed += 1;
            }
        }
    }
    if targets.len() - failed > 1 {
        report("(total)", "", &all, level.is_some());
    }
    if failed > 0 {
        eprintln!("{failed} file(s) failed");
        std::process::exit(1);
    }
}
