//! Compresses files and directories into Aqueous archives, and decompresses them, as zstd does.
//!
//! usage: aqr [-c | -d | -t] [-o OUTPUT] [-f] [-p] [-q | -v] [-T THREADS] [-l LEVEL | -0 ... -9] [--depth N]
//! [--batch MIB | -B SIZE] [-b SIZE] [-D] [--no-check] [--wasm] [--no-sandbox] [--stats[=FILE]] [-h] INPUT...
//!
//! - `-c` (the default; `aqr`): each input into `INPUT.aqr`, a file as an archive of a single
//!   file, a directory as an archive of what it holds; with `-o`, all the inputs into one
//!   archive (several of them, or a directory, as entries of their names).
//! - `-d` (the default as `unaqr`): each archive into its name without `.aqr`, or `-o`. An
//!   archive of another format, by its name (ZIP, 7z, RAR, ALZ, EGG, tar alone or under gzip, bzip2,
//!   xz, LZMA or Zstandard, and those compressors' single files: [`aqr_unpack::by_name`]), is extracted
//!   as it is into the current directory, or `-o` (made where it is not), as `tar`, `unzip` and
//!   7-Zip do; a compressor's single file into its name without its extension, as gzip does.
//!   Its metadata is taken as an Aqueous archive's ([`extract`]); what it holds that is not
//!   extracted (encrypted entries, methods not known, paths reaching out) is told, and the run
//!   fails once the rest is extracted. A file there already is written over only with `-f`.
//! - `--auto-dir` (as the frontends extract): a single entry at the top of an archive (of
//!   either kind) is extracted beside it as it is, where nothing is there of its name; else
//!   the archive is extracted into a directory of its name without its extension, beside it
//!   (`foo.zip` of `foo/` into `foo/`, never `foo/foo/`). `-o` is where it goes instead of
//!   beside it, a directory (with several archives too).
//! - `-t`: each archive decoded as `-d` does, but nothing written (its checksums checked).
//! - `--wasm`: in decoding, every module run as WebAssembly, the known ones too (natively
//!   otherwise).
//! - `--no-sandbox`: in decoding, the modules not known run in this process. By default they
//!   run in worker processes locked down by the system ([`aqr_decompress::sandbox`]; every
//!   module with `--wasm`), started once the first of them is loaded, a sandbox for each
//!   archive. Where the system cannot lock workers down, they do not run but with
//!   `--no-sandbox` (the known ones with `--wasm` in this process).
//! - `-l`: the level of compression ([`aqr::Level`]), `0` (nothing compressed: uncompressed
//!   blocks only), `1` (zstd alone: nothing recompressed) to `9`; `low`, `mid` (the default)
//!   and `high` are 2, 5 and 8. `zstdN` compresses every stream at zstd level `N`, the
//!   recompression as at mid. `-N` is `-l N`, its digits all one level (`-10` is not `-1 -0`).
//! - The metadata of a directory archived is kept ([`meta`]): times, permissions, ownership,
//!   flags, extended attributes and resource forks, hard links, FIFOs and devices; and applied
//!   as it is extracted, but what is not commonly kept. `-p` keeps that too: in compressing,
//!   the status change and backup times; in decompressing, the ownership, the flags only the
//!   superuser can change, the set-user-ID and set-group-ID bits, FIFOs and devices, and the
//!   backup times. An archive of a single file has no metadata.
//! - `-f` overwrites outputs; inputs are always kept. With `--ipc`, the frontend is asked
//!   instead where an output exists ([`ipc`]), and skips it or has it overwritten. `-q` logs nothing but errors; `-v` logs
//!   each file archived too (below). An archive, or a single file decompressed, is written as
//!   `OUTPUT.PID.part` and renamed once done: removed where it fails or the process is stopped
//!   ([`stop`]), an output there before left as it was.
//! - `--batch` is about the most MiB of files made at once (512 by default), which bounds the
//!   memory taken by compression (but for a file larger).
//! - `-B` is `--batch` as a size (512M; `K`, `M` and `G` are KiB, MiB and GiB, a number alone
//!   bytes). `-b` is the most bytes of a block that files share (16M by default), which a file
//!   read alone may have to decompress: smaller for faster random access. `-D` compresses the blocks alike of a batch (many of a class of stream, of
//!   a few MiB each or less) with a dictionary trained for them where it pays.
//! - `-T` is the number of threads (0, the default: as many as the machine has): the size of
//!   the pool of threads that everything runs on ([`aqr_common::pool`]), this work itself too.
//! - `--stats` tells, after each archive made or decoded, what stands out of its files by kind
//!   and of where the time went, sampled as it ran ([`stats`]); `--stats=FILE.json` writes all
//!   of it as well, and `--stats=FILE.html` a page showing it (opened on a terminal). Either
//!   may be given more than once. The sampling slows the work a little.
//!
//! Compressing, standard error has the summary of the archive at the end, and on a terminal a
//! bar of how far it is ([`view`]) until then. With `-v`, a log above the bar tells, for each
//! file, what it was taken for, the modules its formula applies and the bytes of the archive
//! that are its; the files stored together in a block (solid) or coded against each other
//! (sets); the files of the same content as another. It is written a batch at a time, as the
//! files are. Decompressing (and testing), there is a bar too, of the blocks decompressed and
//! the files evaluated ([`view::Decoding`]).
//!
//! `--ipc` has this program driven by another (a graphical frontend) through standard input
//! and output ([`ipc`]): what it shows as it goes told there, and the frontend asked what the
//! options leave open.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::OnceLock;
use std::time::Instant;

mod extract;
mod ipc;
mod meta;
mod stats;
mod stop;
mod view;

use aqr_compress::mapped::Mapper;
use aqr_compress::{Options, compress_formula_to, compress_tree_to, detect, kind_by_name};
use aqr_decompress::{Native, Wasm, sandbox};
use aqr_formula::Bytes;
use aqr_formula::aqr::{self, Entry, EntryNode, Failure, Tree, name_bytes, name_from};
use meta::{os_bytes, os_from};
use view::Ticking;

const USAGE: &str = "usage: aqr [OPTIONS] INPUT...";

const HELP: &str = "\
usage: aqr [OPTIONS] INPUT...
       unaqr [OPTIONS] ARCHIVE...

Modes:
  -c, --compress        each input into INPUT.aqr (the default as aqr)
  -d, --decompress      each archive into its name without .aqr (the default as unaqr);
                        .zip, .7z, .rar, .alz, .egg and .tar (.gz .tgz .bz2 .xz
                        .lzma .zst) too, as they are into the current directory
                        (or -o); .gz, .bz2, .xz, .lzma and .zst into the name
                        without them
  -t, --test            decode and check each archive, writing nothing

