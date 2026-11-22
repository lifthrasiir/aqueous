//! Seeds made of the formats of the codecs whose tests make their inputs in code (no files of
//! theirs to start from), for `compress` (and `natives`, of those recompressed): written here
//! (text of each kind, gzip, Zstandard, PNG, safetensors, asar, Minecraft's region files) or
//! by tools where they are installed (sqlite3, ffmpeg, msgfmt, cc, xz), of synthetic data only.
//! Each at most `FIXTURE_MAX` bytes (the seeds program's).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A generator of numbers, of a seed: xorshift64*.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[self.below(xs.len() as u64) as usize]
    }
}

const WORDS: [&str; 24] = [
    "the", "archive", "of", "files", "and", "a", "stream", "is", "written", "again", "where", "it", "was", "read",
    "by", "its", "model", "data", "block", "each", "one", "to", "from", "with",
];

/// Lines of a log: times, levels, components, numbers and addresses.
fn log(seed: u64, n: usize) -> Vec<u8> {
    let mut r = Rng::new(seed);
    let mut out = String::new();
    let mut t = 1_700_000_000u64;
    for _ in 0..n {
        t += r.below(3000);
        let (s, ms) = (t % 86400, r.below(1000));
        out += &format!(
            "2024-05-{:02}T{:02}:{:02}:{:02}.{ms:03}Z {} worker-{} request id={} took {}ms status={} ip=10.0.{}.{}\n",
            1 + t / 86400 % 28,
            s / 3600,
            s / 60 % 60,
            s % 60,
            r.pick(&["INFO", "WARN", "DEBUG", "ERROR"]),
            r.below(8),
            r.below(100_000),
            r.below(500),
            r.pick(&["200", "200", "404", "500"]),
            r.below(4),
            r.below(256),
        );
    }
    out.into_bytes()
}

/// A table of delimiter `d`: integers, decimals, dates and words, a header first.
fn table(seed: u64, n: usize, d: char) -> Vec<u8> {
    let mut r = Rng::new(seed);
    let mut out = format!("id{d}date{d}amount{d}count{d}name{d}note\n");
    for i in 0..n {
        out += &format!(
            "{}{d}2023-{:02}-{:02}{d}{}.{:02}{d}{}{d}{}{d}\"{} {}\"\n",
            1000 + i,
            1 + r.below(12),
            1 + r.below(28),
            r.below(10_000),
            r.below(100),
            r.below(50),
            r.pick(&WORDS),
            r.pick(&WORDS),
            r.pick(&WORDS),
        );
    }
    out.into_bytes()
}

/// JSON Lines of records of a few keys.
fn jsonl(seed: u64, n: usize) -> Vec<u8> {
    let mut r = Rng::new(seed);
    let mut out = String::new();
    for i in 0..n {
        out += &format!(
            "{{\"id\":{i},\"name\":\"{}\",\"score\":{}.{},\"tags\":[\"{}\",\"{}\"],\"ok\":{}}}\n",
            r.pick(&WORDS),
            r.below(100),
            r.below(1000),
            r.pick(&WORDS),
            r.pick(&WORDS),
            r.below(2) == 1,
        );
    }
    out.into_bytes()
}

/// Prose of the words, in sentences and paragraphs; of markup where `xml`.
fn prose(seed: u64, n: usize, xml: bool) -> Vec<u8> {
    let mut r = Rng::new(seed);
    let mut out = String::new();
    for p in 0..n {
        if xml {
            out += &format!("<p id=\"p{p}\" class=\"{}\">", r.pick(&WORDS));
        }
        for _ in 0..1 + r.below(6) {
            let w: Vec<&str> = (0..3 + r.below(12)).map(|_| r.pick(&WORDS)).collect();
            let s = w.join(" ");
            out += &(s[..1].to_uppercase() + &s[1..]);
            out += r.pick(&[". ", ", and ", "; ", "? "]);
        }
        out += if xml { "</p>\n" } else { "\n\n" };
    }
    out.into_bytes()
}

