//! Compresses files and directories into Aqueous archives, and decompresses them, as zstd does.
//!
//! usage: aqr [-c | -d | -t] [-o OUTPUT] [-f] [-q | -v] [-T THREADS] [-l LEVEL | -0 ... -9] [--depth N] [--batch MIB]
//! [--no-check] [--wasm] [--no-sandbox] INPUT...
//!
//! - `-c` (the default; `aqr`): each input into `INPUT.aqr`, a file as an archive of a single
//!   file, a directory as an archive of what it holds; with `-o`, all the inputs into one
//!   archive (several of them, or a directory, as entries of their names).
//! - `-d` (the default as `unaqr`): each archive into its name without `.aqr`, or `-o`.
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
//! - `-f` overwrites outputs; inputs are always kept. `-q` logs nothing but errors; `-v` logs
//!   each file archived too (below). An archive, or a single file decompressed, is written as
//!   `OUTPUT.PID.part` and renamed once done: removed where it fails or the process is stopped
//!   ([`stop`]), an output there before left as it was.
//! - `--batch` is about the most MiB of files made at once (512 by default), which bounds the
//!   memory taken by compression (but for a file larger).
//! - `-T` is the number of threads (0, the default: as many as the machine has): the size of
//!   the pool of threads that everything runs on ([`aqr_common::pool`]), this work itself too.
//!
//! Compressing, standard error has the summary of the archive at the end, and on a terminal a
//! bar of how far it is ([`view`]) until then. With `-v`, a log above the bar tells, for each
//! file, what it was taken for, the modules its formula applies and the bytes of the archive
//! that are its; the files stored together in a block (solid) or coded against each other
//! (sets); the files of the same content as another. It is written a batch at a time, as the
//! files are.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::OnceLock;
use std::time::{Instant, UNIX_EPOCH};

mod stop;
mod view;

use aqr_compress::{Options, compress_formula_telling, compress_tree_to, detect, kind_by_name};
use aqr_decompress::{Native, Wasm, sandbox};
use aqr_formula::Bytes;
use aqr_formula::aqr::{self, Entry, EntryNode, Extra, Failure, Time, Tree, Value, name_bytes, name_from};

const USAGE: &str = "usage: aqr [-c | -d | -t] [-o OUTPUT] [-f] [-q | -v] [-T THREADS] [-l 0-9|low|mid|high|zstdN | -0 ... -9] [--depth N] \
     [--batch MIB] [--no-check] [--wasm] [--no-sandbox] INPUT...";

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
    force: bool,
    quiet: bool,
    /// Logging each file archived.
    verbose: bool,
    opt: Options,
    inputs: Vec<PathBuf>,
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
            eprintln!("aqr: {e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    aqr_common::pool::init(args.opt.threads);
    let r = aqr_common::pool::run(|| if args.decompress { decompress_all(&args) } else { compress_all(&args) });
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("aqr: {e}");
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
        force: false,
        quiet: false,
        verbose: false,
        opt: Options { threads: 0, ..Options::default() },
        inputs: Vec::new(),
    };
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
            "-c" | "--compress" => a.decompress = false,
            "-d" | "--decompress" => a.decompress = true,
            "-t" | "--test" => (a.decompress, a.test) = (true, true),
            "--wasm" => a.wasm = true,
            "--no-sandbox" => a.sandbox = false,
            "-o" => a.output = Some(value("-o")?.into()),
            "-f" | "--force" => a.force = true,
            "-q" | "--quiet" => a.quiet = true,
            "-v" | "--verbose" => a.verbose = true,
            "-T" => a.opt.threads = value("-T")?.parse().map_err(|_| "-T takes a number")?,
            "-l" => a.opt.level = value("-l")?.parse()?,
            "--depth" => a.opt.depth = value("--depth")?.parse().map_err(|_| "--depth takes a number")?,
            "--batch" => {
                let mib: usize = value("--batch")?.parse().map_err(|_| "--batch takes a number of MiB")?;
                a.opt.batch = mib.max(1) << 20;
            }
            "--no-check" => a.opt.verify = false,
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
    if a.opt.threads == 0 {
        a.opt.threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    }
    if a.decompress && a.output.is_some() && a.inputs.len() > 1 {
        return Err("-o with several archives to decompress".into());
    }
    Ok(a)
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