Output:
  -o OUTPUT             write to OUTPUT; compressing, all the inputs into one archive
      --auto-dir        decompressing, a single entry at an archive's top beside it,
                        else a directory of its name without its extension beside it
                        (-o: in that directory instead of beside it)
  -f, --force           overwrite outputs
  -p, --preserve        keep and apply the metadata not commonly kept too
  -q, --quiet           log nothing but errors
  -v, --verbose         log each file archived
      --stats[=FILE]    tell what stands out after each archive; FILE.json or FILE.html
                        writes all that was sampled (may be given more than once)
      --ipc             be driven by a frontend through standard input and output

Compression:
  -l LEVEL, -0 ... -9   0 (uncompressed), 1 (zstd alone) to 9; low, mid (default), high
                        are 2, 5 and 8; zstdN is zstd level N with recompression as mid
      --depth N         recompress formats nested at most N deep (default 3)
      --batch MIB       make about MIB MiB of files at once (default 512)
  -B SIZE               --batch as a size (512M)
  -b SIZE               the most bytes of a block that files share (default 16M)
  -D                    compress blocks alike with dictionaries trained for them
      --no-check        do not decode the archive made to check it

Decompression:
      --wasm            run every module as WebAssembly, the known ones too
      --no-sandbox      run the Wasm modules (the ones not known, or all with --wasm)
                        in this process, not in workers locked down by the system;
                        where the system cannot lock workers down, they run only so

General:
  -T THREADS            threads to use (default 0: as many as the machine has)
  -h, --help            show this help

SIZE is a number of bytes, or of KiB, MiB or GiB with K, M or G.
";

/// The argument (the only one) that this program is run with as a worker of the sandbox.
const SANDBOX_WORKER: &str = "--sandbox-worker";

struct Args {
    decompress: bool,
    /// Decompressing, but nothing written.
    test: bool,
    wasm: bool,
    /// The modules not known (all, with `wasm`) run in a sandbox.
    sandbox: bool,
    output: Option<PathBuf>,
    /// Decompressing, a single entry at the top beside the archive, else a directory of its name.
    auto_dir: bool,
    force: bool,
    /// What is not commonly kept of the metadata kept too ([`meta`]).
    preserve: bool,
    quiet: bool,
    /// Logging each file archived.
    verbose: bool,
    opt: Options,
    inputs: Vec<PathBuf>,
    /// What `--stats` asked for.
    stats: Option<stats::Stats>,
    /// Driven by a frontend ([`ipc`]).
    ipc: bool,
}

/// The bytes of the leaves decoded at once ([`aqr::Archive::set_budget`]). On 10gb (10
/// threads): 2.56 GB of peak footprint in 19.5 s, against 4.1–4.6 GB in 16.6–20.1 s without
/// (as `enwik9` decodes alone first: `enwik9.zip` and the leaves after it wait for it).
const DECODE_BUDGET: u64 = 2_000_000_000;

#[global_allocator]
static ALLOC: aqr_common::mem::Mapped = aqr_common::mem::Mapped;

fn main() -> ExitCode {
    // before anything else, as the worker's standard input and output are its messages
    if std::env::args_os().nth(1).is_some_and(|a| a == SANDBOX_WORKER) {
        sandbox::worker_main();
    }
    let args = match parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("aqr: {e}\n{USAGE}\nTry 'aqr --help' for more.");
            return ExitCode::from(2);
        }
    };
    if args.ipc
        && let Err(e) = ipc::start()
    {
        eprintln!("aqr: {e}");
        return ExitCode::FAILURE;
    }
    // (before the pool is made, for its workers to be sampled)
    if args.stats.is_some() {
        aqr_common::trace::enable();
    }
    aqr_common::pool::init(args.opt.threads);
    let r = aqr_common::pool::run(|| if args.decompress { decompress_all(&args) } else { compress_all(&args) });
    // (of the archives done, whether or not all are)
    let r = match args.stats.as_ref().map(|s| s.write(std::io::stderr().is_terminal())) {
        Some(Err(e)) => r.and(Err(e)),
        _ => r,
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("aqr: {e}");
            ipc::tell(serde_json::json!({"event": "error", "message": e}));
            ExitCode::FAILURE
        }
    }
}

fn parse() -> Result<Args, String> {
    let mut argv = std::env::args_os();
    let me = argv.next().map(PathBuf::from).unwrap_or_default();
    let unaqr = me.file_stem().is_some_and(|s| s.to_string_lossy().starts_with("unaqr"));
    let mut a = Args {
        decompress: unaqr,
        test: false,
        wasm: false,
        sandbox: true,
        output: None,
        auto_dir: false,
        force: false,
        preserve: false,
        quiet: false,
        verbose: false,
        opt: Options { threads: 0, ..Options::default() },
        inputs: Vec::new(),
        stats: None,
        ipc: false,
    };
    let (mut stats, mut stats_to) = (false, Vec::new());
    let mut only_inputs = false;
    while let Some(arg) = argv.next() {
        let s = arg.to_string_lossy();
        if only_inputs || !s.starts_with('-') || s == "-" {
            a.inputs.push(arg.into());
            continue;
        }
        let mut value = |name: &str| -> Result<String, String> {
            argv.next().map(|v| v.to_string_lossy().into_owned()).ok_or(format!("{name} takes a value"))
        };
        match &*s {
            "--" => only_inputs = true,
            "-h" | "--help" => {
                print!("{HELP}");
                std::process::exit(0);
            }
            "-c" | "--compress" => a.decompress = false,
            "-d" | "--decompress" => a.decompress = true,
            "-t" | "--test" => (a.decompress, a.test) = (true, true),
            "--wasm" => a.wasm = true,
            "--no-sandbox" => a.sandbox = false,
            "-o" => a.output = Some(value("-o")?.into()),
            "--auto-dir" => a.auto_dir = true,
            "-f" | "--force" => a.force = true,
            "-p" | "--preserve" => a.preserve = true,
            "-q" | "--quiet" => a.quiet = true,
            "-v" | "--verbose" => a.verbose = true,
            "-T" => a.opt.threads = value("-T")?.parse().map_err(|_| "-T takes a number")?,
            "-l" => a.opt.level = value("-l")?.parse()?,
            "--depth" => a.opt.depth = value("--depth")?.parse().map_err(|_| "--depth takes a number")?,
            "--batch" => {
                let mib: usize = value("--batch")?.parse().map_err(|_| "--batch takes a number of MiB")?;
                a.opt.batch = mib.max(1) << 20;
            }
            "-B" => a.opt.batch = size_of(&value("-B")?).ok_or("-B takes a size (64M, 1G, ...)")?.max(1),
            "-b" => a.opt.block = size_of(&value("-b")?).ok_or("-b takes a size (64K, 1M, ...)")?.max(1),
            "-D" => a.opt.dictionaries = true,
            "--no-check" => a.opt.verify = false,
            "--stats" => stats = true,
            "--ipc" => a.ipc = true,
            s if s.starts_with("--stats=") => {
                let to = PathBuf::from(&s["--stats=".len()..]);
                if !stats::takes(&to) {
                    return Err("--stats= takes a file named .json or .html".into());
                }
                stats_to.push(to);
            }
            // -T8 and the like
            s if s.starts_with("-T") => a.opt.threads = s[2..].parse().map_err(|_| "-T takes a number")?,
            s if s.starts_with("-l") => a.opt.level = s[2..].parse()?,
            // -0 to -9, and whatever level of more digits there may be (one level, not several)
            s if s[1..].bytes().all(|b| b.is_ascii_digit()) => a.opt.level = s[1..].parse()?,
            s => return Err(format!("unknown option {s}")),
        }
    }
    if a.inputs.is_empty() {
        return Err("no input".into());
    }
    if stats || !stats_to.is_empty() {
        a.stats = Some(stats::Stats::new(stats_to));
    }
    if a.opt.threads == 0 {
        a.opt.threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    }
    if a.decompress && a.output.is_some() && a.inputs.len() > 1 && !a.auto_dir {
        return Err("-o with several archives to decompress".into());
    }
    Ok(a)
}

