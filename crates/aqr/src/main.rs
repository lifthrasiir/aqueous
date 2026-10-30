//! Compresses files and directories into Aqueous archives, and decompresses them, as zstd does.
//!
//! usage: aqr [-c | -d | -t] [-o OUTPUT] [-f] [-p] [-q | -v] [-T THREADS] [-l LEVEL | -0 ... -9] [--depth N]
//! [--batch MIB | -B SIZE] [-b SIZE] [-D] [--no-check] [--wasm] [--no-sandbox] INPUT...
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
//! - The metadata of a directory archived is kept ([`meta`]): times, permissions, ownership,
//!   flags, extended attributes and resource forks, hard links, FIFOs and devices; and applied
//!   as it is extracted, but what is not commonly kept. `-p` keeps that too: in compressing,
//!   the status change and backup times; in decompressing, the ownership, the flags only the
//!   superuser can change, the set-user-ID and set-group-ID bits, FIFOs and devices, and the
//!   backup times. An archive of a single file has no metadata.
//! - `-f` overwrites outputs; inputs are always kept. `-q` logs nothing but errors; `-v` logs
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
//!
//! Compressing, standard error has the summary of the archive at the end, and on a terminal a
//! bar of how far it is ([`view`]) until then. With `-v`, a log above the bar tells, for each
//! file, what it was taken for, the modules its formula applies and the bytes of the archive
//! that are its; the files stored together in a block (solid) or coded against each other
//! (sets); the files of the same content as another. It is written a batch at a time, as the
//! files are. Decompressing (and testing), there is a bar too, of the blocks decompressed and
//! the files evaluated ([`view::Decoding`]).

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::OnceLock;
use std::time::Instant;

mod meta;
mod stop;
mod view;

use aqr_compress::mapped::Mapper;
use aqr_compress::{Options, compress_formula_to, compress_tree_to, detect, kind_by_name};
use aqr_decompress::{Native, Wasm, sandbox};
use aqr_formula::Bytes;
use aqr_formula::aqr::{self, Entry, EntryNode, Failure, Tree, name_bytes, name_from};
use meta::{os_bytes, os_from};
use view::Ticking;