/// FASTA records of nucleotides: lines of 60, runs of N and of lower case now and then.
fn fasta(seed: u64, n: usize) -> Vec<u8> {
    let mut r = Rng::new(seed);
    let mut out = String::new();
    for k in 0..n {
        out += &format!(">seq{k} synthetic sequence {}\n", r.below(1000));
        let len = 500 + r.below(4000) as usize;
        let mut s: Vec<u8> = (0..len).map(|_| b"ACGT"[r.below(4) as usize]).collect();
        let at = r.below(len as u64) as usize;
        s[at..(at + 50).min(len)].fill(b'N');
        let at = r.below(len as u64) as usize;
        s[at..(at + 80).min(len)].make_ascii_lowercase();
        for l in s.chunks(60) {
            out += std::str::from_utf8(l).unwrap();
            out.push('\n');
        }
    }
    out.into_bytes()
}

/// A Wavefront OBJ mesh: a grid's vertices, texture coordinates, normals and faces.
fn obj(seed: u64, n: usize) -> Vec<u8> {
    let mut r = Rng::new(seed);
    let mut out = String::from("# synthetic\no grid\n");
    for y in 0..n {
        for x in 0..n {
            let z = r.below(1000) as f64 / 1000.0;
            out += &format!("v {:.6} {:.6} {z:.6}\n", x as f64 / n as f64, y as f64 / n as f64);
            out += &format!("vt {:.4} {:.4}\n", x as f64 / n as f64, y as f64 / n as f64);
            out += "vn 0.0000 0.0000 1.0000\n";
        }
    }
    for y in 0..n - 1 {
        for x in 0..n - 1 {
            let (a, b, c, d) = (y * n + x + 1, y * n + x + 2, (y + 1) * n + x + 1, (y + 1) * n + x + 2);
            out += &format!("f {a}/{a}/{a} {b}/{b}/{b} {d}/{d}/{d}\nf {a}/{a}/{a} {d}/{d}/{d} {c}/{c}/{c}\n");
        }
    }
    out.into_bytes()
}

/// Bytes of a pattern, as images and binaries have them: gradients, runs and noise.
fn pattern(seed: u64, n: usize) -> Vec<u8> {
    let mut r = Rng::new(seed);
    (0..n)
        .map(|i| match (i / 4096) % 3 {
            0 => (i % 251) as u8,
            1 => (i / 64) as u8,
            _ => r.below(256) as u8,
        })
        .collect()
}

fn zlib(d: &[u8], level: u32) -> Vec<u8> {
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(level));
    e.write_all(d).unwrap();
    e.finish().unwrap()
}

fn gzip(d: &[u8], level: u32) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(level));
    e.write_all(d).unwrap();
    e.finish().unwrap()
}