/// The bytes of size `s`: a number of bytes, or of KiB, MiB or GiB (`K`, `M`, `G`).
fn size_of(s: &str) -> Option<usize> {
    let (n, shift) = match s.as_bytes().last()? {
        b'K' | b'k' => (&s[..s.len() - 1], 10),
        b'M' | b'm' => (&s[..s.len() - 1], 20),
        b'G' | b'g' => (&s[..s.len() - 1], 30),
        _ => (s, 0),
    };
    n.parse::<usize>().ok()?.checked_mul(1 << shift)
}

/// Bytes of a size, for people.
fn size(n: f64) -> String {
    match n {
        n if n < 1e3 => format!("{n:.0} B"),
        n if n < 1e6 => format!("{:.1} KB", n / 1e3),
        n if n < 1e9 => format!("{:.2} MB", n / 1e6),
        n => format!("{:.2} GB", n / 1e9),
    }
}

fn percent(a: f64, of: f64) -> String {
    if of == 0.0 { "-".into() } else { format!("{:.1}%", 100.0 * a / of) }
}

/// Whether output `path` is to be written over where it exists: with `-f`, or as the frontend
/// answers (`None`: not, the output skipped); an error where it exists and nothing tells.
fn output(a: &Args, path: &Path) -> Result<Option<bool>, String> {
    if a.force || fs::symlink_metadata(path).is_err() {
        return Ok(Some(a.force));
    }
    let q = serde_json::json!({"path": path.display().to_string()});
    match ipc::get().and_then(|i| i.ask("overwrite", q)) {
        Some(serde_json::Value::Bool(true)) => Ok(Some(true)),
        Some(_) => {
            ipc::tell(serde_json::json!({"event": "skipped", "output": path.display().to_string()}));
            Ok(None)
        }
        None => Err(format!("{} exists (-f to overwrite)", path.display())),
    }
}

/// Tells the frontend that an archive is being made or decoded.
fn begin(mode: &str, inputs: &[PathBuf], out: Option<&Path>) {
    let inputs: Vec<String> = inputs.iter().map(|i| i.display().to_string()).collect();
    let output = out.map(|o| o.display().to_string());
    ipc::tell(serde_json::json!({"event": "begin", "mode": mode, "inputs": inputs, "output": output}));
}

/// Tells the frontend that an archive is done.
fn end(out: Option<&Path>, files: usize, read: u64, written: u64, start: Instant) {
    ipc::tell(serde_json::json!({
        "event": "end",
        "output": out.map(|o| o.display().to_string()),
        "files": files,
        "read": read,
        "written": written,
        "seconds": start.elapsed().as_secs_f64(),
    }));
}

/// `path` with `ext` appended to its name.
fn with_ext(path: &Path, ext: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(ext);
    s.into()
}

fn compress_all(a: &Args) -> Result<(), String> {
    match &a.output {
        Some(out) => compress_into(a, &a.inputs, out),
        None => {
            for input in &a.inputs {
                if input.as_os_str() == "-" {
                    return Err("standard input is not taken yet".into());
                }
                compress_into(a, std::slice::from_ref(input), &with_ext(input, ".aqr"))?;
            }
            Ok(())
        }
    }
}

/// The entries and contents being gathered for an archive.
struct Gathered {
    tree: Tree,
    /// By content: the path (read as the archive is made), as bytes (the name telling its
    /// kind) and its size as gathered.
    contents: Vec<(PathBuf, Vec<u8>, u64)>,
    skipped: Vec<String>,
    /// What could not be read of the metadata.
    warnings: Vec<String>,
    /// What the files are read by, mapped into memory.
    mapper: Mapper,
    reader: meta::Reader,
    /// By inode (device and number) of more links than one, the files (entries) of it.
    inodes: std::collections::HashMap<(u64, u64), Vec<usize>>,
}

impl Gathered {
    fn new(mapper: Mapper, preserve: bool) -> Gathered {
        Gathered {
            tree: Tree::default(),
            contents: Vec::new(),
            skipped: Vec::new(),
            warnings: Vec::new(),
            mapper,
            reader: meta::Reader::new(preserve),
            inodes: Default::default(),
        }
    }

    /// The link groups of the files of an inode, where there are more than one of it: numbered
    /// from 0, in the order their inodes were first found.
    fn link_groups(&mut self) {
        let mut groups: Vec<&Vec<usize>> = self.inodes.values().filter(|f| f.len() > 1).collect();
        groups.sort_unstable_by_key(|f| f[0]);
        for (g, files) in groups.into_iter().enumerate() {
            files.iter().for_each(|&i| self.tree.entries[i].extra.link_group = Some(g as i128));
        }
    }
}

/// The files of `Gathered`, read as their batch is made.
impl aqr_compress::Source for Gathered {
    fn len(&self) -> usize {
        self.contents.len()
    }

    fn name(&self, c: usize) -> &[u8] {
        &self.contents[c].1
    }

    fn size(&self, c: usize) -> u64 {
        self.contents[c].2
    }

    fn read(&self, c: usize) -> std::io::Result<Bytes> {
        let p = &self.contents[c].0;
        self.mapper.read(p).map_err(|e| std::io::Error::new(e.kind(), format!("{}: {e}", p.display())))
    }

    fn read_range(&self, c: usize, range: std::ops::Range<u64>) -> std::io::Result<Vec<u8>> {
        use std::io::{Read, Seek};
        let p = &self.contents[c].0;
        let at = |e: std::io::Error| std::io::Error::new(e.kind(), format!("{}: {e}", p.display()));
        let mut f = fs::File::open(p).map_err(at)?;
        f.seek(std::io::SeekFrom::Start(range.start)).map_err(at)?;
        let mut d = Vec::with_capacity(range.end.min(self.contents[c].2).saturating_sub(range.start) as usize);
        f.take(range.end.saturating_sub(range.start)).read_to_end(&mut d).map_err(at)?;
        Ok(d)
    }

    fn intact(&self, c: usize) -> std::io::Result<()> {
        let p = &self.contents[c].0;
        match self.mapper.cut(p) {
            true => Err(std::io::Error::other(format!("{}: cut short while archived", p.display()))),
            false => Ok(()),
        }
    }
}