const USAGE: &str = "usage: aqr [-c | -d | -t] [-o OUTPUT] [-f] [-p] [-q | -v] [-T THREADS] [-l 0-9|low|mid|high|zstdN | -0 ... -9] [--depth N] \
     [--batch MIB | -B SIZE] [-b SIZE] [-D] [--no-check] [--wasm] [--no-sandbox] INPUT...";

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
    /// What is not commonly kept of the metadata kept too ([`meta`]).
    preserve: bool,
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
        preserve: false,
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
    output(out, a.force)?;
    let start = Instant::now();
    // large buffers kept in a file beside the output (gone once done), not on the heap
    let mut opt = a.opt.clone();
    let mut name = out.file_name().ok_or(format!("{}: not a file's name", out.display()))?.to_owned();
    name.push(format!(".{}.spill", std::process::id()));
    opt.spill = Some(out.with_file_name(name));
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
        let (n, f) = write_whole(out, a.force, |file| {
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
        return Ok(());
    }

    let mut g = Gathered::new(mapper, a.preserve);
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
    if !a.quiet {
        for s in &g.skipped {
            eprintln!("skipped {s}");
        }
        g.warnings.iter().for_each(|w| eprintln!("{w}"));
    }
    // (with -v, the log as the files are written)
    let view = view::View::new(&g, bars(), a.verbose);
    // written as it is made
    let r = write_whole(out, a.force, |file| {
        let mut w = std::io::BufWriter::with_capacity(8 << 20, file);
        viewed(a, &view, |tell| compress_tree_to(&g.tree, &g, opt, &mut w, tell).and_then(|r| w.flush().map(|_| r)))
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
    let bars = !a.quiet && view.bars();
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
    let names = if !a.quiet && bars() { leaf_names(&archive, out) } else { Vec::new() };
    let view = std::sync::Arc::new(view::Decoding::new(bars(), names));
    if !a.quiet && view.bars() {
        let view = view.clone();
        archive.on_progress(Some(std::sync::Arc::new(move |r| view.tell(r))));
    }
    // (of a single file, the leaves all held for the file anyway)
    if archive.tree().is_some() {
        archive.set_budget(DECODE_BUDGET);
    }
    match archive.tree() {
        // (written as it is evaluated: a file of parts is not held whole)
        None if a.test => {
            let n = ticking(a, &*view, || archive.write_file(a.opt.threads, &mut |_: &[u8]| Ok(())))
                .map_err(|e| err(&e))?;
            if !a.quiet {
                summary_d(input, None, 1, n, start);
            }
        }
        None => {
            let n = write_whole(out, a.force, |w| {
                let mut failed = None;
                let r = ticking(a, &*view, || {
                    archive.write_file(a.opt.threads, &mut |d: &[u8]| {
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
            if !a.quiet {
                summary_d(input, Some(out), 1, n, start);
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
            ticking(a, &*view, || {
                archive.contents(a.opt.threads, |c, d| {
                    n.fetch_add(files[c], std::sync::atomic::Ordering::Relaxed);
                    total.fetch_add(files[c] as u64 * d.len() as u64, std::sync::atomic::Ordering::Relaxed);
                    Ok(())
                })
            })
            .map_err(|e| err(&e))?;
            if !a.quiet {
                summary_d(input, None, n.into_inner(), total.into_inner(), start);
            }
        }
        Some(t) => {
            let ap = meta::Applier { privileged: a.preserve, quiet: a.quiet };
            let (n, total) = ticking(a, &*view, || extract(t, &archive, a.opt.threads, out, a.force, &ap))?;
            if !a.quiet {
                summary_d(input, Some(out), n, total, start);
            }
        }
    }
    Ok(())
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

/// Extracts tree `t` of `archive` into directory `out`: the directories, the files (empty, or
/// hard links to the first of their link group), FIFOs and devices first; then the contents
/// of the files and their forks as they are decoded (on up to `threads` threads); then the
/// links (but those reaching out of `out`); then the metadata of all but the directories, and
/// then that of the directories, the deepest first. Returns the number of files and their
/// bytes.
fn extract(
    t: &Tree,
    archive: &aqr::Archive,
    threads: usize,
    out: &Path,
    force: bool,
    ap: &meta::Applier,
) -> Result<(usize, u64), String> {
    let err = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    match fs::create_dir(out) {
        Err(e) if !(force && e.kind() == std::io::ErrorKind::AlreadyExists && out.is_dir()) => return Err(err(out, e)),
        _ => {}
    }
    let linked = hard_links(t);
    let mut paths: Vec<PathBuf> = Vec::with_capacity(t.entries.len());
    // by entry, whether it was made (and so is to get its metadata)
    let mut made = vec![true; t.entries.len()];
    let mut links = Vec::new();
    // by content, the files written of it (but hard links), and the number of files of it
    let mut files: Vec<(Vec<usize>, usize)> = vec![(Vec::new(), 0); archive.contents_len()];
    for (i, e) in t.entries.iter().enumerate() {
        let (name, _) = name_bytes(&e.name);
        let parent = e.parent.map_or(out, |p| &paths[p]);
        let path = parent.join(os_from(name));
        match &e.node {
            EntryNode::Dir => match fs::create_dir(&path) {
                Err(x) if !(force && path.is_dir() && !path.is_symlink()) => return Err(err(&path, x)),
                _ => {}
            },
            EntryNode::File(c) => {
                files[*c].1 += 1;
                match linked[i] {
                    Some(j) => {
                        if force && fs::symlink_metadata(&path).is_ok_and(|m| !m.is_dir()) {
                            fs::remove_file(&path).map_err(|x| err(&path, x))?;
                        }
                        fs::hard_link(&paths[j], &path).map_err(|x| err(&path, x))?;
                    }
                    None => {
                        create(&path, force)?;
                        files[*c].0.push(i);
                    }
                }
            }
            EntryNode::Link(_) => {
                links.push(i);
                made[i] = false;
            }
            EntryNode::Fifo | EntryNode::Device { .. } => {
                if force && fs::symlink_metadata(&path).is_ok_and(|m| !m.is_dir()) {
                    fs::remove_file(&path).map_err(|x| err(&path, x))?;
                }
                made[i] = ap.special(&path, &e.node)?;
            }
            EntryNode::Special(kind) => {
                ap.tell(&path, &format!("a special entry (kind {kind}), not created"));
                made[i] = false;
            }
        }
        paths.push(path);
    }
    // by content, the forks of it: the entry (`None`: the root) and the fork's index
    let mut forks: Vec<Vec<(Option<usize>, usize)>> = vec![Vec::new(); archive.contents_len()];
    let extras = std::iter::once((None, &t.root)).chain(t.entries.iter().enumerate().map(|(i, e)| (Some(i), &e.extra)));
    for (i, x) in extras {
        if i.is_none_or(|i| linked[i].is_none()) {
            x.forks.iter().enumerate().for_each(|(k, f)| forks[f.1].push((i, k)));
        }
    }
    let kind_of = |i: Option<usize>| match i.map(|i| &t.entries[i].node) {
        None | Some(EntryNode::Dir) => meta::Kind::Dir,
        Some(EntryNode::File(_)) => meta::Kind::File,
        Some(EntryNode::Link(_)) => meta::Kind::Link,
        Some(_) => meta::Kind::Special,
    };
    let path_of = |i: Option<usize>| i.map_or(out, |i| &paths[i]);
    // (the forks of links written once the links are)
    let later = std::sync::Mutex::new(Vec::new());
    let (n, total) = (std::sync::atomic::AtomicUsize::new(0), std::sync::atomic::AtomicU64::new(0));
    archive
        .contents(threads, |c, d| {
            for &i in &files[c].0 {
                let fail = |m: String| aqr::Failure::Resource(m);
                let p = &paths[i];
                // (the file made empty already)
                let mut f = fs::OpenOptions::new().write(true).open(p).map_err(|e| fail(err(p, e)))?;
                f.write_all(&d).map_err(|e| fail(err(p, e)))?;
            }
            for &(i, k) in &forks[c] {
                let (kind, x) = (kind_of(i), i.map_or(&t.root, |i| &t.entries[i].extra));
                match kind {
                    meta::Kind::Link => later.lock().unwrap().push((i, k, d.clone())),
                    _ if i.is_none_or(|i| made[i]) => ap.fork(path_of(i), kind, &x.forks[k].0, &d),
                    _ => {}
                }
            }
            n.fetch_add(files[c].1, std::sync::atomic::Ordering::Relaxed);
            total.fetch_add(files[c].1 as u64 * d.len() as u64, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    for i in links {
        let e = &t.entries[i];
        let EntryNode::Link(target) = &e.node else { unreachable!() };
        let Some(to) = resolve_link(t, i) else {
            ap.tell(&paths[i], &format!("a link reaching out of {}, not created", out.display()));
            continue;
        };
        if force && fs::symlink_metadata(&paths[i]).is_ok_and(|m| !m.is_dir()) {
            fs::remove_file(&paths[i]).map_err(|x| err(&paths[i], x))?;
        }
        // (on Windows, of the kind recorded, else by what it leads to; spec, "Extraction")
        let dir = match e.extra.win_link_type {
            Some(k) => k != 0,
            None => to.is_some_and(|j| t.entries[j].node == EntryNode::Dir),
        };
        symlink(&os_from(name_bytes(target).0), &paths[i], dir).map_err(|x| err(&paths[i], x))?;
        made[i] = true;
    }
    for (i, k, d) in later.into_inner().unwrap() {
        if i.is_some_and(|i| made[i]) {
            ap.fork(path_of(i), meta::Kind::Link, &t.entries[i.unwrap()].extra.forks[k].0, &d);
        }
    }
    // the metadata of the directories once nothing more is written in them, the deepest first
    for (i, e) in t.entries.iter().enumerate() {
        if e.node != EntryNode::Dir && made[i] && linked[i].is_none() {
            ap.apply(&paths[i], kind_of(Some(i)), &e.extra)?;
        }
    }
    for (i, e) in t.entries.iter().enumerate().rev() {
        if e.node == EntryNode::Dir {
            ap.apply(&paths[i], meta::Kind::Dir, &e.extra)?;
        }
    }
    ap.apply(out, meta::Kind::Dir, &t.root)?;
    Ok((n.into_inner(), total.into_inner()))
}

/// By entry of `t`, the file it is a hard link to: the first of its link group, where the group
/// is consistent (its files all of a content).
fn hard_links(t: &Tree) -> Vec<Option<usize>> {
    let mut groups: std::collections::HashMap<i128, Vec<usize>> = Default::default();
    for (i, e) in t.entries.iter().enumerate() {
        if let (Some(g), EntryNode::File(_)) = (e.extra.link_group, &e.node) {
            groups.entry(g).or_default().push(i);
        }
    }
    let mut to = vec![None; t.entries.len()];
    for files in groups.values() {
        if files.iter().all(|&i| t.entries[i].node == t.entries[files[0]].node) {
            files[1..].iter().for_each(|&i| to[i] = Some(files[0]));
        }
    }
    to
}

/// Where link `i` of `t` leads, resolved from the link's directory through the links of the
/// tree: the entry (`None`: none of the tree); `None` where it reaches out of the directory it
/// is extracted into, by an absolute target or by one going above it.
fn resolve_link(t: &Tree, i: usize) -> Option<Option<usize>> {
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
    let EntryNode::Link(target) = &t.entries[i].node else { return Some(None) };
    resolve(t, path_of(t.entries[i].parent), target, &mut budget, &find, &path_of).map(|p| find(&p))
}

/// Makes a link to `target` at `at` (on Windows, to a directory where `dir`).
#[cfg(unix)]
fn symlink(target: &OsStr, at: &Path, _: bool) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, at)
}

#[cfg(windows)]
fn symlink(target: &OsStr, at: &Path, dir: bool) -> std::io::Result<()> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    let slash = |c: u16| if c == b'/' as u16 { b'\\' as u16 } else { c };
    let target = PathBuf::from(OsString::from_wide(&target.encode_wide().map(slash).collect::<Vec<_>>()));
    if dir { std::os::windows::fs::symlink_dir(target, at) } else { std::os::windows::fs::symlink_file(target, at) }
}