/// Where `path`'s output goes, and whether it exists already.
fn output(path: &Path, force: bool) -> Result<(), String> {
    if !force && fs::symlink_metadata(path).is_ok() {
        return Err(format!("{} exists (-f to overwrite)", path.display()));
    }
    Ok(())
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
        read_file(p).map_err(|e| std::io::Error::new(e.kind(), format!("{}: {e}", p.display())))
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
}

/// Files of at least this many bytes are mapped into memory rather than read ([`read_file`]):
/// of fewer, a copy costs less than a mapping.
const MAP_MIN: u64 = 64 << 10;

/// The bytes of file `p`: mapped into memory (where it is of at least [`MAP_MIN`] bytes), so
/// that what is read of it is the system's to drop and read again rather than the heap's,
/// and let go of when the bytes are dropped.
fn read_file(p: &Path) -> std::io::Result<Bytes> {
    use std::io::Read;
    let mut f = fs::File::open(p)?;
    let len = f.metadata()?.len();
    if len < MAP_MIN {
        let mut d = Vec::with_capacity(len as usize);
        f.read_to_end(&mut d)?;
        return Ok(d.into());
    }
    // SAFETY: the file is not to change while it is archived; where it does, the archive's
    // verification (or a reread, [`aqr_compress::Source`]) tells, but for a file cut short,
    // which a read past its end then stops
    let m = unsafe { memmap2::Mmap::map(&f) }?;
    Ok(Bytes::shared(std::sync::Arc::new(m)))
}