fn compress_into(a: &Args, inputs: &[PathBuf], out: &Path) -> Result<(), String> {
    let Some(force) = output(a, out)? else { return Ok(()) };
    begin("compress", inputs, Some(out));
    let start = Instant::now();
    let rec = stats::record(a.stats.as_ref());
    // large buffers kept in files beside the output (gone once dropped), not on the heap; in the
    // temporary directory where the output is on a drive of the network: too slow for them, and
    // Windows there neither holds a file open against deletion nor deletes it on close
    let mut opt = a.opt.clone();
    let name = out.file_name().ok_or(format!("{}: not a file's name", out.display()))?;
    let dir = out.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let mut hidden = std::ffi::OsString::from(".");
    hidden.push(name);
    opt.spill = Some(if remote(dir) { std::env::temp_dir() } else { dir.to_owned() }.join(hidden));
    let opt = &opt;
    let meta = |p: &Path| fs::symlink_metadata(p).map_err(|e| format!("{}: {e}", p.display()));
    let mapper = Mapper::new().map_err(|e| format!("files not to be mapped: {e}"))?;
    // a single file: an archive of the file alone
    if let [input] = inputs
        && meta(input)?.is_file()
    {
        let d = mapper.read(input).map_err(|e| format!("{}: {e}", input.display()))?;
        let name = os_bytes(input.as_os_str());
        let kind = kind_by_name(&name);
        let mut g = Gathered::new(mapper, a.preserve);
        g.contents.push((input.clone(), name, d.len() as u64));
        let view = view::View::new(&g, bars(), false);
        // (of the kind its name tells, as a file in an archive of many; written as it is made)
        let (n, f) = write_whole(out, force, |file| {
            let r = viewed(a, &view, |tell| compress_formula_to(&d, kind, opt, file, tell))?;
            aqr_compress::Source::intact(&g, 0)?;
            Ok(r)
        })?;
        if !a.quiet && a.verbose {
            let modules: Vec<String> = f.applies().iter().map(|m| m.module.to_string()).collect();
            let format = detect(&d).map_or("-", |f| f.name());
            eprintln!(
                "{}: {}, {}, {} -> {} ({})",
                input.display(),
                format,
                if modules.is_empty() { "kept as it is".into() } else { modules.join(" ") },
                size(d.len() as f64),
                size(n as f64),
                percent(n as f64, d.len() as f64)
            );
        }
        if !a.quiet {
            summary(out, 1, d.len(), n, start);
        }
        end(Some(out), 1, d.len() as u64, n as u64, start);
        if let (Some(s), Some(rec)) = (&a.stats, rec) {
            let path = input.display().to_string();
            s.add(stats::Run {
                mode: "compress",
                input: path.clone(),
                output: out.display().to_string(),
                seconds: start.elapsed().as_secs_f64(),
                threads: a.opt.threads,
                read: d.len() as u64,
                written: n as u64,
                overhead: None,
                contents: vec![stats::Content {
                    path,
                    kind: detect(&d).map_or("-", |f| f.name()),
                    len: d.len() as u64,
                    archived: Some(n as f64),
                    same_as: None,
                    modules: f.applies().iter().map(|m| m.module.to_string()).collect(),
                }],
                trace: rec.finish(),
            });
        }
        return Ok(());
    }

    let mut g = Gathered::new(mapper, a.preserve);
    let gathering = aqr_common::trace::span("gather");
    match inputs {
        // a directory alone: its entries at the root
        [dir] => {
            let m = meta(dir)?;
            let (x, forks) = g.reader.extra(dir, &m, &mut |w| g.warnings.push(w));
            g.tree.root = x;
            for f in forks {
                g.tree.root.forks.push((f.name, g.contents.len()));
                g.contents.push((f.path.clone(), os_bytes(f.path.as_os_str()), f.size));
            }
            gather_dir(dir, None, &mut g)?;
        }
        _ => {
            for input in inputs {
                let name = input.file_name().ok_or(format!("{}: no name to archive it by", input.display()))?;
                gather(input, name, None, &mut g)?;
            }
        }
    }
    if g.tree.entries.is_empty() {
        return Err(format!("nothing to archive in {}", inputs[0].display()));
    }
    g.link_groups();
    drop(gathering);
    if !a.quiet {
        for s in &g.skipped {
            eprintln!("skipped {s}");
        }
        g.warnings.iter().for_each(|w| eprintln!("{w}"));
    }
    // (with -v, the log as the files are written)
    let view = view::View::new(&g, bars(), a.verbose);
    // written as it is made
    let r = write_whole(out, force, |file| {
        let mut w = std::io::BufWriter::with_capacity(8 << 20, file);
        viewed(a, &view, |tell| compress_tree_to(&g.tree, &g, opt, &mut w, tell).and_then(|r| w.flush().map(|_| r)))
    })?;
    // (the files, as decompressing counts them: not their forks, each hard link)
    let files = g.tree.entries.iter().filter(|e| matches!(e.node, EntryNode::File(_))).count();
    if !a.quiet {
        eprintln!("archive overhead (header, modules, metadata): {}", size(r.overhead as f64));
        summary(out, files, r.read as usize, r.len, start);
    }
    end(Some(out), files, r.read, r.len as u64, start);
    if let (Some(s), Some(rec)) = (&a.stats, rec) {
        let contents = (r.files.iter().zip(&g.contents))
            .map(|(f, (p, ..))| stats::Content {
                path: p.display().to_string(),
                kind: f.format.unwrap_or("-"),
                len: f.len,
                archived: Some(f.archived),
                same_as: f.same_as,
                modules: f.modules.iter().map(|m| m.to_string()).collect(),
            })
            .collect();
        s.add(stats::Run {
            mode: "compress",
            input: inputs.iter().map(|i| i.display().to_string()).collect::<Vec<_>>().join(" "),
            output: out.display().to_string(),
            seconds: start.elapsed().as_secs_f64(),
            threads: a.opt.threads,
            read: r.read,
            written: r.len as u64,
            overhead: Some(r.overhead as u64),
            contents,
            trace: rec.finish(),
        });
    }
    Ok(())
}

/// Whether `dir` is on a drive of the network (false where that is not known).
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn remote(dir: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else { return false };
    let mut s = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: a path of a NUL, and room for the result
    if unsafe { libc::statfs(c.as_ptr(), s.as_mut_ptr()) } != 0 {
        return false;
    }
    // SAFETY: filled by statfs
    let s = unsafe { s.assume_init() };
    #[cfg(target_os = "macos")]
    return s.f_flags & libc::MNT_LOCAL as u32 == 0;
    // NFS, SMB, CIFS, SMB2, AFS, Coda, Ceph and 9P (FUSE left out: as often local)
    #[cfg(target_os = "linux")]
    return matches!(
        s.f_type as u32,
        0x6969 | 0x517b | 0xff53_4d42 | 0xfe53_4d42 | 0x5346_414f | 0x7375_7245 | 0x00c3_6400 | 0x0102_1997
    );
}