/// A PNG of colour type `ct` (0, 2, 3, 4, 6; 8 bits), `w` by `h`, its rows of filter `filter`
/// (0 to 4), zlib at `level`.
fn png(seed: u64, w: u32, h: u32, ct: u8, filter: u8, level: u32) -> Vec<u8> {
    let mut r = Rng::new(seed);
    let channels = [1, 0, 3, 1, 2, 0, 4][ct as usize];
    let stride = w as usize * channels;
    let mut raw = Vec::with_capacity((stride + 1) * h as usize);
    let mut prev = vec![0u8; stride];
    for y in 0..h as usize {
        let row: Vec<u8> = (0..stride)
            .map(|i| match ct {
                3 => ((i / 3 + y / 5) % 16) as u8,
                _ => ((i * 3 + y * 2) % 256) as u8 ^ (r.below(8) as u8),
            })
            .collect();
        raw.push(filter);
        for i in 0..stride {
            let a = if i >= channels { row[i - channels] } else { 0 };
            let b = prev[i];
            let c = if i >= channels { prev[i - channels] } else { 0 };
            let p = match filter {
                1 => a,
                2 => b,
                3 => ((a as u16 + b as u16) / 2) as u8,
                4 => {
                    let (pa, pb, pc) = (
                        (b as i16 - c as i16).abs(),
                        (a as i16 - c as i16).abs(),
                        (a as i16 + b as i16 - 2 * c as i16).abs(),
                    );
                    if pa <= pb && pa <= pc {
                        a
                    } else if pb <= pc {
                        b
                    } else {
                        c
                    }
                }
                _ => 0,
            };
            raw.push(row[i].wrapping_sub(p));
        }
        prev = row;
    }
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |ty: &[u8], d: &[u8]| {
        out.extend_from_slice(&(d.len() as u32).to_be_bytes());
        let body = [ty, d].concat();
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, ct, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    if ct == 3 {
        let plte: Vec<u8> = (0..16u8).flat_map(|i| [i * 16, 255 - i * 16, i * 8]).collect();
        chunk(b"PLTE", &plte);
    }
    chunk(b"IDAT", &zlib(&raw, level));
    chunk(b"IEND", &[]);
    out
}

fn crc32(d: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in d {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}

/// A safetensors file of a few tensors of each float type.
fn safetensors(seed: u64) -> Vec<u8> {
    let mut r = Rng::new(seed);
    let (mut header, mut data) = (Vec::new(), Vec::new());
    for (k, (dtype, size)) in [("F32", 4), ("BF16", 2), ("F16", 2), ("F32", 4)].into_iter().enumerate() {
        let n = 1024 + r.below(4096) as usize;
        let start = data.len();
        for i in 0..n {
            // (weights of a narrow range: exponents of a few values)
            let v = ((i as f32 * 0.37).sin() * 0.05 + (r.below(1000) as f32 - 500.0) * 1e-5) as f32;
            match size {
                4 => data.extend_from_slice(&v.to_le_bytes()),
                _ => data.extend_from_slice(&((v.to_bits() >> 16) as u16).to_le_bytes()),
            }
        }
        header.push(format!(
            "\"layer.{k}.weight\":{{\"dtype\":\"{dtype}\",\"shape\":[{n}],\"data_offsets\":[{start},{}]}}",
            data.len()
        ));
    }
    let mut h = format!("{{\"__metadata__\":{{\"format\":\"pt\"}},{}}}", header.join(","));
    while h.len() % 8 != 0 {
        h.push(' ');
    }
    [&(h.len() as u64).to_le_bytes()[..], h.as_bytes(), &data].concat()
}

/// An asar archive (Electron's) of a few files, with their SHA-256 integrity.
fn asar(seed: u64) -> Vec<u8> {
    use sha2::Digest;
    let mut r = Rng::new(seed);
    let (mut entries, mut data) = (Vec::new(), Vec::new());
    for k in 0..4 {
        let d = match k % 2 {
            0 => prose(seed + k, 20, false),
            _ => pattern(seed + k, 2000 + r.below(8000) as usize),
        };
        let hash = |b: &[u8]| sha2::Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect::<String>();
        entries.push(format!(
            "\"f{k}.txt\":{{\"size\":{},\"offset\":\"{}\",\"integrity\":{{\"algorithm\":\"SHA256\",\"hash\":\"{}\",\"blockSize\":4194304,\"blocks\":[\"{}\"]}}}}",
            d.len(),
            data.len(),
            hash(&d),
            hash(&d)
        ));
        data.extend_from_slice(&d);
    }
    let json = format!("{{\"files\":{{\"dir\":{{\"files\":{{{}}}}}}}}}", entries.join(","));
    // (Chromium's pickles: a size, then the header's pickle of its string)
    let padded = json.len().next_multiple_of(4);
    let mut out = Vec::new();
    out.extend_from_slice(&4u32.to_le_bytes());
    out.extend_from_slice(&(padded as u32 + 8).to_le_bytes());
    out.extend_from_slice(&(padded as u32 + 4).to_le_bytes());
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(json.as_bytes());
    out.resize(16 + padded, 0);
    out.extend_from_slice(&data);
    out
}

/// A Minecraft region file of `n` chunks: each an NBT compound (a few tags, a long array of
/// block states) in zlib at level 6 (as Minecraft writes them), in sectors of 4 KiB.
fn mca(seed: u64, n: usize) -> Vec<u8> {
    let mut r = Rng::new(seed);
    let mut out = vec![0u8; 8192];
    for k in 0..n {
        // (an NBT compound: tag 10, a name, then its tags, then 0)
        let mut nbt = vec![10, 0, 0];
        let tag = |nbt: &mut Vec<u8>, ty: u8, name: &str| {
            nbt.push(ty);
            nbt.extend_from_slice(&(name.len() as u16).to_be_bytes());
            nbt.extend_from_slice(name.as_bytes());
        };
        tag(&mut nbt, 3, "DataVersion");
        nbt.extend_from_slice(&3465i32.to_be_bytes());
        tag(&mut nbt, 3, "xPos");
        nbt.extend_from_slice(&(k as i32).to_be_bytes());
        tag(&mut nbt, 8, "Status");
        nbt.extend_from_slice(&4u16.to_be_bytes());
        nbt.extend_from_slice(b"full");
        tag(&mut nbt, 12, "block_states");
        nbt.extend_from_slice(&256i32.to_be_bytes());
        for i in 0..256u64 {
            let v = if r.below(8) == 0 { r.next() } else { i / 32 * 0x1111_1111_1111_1111 };
            nbt.extend_from_slice(&v.to_be_bytes());
        }
        nbt.push(0);
        let z = zlib(&nbt, 6);
        let at = out.len();
        out.extend_from_slice(&(z.len() as u32 + 1).to_be_bytes());
        out.push(2);
        out.extend_from_slice(&z);
        out.resize(out.len().next_multiple_of(4096), 0);
        let (sector, count) = (at / 4096, (out.len() - at) / 4096);
        out[4 * k..4 * k + 4].copy_from_slice(&((sector as u32) << 8 | count as u32).to_be_bytes());
        out[4096 + 4 * k..4096 + 4 * k + 4].copy_from_slice(&(1_700_000_000u32 + k as u32).to_be_bytes());
    }
    out
}

/// What `cmd` makes in a directory of its own (`dir`), of file `out` there: none where it is not
/// installed or fails.
fn made(dir: &Path, cmd: &mut Command, out: &str) -> Option<Vec<u8>> {
    let ok = cmd.current_dir(dir).output().ok()?.status.success();
    ok.then(|| std::fs::read(dir.join(out)).ok()).flatten()
}

/// A C program of `n` functions: arithmetic, loops, branches and calls.
fn c_program(n: usize) -> String {
    let mut s = String::from("#include <stdio.h>\n");
    for k in 0..n {
        s += &format!(
            "int f{k}(int x) {{ int s = {k}; for (int i = 0; i < x; i++) {{ if (i % {} == 0) s += i * {}; else s ^= i; }} return s; }}\n",
            k % 7 + 2,
            k + 3
        );
    }
    s += "int main(int argc, char **argv) { int s = 0;\n";
    for k in 0..n {
        s += &format!("  s += f{k}(argc + {k});\n");
    }
    s += "  printf(\"%d\\n\", s); return 0; }\n";
    s
}

/// Seeds made by the tools installed (in `dir`, made empty): SQLite databases, audio and video
/// (ffmpeg, of tones and noise and a test pattern), message catalogs, executables and objects
/// of debugging information (x86-64 and ARM64), xz of branch filters.
fn by_tools(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let sql = "PRAGMA page_size=4096; CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT, n REAL, b BLOB); \
        WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x<1500) \
        INSERT INTO t SELECT x, 'name ' || (x % 97), x * 1.5, randomblob(x % 40) FROM c; \
        CREATE INDEX i ON t(name); DELETE FROM t WHERE id % 7 = 0;";
    if let Some(d) = made(dir, Command::new("sqlite3").args(["db.sqlite", sql]), "db.sqlite") {
        out.push(("gen-sqlite.sqlite".into(), d));
    }
    let tone = ["-f", "lavfi", "-i", "sine=frequency=440:duration=1.5"];
    let noise = ["-f", "lavfi", "-i", "anoisesrc=d=1:c=pink:a=0.3"];
    let video = ["-f", "lavfi", "-i", "testsrc=size=64x48:rate=10:duration=0.5"];
    let ff: [(&str, &[&str], &[&str]); 8] = [
        ("tone.ac3", &tone, &["-c:a", "ac3", "-b:a", "96k"]),
        ("tone.ogg", &tone, &["-c:a", "libvorbis", "-q:a", "2"]),
        ("noise.ogg", &noise, &["-c:a", "libvorbis", "-q:a", "0"]),
        ("ima.wav", &tone, &["-c:a", "adpcm_ima_wav"]),
        ("msadpcm.wav", &noise, &["-c:a", "adpcm_ms"]),
        ("tone.mkv", &tone, &["-c:a", "flac"]),
        ("vorbis.mkv", &noise, &["-c:a", "libvorbis"]),
        ("prores.mov", &video, &["-c:v", "prores_ks", "-profile:v", "0"]),
    ];
    for (name, input, codec) in ff {
        let mut c = Command::new("ffmpeg");
        c.args(["-hide_banner", "-loglevel", "error", "-y"]).args(input).args(codec).arg(name);
        if let Some(d) = made(dir, &mut c, name) {
            out.push((format!("gen-ffmpeg-{name}"), d));
        }
    }
    let po: String = (0..200)
        .map(|k| {
            format!("msgid \"message {k} of the {}\"\nmsgstr \"메시지 {k} {}\"\n\n", WORDS[k % 24], WORDS[(k * 7) % 24])
        })
        .collect();
    std::fs::write(dir.join("m.po"), po).unwrap();
    if let Some(d) = made(dir, Command::new("msgfmt").args(["-o", "m.mo", "m.po"]), "m.mo") {
        out.push(("gen-gettext.mo".into(), d));
    }
    std::fs::write(dir.join("p.c"), c_program(60)).unwrap();
    for arch in ["arm64", "x86_64"] {
        for opt in ["-O0", "-O2"] {
            let exe = format!("p-{arch}{opt}");
            if let Some(d) = made(dir, Command::new("cc").args(["-arch", arch, opt, "-o", &exe, "p.c"]), &exe) {
                out.push((format!("gen-cc-{exe}"), d.clone()));
                // (of branch filters, as xz and 7z have them)
                let filter = if arch == "x86_64" { "--x86" } else { "--arm64" };
                std::fs::write(dir.join(&exe), &d).unwrap();
                let xz = format!("{exe}.xz");
                let mut c = Command::new("xz");
                c.args(["-k", "-f", filter, "--lzma2=preset=6", &exe]);
                if let Some(x) = made(dir, &mut c, &xz) {
                    out.push((format!("gen-xz-{xz}"), x));
                }
            }
        }
        let o = format!("p-{arch}.o");
        if let Some(d) = made(dir, Command::new("cc").args(["-arch", arch, "-g", "-c", "-o", &o, "p.c"]), &o) {
            out.push((format!("gen-cc-{o}"), d));
        }
    }
    std::fs::write(dir.join("t.bin"), pattern(7, 100_000)).unwrap();
    if let Some(d) = made(dir, Command::new("xz").args(["-k", "-f", "--delta=dist=4", "--lzma2", "t.bin"]), "t.bin.xz")
    {
        out.push(("gen-xz-delta.xz".into(), d));
    }
    out
}

/// The seeds made, by name, each of at most `max` bytes.
pub fn generated(max: usize) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = vec![
        ("gen-log.log".into(), log(1, 2000)),
        ("gen-table.csv".into(), table(2, 2000, ',')),
        ("gen-table.tsv".into(), table(3, 1500, '\t')),
        ("gen-records.jsonl".into(), jsonl(4, 1500)),
        ("gen-prose.txt".into(), prose(5, 400, false)),
        ("gen-markup.xml".into(), prose(6, 300, true)),
        ("gen-seq.fasta".into(), fasta(7, 30)),
        ("gen-mesh.obj".into(), obj(8, 40)),
        ("gen-safetensors.safetensors".into(), safetensors(9)),
        ("gen-app.asar".into(), asar(10)),
        ("gen-region.mca".into(), mca(11, 8)),
    ];
    for (k, level) in [1, 6, 9].into_iter().enumerate() {
        out.push((format!("gen-log-{level}.gz"), gzip(&log(20 + k as u64, 1500), level)));
        out.push((format!("gen-pattern-{level}.gz"), gzip(&pattern(30 + k as u64, 60_000), level)));
        out.push((
            format!("gen-log-{level}.zst"),
            zstd::encode_all(&log(40 + k as u64, 1500)[..], level as i32).unwrap(),
        ));
    }
    for (k, ct) in [0u8, 2, 3, 4, 6].into_iter().enumerate() {
        for filter in 0..5u8 {
            let level = [1, 6, 9][(k + filter as usize) % 3];
            let name = format!("gen-png-c{ct}-f{filter}-l{level}.png");
            out.push((name, png(50 + k as u64, 64 + 16 * filter as u32, 48, ct, filter, level)));
        }
    }
    let wasm = Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/aqr-modules/wasm");
    let mut ws: Vec<PathBuf> =
        std::fs::read_dir(&wasm).map(|d| d.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    ws.retain(|p| p.extension().is_some_and(|e| e == "wasm"));
    ws.sort();
    for p in ws.iter().step_by(9) {
        out.push((format!("gen-module-{}", p.file_name().unwrap().to_string_lossy()), std::fs::read(p).unwrap()));
    }
    let dir = std::env::temp_dir().join(format!("aqr-fuzz-seedgen-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    out.extend(by_tools(&dir));
    let _ = std::fs::remove_dir_all(&dir);
    out.retain(|(_, d)| d.len() <= max);
    out
}