fn compress_into(a: &Args, inputs: &[PathBuf], out: &Path) -> Result<(), String> {
    output(out, a.force)?;
    let start = Instant::now();
    let meta = |p: &Path| fs::symlink_metadata(p).map_err(|e| format!("{}: {e}", p.display()));
    // a single file: an archive of the file alone
    if let [input] = inputs
        && meta(input)?.is_file()
    {
        let d = read_file(input).map_err(|e| format!("{}: {e}", input.display()))?;
        let name = os_bytes(input.as_os_str());
        let kind = kind_by_name(&name);
        let g = Gathered {
            tree: Tree::default(),
            contents: vec![(input.clone(), name, d.len() as u64)],
            skipped: Vec::new(),
        };
        let view = view::View::new(&g, bars(), false);
        // (of the kind its name tells, as a file in an archive of many)
        let (c, f) = viewed(a, &view, |tell| compress_formula_telling(&d, kind, &a.opt, tell));
        write_whole(out, a.force, |w| w.write_all(&c))?;
        if !a.quiet && a.verbose {
            let modules: Vec<String> = f.applies().iter().map(|m| m.module.to_string()).collect();
            let format = detect(&d).map_or("-", |f| f.name());
            eprintln!(
                "{}: {}, {}, {} -> {} ({})",
                input.display(),
                format,
                if modules.is_empty() { "kept as it is".into() } else { modules.join(" ") },
                size(d.len() as f64),
                size(c.len() as f64),
                percent(c.len() as f64, d.len() as f64)
            );
        }
        if !a.quiet {
            summary(out, 1, d.len(), c.len(), start);
        }
        return Ok(());
    }

    let mut g = Gathered { tree: Tree::default(), contents: Vec::new(), skipped: Vec::new() };
    match inputs {
        // a directory alone: its entries at the root
        [dir] => {
            let m = meta(dir)?;
            g.tree.root = extra_of(&m);
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
    if !a.quiet {
        for s in &g.skipped {
            eprintln!("skipped {s}");
        }
    }
    // (with -v, the log as the files are written)
    let view = view::View::new(&g, bars(), a.verbose);
    // written as it is made
    let r = write_whole(out, a.force, |file| {
        let mut w = std::io::BufWriter::with_capacity(8 << 20, file);
        viewed(a, &view, |tell| compress_tree_to(&g.tree, &g, &a.opt, &mut w, tell).and_then(|r| w.flush().map(|_| r)))
    })?;
    if !a.quiet {
        eprintln!("archive overhead (header, modules, metadata): {}", size(r.overhead as f64));
        summary(out, g.contents.len(), r.read as usize, r.len, start);
    }
    Ok(())
}

/// Whether the bar is drawn: on a terminal that takes it.
fn bars() -> bool {
    std::io::stderr().is_terminal() && view::takes_escapes()
}

/// `work` done telling `view` how far it is (nothing, with -q), and the bar drawn again now and
/// then (for the time a file takes) until it is done.
fn viewed<T>(a: &Args, view: &view::View, work: impl FnOnce(&(dyn Fn(aqr_compress::Event) + Sync)) -> T) -> T {
    let done = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|s| {
        if !a.quiet && view.bars() {
            s.spawn(|| {
                while !done.load(std::sync::atomic::Ordering::Relaxed) {
                    std::thread::park_timeout(std::time::Duration::from_millis(250));
                    view.tick();
                }
            });
        }
        let quiet = |_: aqr_compress::Event| {};
        let tell: &(dyn Fn(aqr_compress::Event) + Sync) = if a.quiet { &quiet } else { &|e| view.tell(e) };
        let r = work(tell);
        done.store(true, std::sync::atomic::Ordering::Relaxed);
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

/// Gathers `path` as an entry `name` in `parent` (`None`: the root).
fn gather(path: &Path, name: &OsStr, parent: Option<usize>, g: &mut Gathered) -> Result<(), String> {
    let m = fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let name = name_from(&os_bytes(name));
    let extra = extra_of(&m);
    let t = m.file_type();
    let node = if t.is_dir() {
        EntryNode::Dir
    } else if t.is_file() {
        g.contents.push((path.to_owned(), os_bytes(path.as_os_str()), m.len()));
        EntryNode::File(g.contents.len() - 1)
    } else if t.is_symlink() {
        let target = fs::read_link(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let target = name_from(&os_bytes(target.as_os_str()));
        if name_bytes(&target).0.is_empty() {
            g.skipped.push(format!("{} (an empty link)", path.display()));
            return Ok(());
        }
        EntryNode::Link(target)
    } else {
        g.skipped.push(format!("{} (neither a file, a directory nor a link)", path.display()));
        return Ok(());
    };
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

fn extra_of(m: &fs::Metadata) -> Extra {
    let mtime = m.modified().ok().map(|t| match t.duration_since(UNIX_EPOCH) {
        Ok(d) => Time { secs: d.as_secs() as i64, nanos: d.subsec_nanos() },
        Err(e) => {
            let d = e.duration();
            let (s, n) = (d.as_secs() as i64, d.subsec_nanos());
            if n == 0 { Time { secs: -s, nanos: 0 } } else { Time { secs: -s - 1, nanos: 1_000_000_000 - n } }
        }
    });
    Extra { mtime, mode: mode_of(m) }
}

#[cfg(unix)]
fn mode_of(m: &fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(m.permissions().mode() & 0o7777)
}

#[cfg(not(unix))]
fn mode_of(_: &fs::Metadata) -> Option<u32> {
    None
}

#[cfg(unix)]
fn os_bytes(s: &OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    s.as_bytes().to_vec()
}

#[cfg(not(unix))]
fn os_bytes(s: &OsStr) -> Vec<u8> {
    s.to_string_lossy().replace('\\', "/").into_bytes()
}

#[cfg(unix)]
fn os_from(b: &[u8]) -> OsString {
    use std::os::unix::ffi::OsStrExt;
    OsStr::from_bytes(b).to_owned()
}

#[cfg(not(unix))]
fn os_from(b: &[u8]) -> OsString {
    String::from_utf8_lossy(b).into_owned().into()
}

/// Writes `d` to a new file `path` (or over one, where `force`).
fn write_new(path: &Path, d: &[u8], force: bool) -> Result<(), String> {
    create(path, force)?.write_all(d).map_err(|e| format!("{}: {e}", path.display()))
}

/// Writes a new file `path` (or one in place of it, where `force`) by `write`: under a name of
/// its own beside it, renamed once it is written (so that `path` is never written in part),
/// removed where it is not (`write` failing, or the process stopped: [`stop`]).
fn write_whole<T>(
    path: &Path,
    force: bool,
    write: impl FnOnce(&mut fs::File) -> std::io::Result<T>,
) -> Result<T, String> {
    let err = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    let mut name = path.file_name().ok_or(format!("{}: not a file's name", path.display()))?.to_owned();
    name.push(format!(".{}.part", std::process::id()));
    let part = path.with_file_name(name);
    let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&part).map_err(|e| err(&part, e))?;
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
        return fs::rename(from, to).map_err(err);
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
    fs::OpenOptions::new().write(true).create_new(true).open(path).map_err(err)
}

fn decompress_all(a: &Args) -> Result<(), String> {
    for input in &a.inputs {
        let out = match &a.output {
            Some(o) => o.clone(),
            None => match input.extension() {
                Some(e) if e == "aqr" => input.with_extension(""),
                _ => return Err(format!("{}: not named .aqr (-o to name the output)", input.display())),
            },
        };
        decompress_into(a, input, &out)?;
    }
    Ok(())
}

fn decompress_into(a: &Args, input: &Path, out: &Path) -> Result<(), String> {
    if !a.test {
        output(out, a.force)?;
    }
    let start = Instant::now();
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", input.display());
    let file = fs::File::open(input).map_err(|e| err(&e))?;
    // (mapped, so that what is read of it is the system's to drop)
    // SAFETY: the archive is not to change while it is read; where it does, its checksums tell
    let c = unsafe { memmap2::Mmap::map(&file) }.map_err(|e| err(&e))?;
    let decoder =
        Decoder { native: Native { threads: a.opt.threads }, wasm: a.wasm, sandbox: a.sandbox.then(OnceLock::new) };
    let runner: &dyn aqr::Runner = &decoder;
    let mut archive = aqr::open(&c, runner).map_err(|e| err(&e))?;
    archive.set_budget(DECODE_BUDGET);
    match archive.tree() {
        None => {
            drop(archive);
            let d = aqr::decode_value_with(&c, runner, a.opt.threads).map_err(|e| err(&e))?;
            let Value::File(d) = d else { unreachable!("an archive of a single file") };
            if !a.test {
                write_whole(out, a.force, |w| w.write_all(&d))?;
            }
            if !a.quiet {
                summary_d(input, (!a.test).then_some(out), 1, d.len() as u64, start);
            }
        }
        Some(t) if a.test => {
            let (n, total) = (std::sync::atomic::AtomicUsize::new(0), std::sync::atomic::AtomicU64::new(0));
            let files = t.entries.iter().fold(vec![0; archive.contents_len()], |mut f, e| {
                if let EntryNode::File(c) = e.node {
                    f[c] += 1;
                }
                f
            });
            archive
                .contents(a.opt.threads, |c, d| {
                    n.fetch_add(files[c], std::sync::atomic::Ordering::Relaxed);
                    total.fetch_add(files[c] as u64 * d.len() as u64, std::sync::atomic::Ordering::Relaxed);
                    Ok(())
                })
                .map_err(|e| err(&e))?;
            if !a.quiet {
                summary_d(input, None, n.into_inner(), total.into_inner(), start);
            }
        }
        Some(t) => {
            let (n, total) = extract(t, &archive, a.opt.threads, out, a.force, a.quiet)?;
            if !a.quiet {
                summary_d(input, Some(out), n, total, start);
            }
        }
    }
    Ok(())
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

/// Extracts tree `t` of `archive` into directory `out`: the directories first, then the files
/// as their contents are decoded (on up to `threads` threads), then the links (but those
/// reaching out of `out`), then the times of the directories. Returns the number of files and
/// their bytes.
fn extract(
    t: &Tree,
    archive: &aqr::Archive,
    threads: usize,
    out: &Path,
    force: bool,
    quiet: bool,
) -> Result<(usize, u64), String> {
    let err = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    match fs::create_dir(out) {
        Err(e) if !(force && e.kind() == std::io::ErrorKind::AlreadyExists && out.is_dir()) => return Err(err(out, e)),
        _ => {}
    }
    let mut paths: Vec<PathBuf> = Vec::with_capacity(t.entries.len());
    let mut links = Vec::new();
    // by content, the files of it
    let mut files: Vec<Vec<usize>> = vec![Vec::new(); archive.contents_len()];
    for (i, e) in t.entries.iter().enumerate() {
        let (name, _) = name_bytes(&e.name);
        let parent = e.parent.map_or(out, |p| &paths[p]);
        let path = parent.join(os_from(name));
        match &e.node {
            EntryNode::Dir => match fs::create_dir(&path) {
                Err(x) if !(force && path.is_dir() && !path.is_symlink()) => return Err(err(&path, x)),
                _ => {}
            },
            EntryNode::File(c) => files[*c].push(i),
            EntryNode::Link(_) => links.push(i),
            EntryNode::Special(kind) => {
                if !quiet {
                    eprintln!("{}: a special entry (kind {kind}), not created", path.display());
                }
            }
        }
        paths.push(path);
    }
    let (n, total) = (std::sync::atomic::AtomicUsize::new(0), std::sync::atomic::AtomicU64::new(0));
    archive
        .contents(threads, |c, d| {
            for &i in &files[c] {
                let fail = |m: String| aqr::Failure::Resource(m);
                write_new(&paths[i], &d, force).map_err(fail)?;
                set_extra(&paths[i], &t.entries[i].extra).map_err(fail)?;
                n.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                total.fetch_add(d.len() as u64, std::sync::atomic::Ordering::Relaxed);
            }
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    for i in links {
        let e = &t.entries[i];
        let EntryNode::Link(target) = &e.node else { unreachable!() };
        if escapes(t, i) {
            eprintln!("{}: a link reaching out of {}, not created", paths[i].display(), out.display());
            continue;
        }
        if force && fs::symlink_metadata(&paths[i]).is_ok_and(|m| !m.is_dir()) {
            fs::remove_file(&paths[i]).map_err(|x| err(&paths[i], x))?;
        }
        symlink(&os_from(name_bytes(target).0), &paths[i]).map_err(|x| err(&paths[i], x))?;
    }
    // the directories' times once nothing more is written in them, the deepest first
    for (i, e) in t.entries.iter().enumerate().rev() {
        if e.node == EntryNode::Dir {
            set_extra(&paths[i], &e.extra)?;
        }
    }
    set_extra(out, &t.root)?;
    Ok((n.into_inner(), total.into_inner()))
}

/// Whether link `i` of `t` reaches out of the directory it is extracted into: by an absolute
/// target, or by one that, resolved from the link's directory through the links of the tree,
/// goes above it.
fn escapes(t: &Tree, i: usize) -> bool {
    // the path of an entry, as names from the root
    let path_of = |mut e: Option<usize>| {
        let mut p = Vec::new();
        while let Some(i) = e {
            p.push(t.entries[i].name.clone());
            e = t.entries[i].parent;
        }
        p.reverse();
        p
    };
    let find = |path: &[Vec<u8>]| -> Option<usize> {
        let mut at = None;
        for name in path {
            at = Some(t.entries.iter().position(|e| e.parent == at && &e.name == name)?);
        }
        at
    };
    // (the tree's own links followed, as many as there are entries at most)
    let mut budget = t.entries.len() + 1;
    fn resolve(
        t: &Tree,
        dir: Vec<Vec<u8>>,
        target: &[u8],
        budget: &mut usize,
        find: &dyn Fn(&[Vec<u8>]) -> Option<usize>,
        path_of: &dyn Fn(Option<usize>) -> Vec<Vec<u8>>,
    ) -> Option<Vec<Vec<u8>>> {
        let (bytes, _) = name_bytes(target);
        if bytes.starts_with(b"/") || (cfg!(windows) && bytes.get(1) == Some(&b':')) {
            return None;
        }
        let mut at = dir;
        for part in bytes.split(|&b| b == b'/') {
            match part {
                b"" | b"." => {}
                b".." => {
                    at.pop()?;
                }
                name => {
                    at.push(name_from(name));
                    if let Some(j) = find(&at)
                        && let EntryNode::Link(next) = &t.entries[j].node
                    {
                        *budget = budget.checked_sub(1)?;
                        let parent = path_of(t.entries[j].parent);
                        at = resolve(t, parent, next, budget, find, path_of)?;
                    }
                }
            }
        }
        Some(at)
    }
    let EntryNode::Link(target) = &t.entries[i].node else { return false };
    resolve(t, path_of(t.entries[i].parent), target, &mut budget, &find, &path_of).is_none()
}

#[cfg(unix)]
fn symlink(target: &OsStr, at: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, at)
}

#[cfg(windows)]
fn symlink(target: &OsStr, at: &Path) -> std::io::Result<()> {
    let target = PathBuf::from(target.to_string_lossy().replace('/', "\\"));
    std::os::windows::fs::symlink_file(target, at)
}

/// Applies `extra` to the file or directory at `path`: its permissions, then its time.
fn set_extra(path: &Path, extra: &Extra) -> Result<(), String> {
    let err = |e: std::io::Error| format!("{}: {e}", path.display());
    #[cfg(unix)]
    if let Some(mode) = extra.mode {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(err)?;
    }
    if let Some(t) = extra.mtime {
        let time = match t.secs {
            s if s >= 0 => UNIX_EPOCH + std::time::Duration::new(s as u64, t.nanos),
            s => {
                UNIX_EPOCH - std::time::Duration::from_secs(s.unsigned_abs())
                    + std::time::Duration::from_nanos(t.nanos as u64)
            }
        };
        let f = fs::File::options().write(!path.is_dir()).read(path.is_dir()).open(path);
        // (where a file's permissions do not let it be opened, its time is left)
        if let Ok(f) = f {
            f.set_modified(time).map_err(err)?;
        }
    }
    Ok(())
}