#[cfg(windows)]
fn remote(dir: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetVolumePathNameW};
    /// `DRIVE_REMOTE` (of WindowsProgramming, a feature not taken for it alone).
    const DRIVE_REMOTE: u32 = 4;
    let Ok(dir) = std::path::absolute(dir) else { return false };
    let w: Vec<u16> = dir.as_os_str().encode_wide().chain([0]).collect();
    let mut root = vec![0u16; w.len() + 1];
    // SAFETY: a path of a NUL, and room for its root (no longer than the path and a backslash)
    if unsafe { GetVolumePathNameW(w.as_ptr(), root.as_mut_ptr(), root.len() as u32) } == 0 {
        return false;
    }
    // SAFETY: the root of a NUL, as GetVolumePathNameW wrote it
    unsafe { GetDriveTypeW(root.as_ptr()) == DRIVE_REMOTE }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn remote(_: &Path) -> bool {
    false
}

/// Whether the bar is drawn: on a terminal that takes it (a frontend shows how far it is
/// otherwise, [`ipc`]).
fn bars() -> bool {
    std::io::stderr().is_terminal() && view::takes_escapes()
}

/// `work` done telling `view` how far it is (nothing, with -q), its bar drawn as [`ticking`].
fn viewed<T>(a: &Args, view: &view::View, work: impl FnOnce(&(dyn Fn(aqr_compress::Event) + Sync)) -> T) -> T {
    ticking(a, view, || {
        let quiet = |_: aqr_compress::Event| {};
        let tell: &(dyn Fn(aqr_compress::Event) + Sync) = if a.quiet { &quiet } else { &|e| view.tell(e) };
        work(tell)
    })
}

/// `work` done with `view`'s bar drawn again now and then (for the time a file takes) until it
/// is done, and once more then (none, with -q).
fn ticking<T>(a: &Args, view: &dyn view::Ticking, work: impl FnOnce() -> T) -> T {
    let done = std::sync::atomic::AtomicBool::new(false);
    let bars = !a.quiet && view.shows();
    std::thread::scope(|s| {
        if bars {
            s.spawn(|| {
                while !done.load(std::sync::atomic::Ordering::Relaxed) {
                    std::thread::park_timeout(std::time::Duration::from_millis(250));
                    view.tick();
                }
            });
        }
        let r = work();
        done.store(true, std::sync::atomic::Ordering::Relaxed);
        if bars {
            view.tick();
        }
        r
    })
}

fn summary(out: &Path, files: usize, total: usize, len: usize, start: Instant) {
    let t = start.elapsed().as_secs_f64();
    eprintln!(
        "{}: {files} file{}, {} -> {} ({}) in {t:.2} s, {:.1} MB/s",
        out.display(),
        if files == 1 { "" } else { "s" },
        size(total as f64),
        size(len as f64),
        percent(len as f64, total as f64),
        total as f64 / 1e6 / t.max(1e-9)
    );
}

/// Gathers `path` as an entry `name` in `parent` (`None`: the root). The files of an inode
/// archived already are of its content (and forks), read once.
fn gather(path: &Path, name: &OsStr, parent: Option<usize>, g: &mut Gathered) -> Result<(), String> {
    let m = fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let name = name_from(&os_bytes(name));
    let (mut extra, forks) = g.reader.extra(path, &m, &mut |w| g.warnings.push(w));
    let t = m.file_type();
    let inode = if t.is_file() { meta::inode(path, &m) } else { None };
    let first = inode.and_then(|i| g.inodes.get(&i)).map(|f| f[0]);
    if let Some(first) = first {
        extra.forks = g.tree.entries[first].extra.forks.clone();
    } else {
        for f in forks {
            extra.forks.push((f.name, g.contents.len()));
            g.contents.push((f.path.clone(), os_bytes(f.path.as_os_str()), f.size));
        }
    }
    let node = if t.is_dir() {
        EntryNode::Dir
    } else if let Some(first) = first {
        g.tree.entries[first].node.clone()
    } else if t.is_file() {
        g.contents.push((path.to_owned(), os_bytes(path.as_os_str()), m.len()));
        EntryNode::File(g.contents.len() - 1)
    } else if let Some(node) = meta::special(&m) {
        node
    } else if t.is_symlink() {
        let target = meta::link_target(fs::read_link(path).map_err(|e| format!("{}: {e}", path.display()))?);
        let target = name_from(&os_bytes(target.as_os_str()));
        if name_bytes(&target).0.is_empty() {
            g.skipped.push(format!("{} (an empty link)", path.display()));
            return Ok(());
        }
        EntryNode::Link(target)
    } else {
        g.skipped.push(format!("{} (a socket, or of a kind not known)", path.display()));
        return Ok(());
    };
    if let Some(i) = inode {
        g.inodes.entry(i).or_default().push(g.tree.entries.len());
    }
    g.tree.entries.push(Entry { parent, name, node: node.clone(), extra });
    if node == EntryNode::Dir {
        gather_dir(path, Some(g.tree.entries.len() - 1), g)?;
    }
    Ok(())
}

/// Gathers what directory `dir` holds into `parent`, by name.
fn gather_dir(dir: &Path, parent: Option<usize>, g: &mut Gathered) -> Result<(), String> {
    let mut names: Vec<OsString> = fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .map(|e| e.map(|e| e.file_name()))
        .collect::<Result<_, _>>()
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    names.sort();
    for name in names {
        gather(&dir.join(&name), &name, parent, g)?;
    }
    Ok(())
}

/// Writes a new file `path` (or one in place of it, where `force`) by `write`: under a name of
/// its own beside it, renamed once it is written (so that `path` is never written in part),
/// removed where it is not (`write` failing, or the process stopped: [`stop`]). The file is
/// opened to be read as well (an archive read back to be verified, [`aqr_compress::Sink`]).
fn write_whole<T>(
    path: &Path,
    force: bool,
    write: impl FnOnce(&mut fs::File) -> std::io::Result<T>,
) -> Result<T, String> {
    let err = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    let mut name = path.file_name().ok_or(format!("{}: not a file's name", path.display()))?.to_owned();
    name.push(format!(".{}.part", std::process::id()));
    let part = path.with_file_name(name);
    let mut file =
        fs::OpenOptions::new().read(true).write(true).create_new(true).open(&part).map_err(|e| err(&part, e))?;
    let stop = stop::removing(&part);
    let r = write(&mut file).map_err(|e| err(path, e)).and_then(|t| {
        drop(file);
        place(&part, path, force)?;
        Ok(t)
    });
    if r.is_err() {
        let _ = fs::remove_file(&part);
    }
    drop(stop);
    r
}

/// Moves `from` to `to`, over a file there only where `force`.
fn place(from: &Path, to: &Path, force: bool) -> Result<(), String> {
    let err = |e: std::io::Error| format!("{}: {e}", to.display());
    if force {
        return match fs::rename(from, to) {
            // replacing a file denied though neither file is held (seen on Windows over a file
            // written just before, about 1 in 30 times, for seconds): the old one moved aside
            Err(e) if cfg!(windows) && e.kind() == std::io::ErrorKind::PermissionDenied => {
                let aside = with_ext(to, &format!(".{}.old", std::process::id()));
                fs::rename(to, &aside).map_err(|_| err(e))?;
                match fs::rename(from, to) {
                    Ok(()) => {
                        _ = fs::remove_file(&aside);
                        Ok(())
                    }
                    Err(e) => {
                        _ = fs::rename(&aside, to);
                        Err(err(e))
                    }
                }
            }
            r => r.map_err(err),
        };
    }
    // (a link failing where `to` is, which a rename would replace)
    match fs::hard_link(from, to) {
        Ok(()) => fs::remove_file(from).map_err(err),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(format!("{} exists (-f to overwrite)", to.display()))
        }
        // (no links on the file system: whether it is there checked just before)
        Err(_) if fs::symlink_metadata(to).is_err() => fs::rename(from, to).map_err(err),
        Err(e) => Err(err(e)),
    }
}

/// A new file `path` (or one in place of it, where `force`).
fn create(path: &Path, force: bool) -> Result<fs::File, String> {
    let err = |e: std::io::Error| format!("{}: {e}", path.display());
    if force && fs::symlink_metadata(path).is_ok_and(|m| !m.is_dir()) {
        fs::remove_file(path).map_err(err)?;
    }
    fs::OpenOptions::new().write(true).create_new(true).open(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => format!("{} exists (-f to overwrite)", path.display()),
        _ => err(e),
    })
}

fn decompress_all(a: &Args) -> Result<(), String> {
    for input in &a.inputs {
        // an archive of another format by its name (`.zip`, `.tar.gz`, ...), else an Aqueous one
        let name = input.file_name().map(os_bytes).unwrap_or_default();
        let foreign = aqr_unpack::by_name(&name);
        // its name without its extension, beside it
        let named = match foreign {
            Some((_, n)) => Some(input.with_file_name(os_from(&name[..name.len() - n]))),
            None => input.extension().is_some_and(|e| e == "aqr").then(|| input.with_extension("")),
        };
        let to = match (named, &a.output, a.auto_dir) {
            (Some(n), Some(o), true) => To { named: o.join(n.file_name().unwrap()), base: o.clone() },
            (Some(n), None, true) => To { base: n.parent().unwrap_or(Path::new("")).to_owned(), named: n },
            (_, Some(o), false) => To { named: o.clone(), base: o.clone() },
            (Some(n), None, false) => To { named: n, base: PathBuf::new() },
            (None, ..) => {
                return Err(format!(
                    "{}: not named .aqr nor as an archive known (-o to name the output)",
                    input.display()
                ));
            }
        };
        match foreign {
            Some((f, _)) => decompress_foreign(a, input, &to, f)?,
            None => decompress_into(a, input, &to)?,
        }
    }
    Ok(())
}

/// Where an archive is decoded: `named`, its name without its extension beside it (or in
/// `-o`, with `--auto-dir`), or `-o`; `base`, the directory that it is extracted into as it is
/// (`-o`, or the current directory where empty), or (with `--auto-dir`) that its single entry
/// at the top goes into.
struct To {
    named: PathBuf,
    base: PathBuf,
}

impl To {
    /// Where `a` has a tree extracted (`foreign`: of another format than Aqueous), and whether
    /// what is there is written over (`None`: the output skipped).
    fn place<'p>(
        &'p self,
        a: &Args,
        foreign: bool,
        ask: &'p extract::Ask<'p>,
    ) -> Result<Option<(extract::Place<'p>, bool)>, String> {
        Ok(Some(match (a.auto_dir, foreign) {
            (true, _) => (extract::Place::Auto { base: &self.base, own: &self.named, ask }, a.force),
            (false, true) => (extract::Place::Into(&self.base), a.force),
            (false, false) => match output(a, &self.named)? {
                Some(f) => (extract::Place::Own(&self.named), f),
                None => return Ok(None),
            },
        }))
    }
}

/// What a frontend is told of the output of place `p` as it begins, where that is known yet.
fn shown<'p>(p: &extract::Place<'p>) -> Option<&'p Path> {
    match *p {
        extract::Place::Own(o) => Some(o),
        extract::Place::Into(d) if d.as_os_str().is_empty() => Some(Path::new(".")),
        extract::Place::Into(d) => Some(d),
        extract::Place::Auto { .. } => None,
    }
}

/// What an archive of another format is extracted to: [`extract::Extractor`], or nothing but
/// counts where it is tested; its notes told on standard error (but with -q), and how far it is
/// to the bar.
struct Unpacking<'x, 'a> {
    x: Option<&'x extract::Extractor<'a>>,
    input: &'x Path,
    quiet: bool,
    view: Option<&'x view::Decoding>,
    seen: &'x stats::Seen,
    /// Tested: by content, its bytes.
    sizes: std::sync::Mutex<Vec<u64>>,
}

impl aqr_unpack::Sink for Unpacking<'_, '_> {
    fn entries(&self, t: &Tree, new: &[usize]) -> Result<(), String> {
        self.x.map_or(Ok(()), |x| x.entries(t, new))
    }

    fn content(&self, c: usize, d: Vec<u8>) -> Result<(), String> {
        match self.x {
            Some(x) => x.content(c, d),
            None => {
                self.seen.saw(c, &d, d.len() as u64);
                let mut s = self.sizes.lock().unwrap();
                if s.len() <= c {
                    s.resize(c + 1, 0);
                }
                s[c] = d.len() as u64;
                Ok(())
            }
        }
    }

    fn note(&self, what: &str) {
        if !self.quiet {
            eprintln!("{}: {what}", self.input.display());
        }
    }

    fn step(&self, s: aqr::Step) {
        if let Some(v) = self.view {
            v.tell(s);
        }
    }
}

/// Extracts (or tests) `input`, an archive of format `format`, to `to`: as an Aqueous archive
/// is ([`extract::Extractor`]); an error at the end where entries were left out.
fn decompress_foreign(a: &Args, input: &Path, to: &To, format: aqr_unpack::Format) -> Result<(), String> {
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", input.display());
    let file = fs::File::open(input).map_err(|e| err(&e))?;
    // SAFETY: the archive is not to change while it is read; where it does, its checksums tell
    let c = unsafe { memmap2::Mmap::map(&file) }.map_err(|e| err(&e))?;
    let opened = aqr_unpack::Opened::open(format, &c).map_err(|e| err(&e))?;
    let ask = |p: &Path| output(a, p);
    let (place, force) = match (a.test, opened.is_tree()) {
        (true, _) => (None, false),
        (false, true) => match to.place(a, true, &ask)? {
            Some((p, f)) => (Some(p), f),
            None => return Ok(()),
        },
        (false, false) => match output(a, &to.named)? {
            Some(f) => (None, f),
            None => return Ok(()),
        },
    };
    let told = match &place {
        Some(p) => shown(p),
        None => (!a.test).then_some(&*to.named),
    };
    begin(if a.test { "test" } else { "decompress" }, &[input.to_owned()], told);
    let start = Instant::now();
    let rec = stats::record(a.stats.as_ref());
    let names = if !a.quiet && (bars() || a.ipc) { opened.units() } else { Vec::new() };
    let view = view::Decoding::new(bars(), names);
    let shown = (!a.quiet && view.shows()).then_some(&view);
    let seen = stats::Seen::new(0, rec.is_some());
    let threads = a.opt.threads;
    let (files, total, tree, skipped, out) = if !opened.is_tree() {
        let step = |s: aqr::Step| shown.into_iter().for_each(|v| v.tell(s));
        let n = if a.test {
            ticking(a, &view, || {
                opened.decompress(
                    threads,
                    &mut |d| {
                        seen.saw(0, d, 0);
                        Ok(())
                    },
                    &step,
                )
            })
            .map_err(|e| err(&e))?
        } else {
            write_whole(&to.named, force, |w| {
                ticking(a, &view, || {
                    opened.decompress(
                        threads,
                        &mut |d| {
                            seen.saw(0, d, 0);
                            let _s = aqr_common::trace::span("write");
                            w.write_all(d).map_err(|e| e.to_string())
                        },
                        &step,
                    )
                })
                .map_err(std::io::Error::other)
            })?
        };
        (1, n, None, 0, (!a.test).then(|| to.named.clone()))
    } else {
        let ap = meta::Applier { privileged: a.preserve, quiet: a.quiet };
        let r = ticking(a, &view, || -> Result<_, String> {
            let x = match place {
                None => None,
                Some(p) => Some(extract::Extractor::new(p, force, &ap, &seen)?),
            };
            let sink =
                Unpacking { x: x.as_ref(), input, quiet: a.quiet, view: shown, seen: &seen, sizes: Default::default() };
            let done = match opened.unpack(threads, &sink) {
                Err(_) if x.as_ref().is_some_and(|x| x.skipped()) => return Ok(None),
                r => r.map_err(|e| err(&e))?,
            };
            let sizes = sink.sizes.into_inner().unwrap();
            let (n, total, out) = match x {
                Some(x) => match x.finish(&done.tree)? {
                    Some((n, t, o)) => (n, t, Some(o)),
                    None => return Ok(None),
                },
                None => done.tree.entries.iter().fold((0, 0, None), |(n, t, o), e| match e.node {
                    EntryNode::File(c) => (n + 1, t + sizes.get(c).copied().unwrap_or(0), o),
                    _ => (n, t, o),
                }),
            };
            Ok(Some((n, total, Some(done.tree), done.skipped, out)))
        })?;
        match r {
            Some(r) => r,
            None => return Ok(()),
        }
    };
    let out = out.as_deref();
    if !a.quiet {
        summary_d(input, out, files, total, start);
    }
    end(out, files, c.len() as u64, total, start);
    if let (Some(s), Some(rec)) = (&a.stats, rec) {
        let paths = match &tree {
            Some(t) => tree_content_paths(t),
            None => vec![to.named.display().to_string()],
        };
        let mut contents = seen.contents(paths);
        if tree.is_none()
            && let Some(c) = contents.first_mut()
        {
            c.len = total;
        }
        s.add(stats::Run {
            mode: if a.test { "test" } else { "decompress" },
            input: input.display().to_string(),
            output: out.map_or(String::new(), |o| o.display().to_string()),
            seconds: start.elapsed().as_secs_f64(),
            threads,
            read: c.len() as u64,
            written: total,
            overhead: None,
            contents,
            trace: rec.finish(),
        });
    }
    if skipped > 0 {
        let what = if skipped == 1 { "entry" } else { "entries" };
        return Err(format!("{}: {skipped} {what} not extracted (told above)", input.display()));
    }
    Ok(())
}

/// Decodes (or tests) `input`, an Aqueous archive, to `to`.
fn decompress_into(a: &Args, input: &Path, to: &To) -> Result<(), String> {
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", input.display());
    let file = fs::File::open(input).map_err(|e| err(&e))?;
    // (mapped, so that what is read of it is the system's to drop)
    // SAFETY: the archive is not to change while it is read; where it does, its checksums tell
    let c = unsafe { memmap2::Mmap::map(&file) }.map_err(|e| err(&e))?;
    let decoder =
        Decoder { native: Native { threads: a.opt.threads }, wasm: a.wasm, sandbox: a.sandbox.then(OnceLock::new) };
    let runner: &dyn aqr::Runner = &decoder;
    let mut archive = aqr::open(&c, runner).map_err(|e| err(&e))?;
    let ask = |p: &Path| output(a, p);
    let (place, force) = match (a.test, archive.tree().is_some()) {
        (true, _) => (None, false),
        (false, true) => match to.place(a, false, &ask)? {
            Some((p, f)) => (Some(p), f),
            None => return Ok(()),
        },
        (false, false) => match output(a, &to.named)? {
            Some(f) => (None, f),
            None => return Ok(()),
        },
    };
    let told = match &place {
        Some(p) => shown(p),
        None => (!a.test).then_some(&*to.named),
    };
    begin(if a.test { "test" } else { "decompress" }, &[input.to_owned()], told);
    let start = Instant::now();
    let rec = stats::record(a.stats.as_ref());
    let names = if !a.quiet && (bars() || a.ipc) { leaf_names(&archive, &to.named) } else { Vec::new() };
    let view = std::sync::Arc::new(view::Decoding::new(bars(), names));
    if !a.quiet && view.shows() {
        let view = view.clone();
        archive.on_progress(Some(std::sync::Arc::new(move |r| view.tell(r))));
    }
    // (of a single file, the leaves all held for the file anyway)
    if archive.tree().is_some() {
        archive.set_budget(DECODE_BUDGET);
    }
    // what is decoded of each content, for --stats (each leaf sampled as of its first content)
    let seen = stats::Seen::new(archive.contents_len().max(1), rec.is_some());
    if rec.is_some() && archive.tree().is_some() {
        let mut items = vec![None; archive.leaves_len()];
        for c in 0..archive.contents_len() {
            if let Some(l) = archive.leaf_of(c).and_then(|l| items.get_mut(l)) {
                l.get_or_insert(c);
            }
        }
        archive.set_items(items);
    }
    let (files, total, out) = match archive.tree() {
        // (written as it is evaluated: a file of parts is not held whole)
        None if a.test => {
            let n = ticking(a, &*view, || {
                archive.write_file(a.opt.threads, &mut |d: &[u8]| {
                    seen.saw(0, d, 0);
                    Ok(())
                })
            })
            .map_err(|e| err(&e))?;
            (1, n, None)
        }
        None => {
            let n = write_whole(&to.named, force, |w| {
                let mut failed = None;
                let r = ticking(a, &*view, || {
                    archive.write_file(a.opt.threads, &mut |d: &[u8]| {
                        seen.saw(0, d, 0);
                        let _s = aqr_common::trace::span("write");
                        w.write_all(d).map_err(|e| {
                            let m = e.to_string();
                            failed = Some(e);
                            Failure::Resource(m)
                        })
                    })
                });
                match (r, failed) {
                    (_, Some(e)) => Err(e),
                    (Ok(n), None) => Ok(n),
                    // (the part written so far removed)
                    (Err(e), None) => Err(std::io::Error::other(e.to_string())),
                }
            })?;
            (1, n, Some(to.named.clone()))
        }
        Some(t) if a.test => {
            let (n, total) = (std::sync::atomic::AtomicUsize::new(0), std::sync::atomic::AtomicU64::new(0));
            let files = t.entries.iter().fold(vec![0; archive.contents_len()], |mut f, e| {
                if let EntryNode::File(c) = e.node {
                    f[c] += 1;
                }
                f
            });
            ticking(a, &*view, || {
                archive.contents(a.opt.threads, |c, d| {
                    seen.saw(c, &d, d.len() as u64);
                    n.fetch_add(files[c], std::sync::atomic::Ordering::Relaxed);
                    total.fetch_add(files[c] as u64 * d.len() as u64, std::sync::atomic::Ordering::Relaxed);
                    Ok(())
                })
            })
            .map_err(|e| err(&e))?;
            (n.into_inner(), total.into_inner(), None)
        }
        Some(t) => {
            let ap = meta::Applier { privileged: a.preserve, quiet: a.quiet };
            let r = ticking(a, &*view, || {
                let x = extract::Extractor::new(place.unwrap(), force, &ap, &seen)?;
                match x.entries(t, &(0..t.entries.len()).collect::<Vec<_>>()) {
                    Err(_) if x.skipped() => return Ok(None),
                    r => r?,
                }
                archive
                    .contents(a.opt.threads, |c, d| x.content(c, d).map_err(Failure::Resource))
                    .map_err(|e| e.to_string())?;
                x.finish(t)
            })?;
            match r {
                Some((n, total, out)) => (n, total, Some(out)),
                None => return Ok(()),
            }
        }
    };
    let out = out.as_deref();
    if !a.quiet {
        summary_d(input, out, files, total, start);
    }
    end(out, files, c.len() as u64, total, start);
    if let (Some(s), Some(rec)) = (&a.stats, rec) {
        let mut contents = seen.contents(content_paths(&archive, &to.named));
        if archive.tree().is_none()
            && let Some(c) = contents.first_mut()
        {
            c.len = total;
        }
        s.add(stats::Run {
            mode: if a.test { "test" } else { "decompress" },
            input: input.display().to_string(),
            output: out.map_or(String::new(), |o| o.display().to_string()),
            seconds: start.elapsed().as_secs_f64(),
            threads: a.opt.threads,
            read: c.len() as u64,
            written: total,
            overhead: None,
            contents,
            trace: rec.finish(),
        });
    }
    Ok(())
}

/// By content of `archive` (decoded into `out`), the path of its first file in the archive (of
/// a single file, `out`).
fn content_paths(archive: &aqr::Archive, out: &Path) -> Vec<String> {
    let Some(t) = archive.tree() else { return vec![out.display().to_string()] };
    let mut paths = tree_content_paths(t);
    paths.resize(archive.contents_len(), String::new());
    paths
}

/// By content of tree `t` (as far as its files tell), the path of its first file.
fn tree_content_paths(t: &Tree) -> Vec<String> {
    let mut first: Vec<Option<String>> = Vec::new();
    let mut paths: Vec<PathBuf> = Vec::with_capacity(t.entries.len());
    for e in &t.entries {
        let name = os_from(name_bytes(&e.name).0);
        let path = e.parent.map_or_else(|| PathBuf::from(&name), |p| paths[p].join(&name));
        if let EntryNode::File(c) = e.node {
            if first.len() <= c {
                first.resize(c + 1, None);
            }
            first[c].get_or_insert_with(|| path.display().to_string());
        }
        paths.push(path);
    }
    first.into_iter().map(Option::unwrap_or_default).collect()
}

/// By leaf of `archive` (decoded into `out`), the files of it as they are shown where it takes
/// long: the first by its path in the archive, and how many more.
fn leaf_names(archive: &aqr::Archive, out: &Path) -> Vec<String> {
    let Some(t) = archive.tree() else { return vec![out.display().to_string(); archive.leaves_len()] };
    let mut files: Vec<(Option<PathBuf>, usize)> = vec![(None, 0); archive.leaves_len()];
    let mut paths: Vec<PathBuf> = Vec::with_capacity(t.entries.len());
    for e in &t.entries {
        let name = os_from(name_bytes(&e.name).0);
        let path = e.parent.map_or_else(|| PathBuf::from(&name), |p| paths[p].join(&name));
        if let EntryNode::File(c) = e.node
            && let Some(f) = archive.leaf_of(c).and_then(|l| files.get_mut(l))
        {
            f.0.get_or_insert_with(|| path.clone());
            f.1 += 1;
        }
        paths.push(path);
    }
    files
        .into_iter()
        .map(|(first, n)| match (first, n) {
            (Some(p), 1) => p.display().to_string(),
            (Some(p), n) => format!("{} and {} more files", p.display(), n - 1),
            _ => String::new(),
        })
        .collect()
}

/// The runner of an archive's modules: those known natively (as WebAssembly with `wasm`), and
/// the others in a sandbox of this archive's, made as the first of them is loaded (or in this
/// process, without a sandbox). Where the system cannot lock workers down, the known ones are
/// never sandboxed and the others fail.
struct Decoder {
    native: Native,
    wasm: bool,
    sandbox: Option<OnceLock<Result<sandbox::Sandbox, String>>>,
}

impl aqr::Runner for Decoder {
    fn load(&self, wasm: &[u8]) -> aqr::Decoded<Box<dyn aqr::Loaded + '_>> {
        let sandboxed = aqr_decompress::known(wasm).is_none() || self.wasm && sandbox::SUPPORTED;
        let Some(s) = self.sandbox.as_ref().filter(|_| sandboxed) else {
            return if self.wasm { Wasm.load(wasm) } else { self.native.load(wasm) };
        };
        let s = s.get_or_init(|| match sandbox::SUPPORTED {
            true => sandbox::Sandbox::current_exe([SANDBOX_WORKER]).map_err(|e| e.to_string()),
            false => Err("this system does not lock workers down (--no-sandbox to run the modules not known in \
                 this process)"
                .into()),
        });
        match s {
            Ok(s) => s.load(wasm),
            Err(e) => Err(Failure::Resource(format!("no sandbox: {e}"))),
        }
    }
}

/// The summary of an archive decoded into `out` (`None`: tested).
fn summary_d(input: &Path, out: Option<&Path>, files: usize, total: u64, start: Instant) {
    let t = start.elapsed().as_secs_f64();
    let to = out.map_or(" (tested)".into(), |o| format!(" -> {}", o.display()));
    eprintln!(
        "{}{to}: {files} file{}, {} in {t:.2} s, {:.1} MB/s",
        input.display(),
        if files == 1 { "" } else { "s" },
        size(total as f64),
        total as f64 / 1e6 / t.max(1e-9)
    );
}
