//! Compresses files and directories into Aqueous archives, and decompresses them, as zstd does.
//!
//! usage: aqr [-c | -d | -t | -L] [-o OUTPUT] [-f] [-p] [-q | -v] [-T THREADS] [-l LEVEL | -0 ... -9] [--depth N]
//! [--batch MIB | -B SIZE] [-b SIZE] [-D] [--no-check] [-P] [--wasm] [--no-sandbox] [--stats[=FILE]] [-h]
//! INPUT [--rename TO]...
//!
//! aqr -d [OPTIONS] ARCHIVE [PATH [--rename TO]]...; aqr -d --each [OPTIONS] ARCHIVE...; aqr -L [-q | -v] ARCHIVE...
//!
//! - `-c` (the default; `aqr`): each input into `INPUT.aqr`, a file as an archive of a single
//!   file, a directory as an archive of what it holds; with `-o`, all the inputs into one
//!   archive (several of them, or a directory, as entries of their names). Several inputs, or
//!   one renamed, are archived by their paths: a relative one as it is given (`.` left out,
//!   the directories on the way made plainly, without their metadata), an absolute one or one
//!   with `..` by its last name alone, as 7-Zip does (tar, zip and RAR would keep an absolute
//!   path whole, but the frontends give absolute paths); `--rename TO` after an input has it at
//!   `TO` (`/` between names) instead. Two inputs of the same path are an error.
//! - `--format FORMAT` (or `-o` of a name ending in its extension): an archive of another
//!   format instead ([`aqr_pack`]): ZIP, 7z, ALZ, EGG, tar alone or under gzip, bzip2, xz, LZMA
//!   or Zstandard, or those compressors' stream of a single file; `INPUT.EXT` without `-o`. A
//!   single input is in it by its name (a directory with what it holds under it, as 7-Zip, `zip
//!   -r` and tar have it). `-l` is the format's level there ([`aqr_pack::Format::levels`]: zlib's
//!   0-9 of ZIP, ALZ, EGG and gzip, XZ Utils' presets of xz, LZMA and 7z, bzip2's 1-9,
//!   Zstandard's 1-22). What the format does not keep (FIFOs, devices, extended attributes,
//!   forks) is told; `-P` encrypts ZIP (WinZip's AES-256) and 7z archives (AES-256, the header
//!   too).
//! - `-d` (the default as `unaqr`): an archive into its name without `.aqr`, or `-o`. An
//!   archive of another format, by its name (ZIP, 7z, RAR, ALZ, EGG, tar alone or under gzip, bzip2,
//!   xz, LZMA or Zstandard, and those compressors' single files: [`aqr_unpack::by_name`]), is extracted
//!   as it is into the current directory, or `-o` (made where it is not), as `tar`, `unzip` and
//!   7-Zip do; a compressor's single file into its name without its extension, as gzip does.
//!   Its metadata is taken as an Aqueous archive's ([`extract`]); what it holds that is not
//!   extracted (encrypted entries, methods not known, paths reaching out) is told, and the run
//!   fails once the rest is extracted. A file there already is written over only with `-f`.
//! - `-d ARCHIVE PATH...` (and `-t`): the paths of the archive given alone (`/` between names;
//!   a directory with what is under it), where the whole archive would have them, with the
//!   directories on the way (made plainly, without their metadata); `--rename TO` after a path
//!   has that one at `TO` instead (with what is under it; the directories before `TO` made).
//!   Only what they need is decoded ([`aqr::Archive::contents_of`], [`aqr_unpack::Select`]). A
//!   path not in the archive is an error, before anything is extracted where its listing tells
//!   (but in a tar archive, after); so is one under another.
//! - `-C DIR` (`--directory`), as tar has it: compressing, the inputs after it found in `DIR`
//!   (each `-C` from the one before) and archived as if this were run there, `INPUT.aqr`
//!   beside them; decompressing (given anywhere), what is extracted goes into `DIR` in place of
//!   the current directory or beside the archive, `-o` from there too. The archives (`-o`
//!   compressing, the inputs decompressing) are where they are given, as tar's `-f`.
//! - `--each`: decompressing, every input an archive, each extracted whole (as the frontends
//!   extract a selection of archives).
//! - `--auto-dir` (as the frontends extract): a single entry at the top of an archive (of
//!   either kind) is extracted beside it as it is, where nothing is there of its name; else
//!   the archive is extracted into a directory of its name without its extension, beside it
//!   (`foo.zip` of `foo/` into `foo/`, never `foo/foo/`). `-o` is where it goes instead of
//!   beside it, a directory (with several archives too).
//! - `-t`: each archive decoded as `-d` does, but nothing written (its checksums checked).
//! - `-L`: the entries of each archive listed on standard output, and what each file takes of
//!   the archive and how it is made ([`list`]); `-q` the paths alone, `-v` all that is told.
//! - `-P`: compressing, the archives encrypted with a passphrase ([`aqr::envelope`]), asked for
//!   as the work begins (twice on a terminal); decompressing, asked for then rather than as an
//!   archive needs it. An encrypted archive (of either kind) is opened by the passphrases that
//!   opened those before it, else asks for one where there is a terminal or a frontend
//!   ([`pass`]). Without a terminal, `-P` takes a line of standard input. An empty passphrase
//!   is a passphrase too.
//! - `-E`, `--normalize`, `--bad-names`: decompressing, how the archives' names are written
//!   ([`names`], `docs/names-design.md`): names of no declared encoding decoded from the set
//!   given, guessed (`auto`), or kept as bytes (`raw`); Unicode names composed (`auto`: but on
//!   macOS), `nfc`, `nfd` or kept; and names this system cannot write as they are kept as bytes
//!   where it takes them, replaced (`%XX`, U+FFFD, `_`), or failed. What was guessed or done is
//!   told.
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
mod fold;
mod ipc;
mod list;
mod meta;
mod names;
mod pass;
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
usage: aqr [OPTIONS] INPUT [--rename TO]...
       aqr -d [OPTIONS] ARCHIVE [PATH [--rename TO]]...
       aqr -d --each [OPTIONS] ARCHIVE...
       aqr -L [-q | -v] ARCHIVE...
       unaqr [OPTIONS] ARCHIVE [PATH [--rename TO]]...

Modes:
  -c, --compress        each input into INPUT.aqr (the default as aqr)
  -d, --decompress      an archive into its name without .aqr (the default as unaqr);
                        .zip, .7z, .rar, .alz, .egg and .tar (.gz .tgz .bz2 .xz
                        .lzma .zst) too, as they are into the current directory
                        (or -o); .gz, .bz2, .xz, .lzma and .zst into the name
                        without them
  -t, --test            decode and check an archive, writing nothing
  -L, --list            list the entries of archives (-q: the paths alone; -v: the
                        blocks each file takes, its formula and all its metadata)

Compressing (aqr -o ARCHIVE INPUT [--rename TO]...):
  INPUT                 a file or directory; several of them archived by their paths,
                        a relative one as given, an absolute one or one with .. by
                        its last name
      --rename TO       after an INPUT: that one archived at TO (names between /)
      --format FORMAT   aqr (the default), zip, 7z, alz, egg, tar, tar.gz, tar.bz2,
                        tar.xz, tar.lzma, tar.zst, or gz, bz2, xz, lzma, zst of a
                        single file (also by -o's name); -l is the format's level
                        (zlib's 0-9; xz, lzma, 7z: 0-9; bz2: 1-9; zst: 1-22)

Decompressing some paths (aqr -d ARCHIVE PATH [--rename TO]...):
  PATH                  a path in the archive (a directory with what is under it),
                        extracted where the whole archive would have it
      --rename TO       after a PATH: that one extracted at TO instead
      --each            every input an archive, each extracted whole

Output:
  -o OUTPUT             write to OUTPUT; compressing, all the inputs into one archive
  -C, --directory DIR   compressing, the inputs after it found in DIR, archived as if
                        run there; decompressing, extract into DIR (-o in it too);
                        the archives' own paths are taken from where aqr is run
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
  -P, --passphrase      encrypt the archives with a passphrase asked for (decompressing:
                        ask for it at once; an encrypted archive asks anyway where there
                        is a terminal); without a terminal, a line of standard input
  -l LEVEL, -0 ... -9   0 (uncompressed), 1 (zstd alone) to 9; low, mid (default), high
                        are 2, 5 and 8; zstdN is zstd level N with recompression as mid
      --depth N         recompress formats nested at most N deep (default 3)
      --batch MIB       make about MIB MiB of files at once (default 512)
  -B SIZE               --batch as a size (512M)
  -b SIZE               the most bytes of a block that files share (default 16M)
  -D                    compress blocks alike with dictionaries trained for them
      --no-check        do not decode the archive made to check it

Decompression:
  -E, --encoding ENC    names of no declared encoding: auto (the default: as bytes where
                        this system takes them, else decoded from a set guessed), raw
                        (as bytes), system, or sets (shift_jis, euc-kr, gbk, big5,
                        cp437, ...; several: guessed among them)
      --normalize FORM  auto (the default: composed, but on macOS), nfc, nfd or keep
      --bad-names WHAT  names not written as they are: auto (the default), keep (as
                        bytes, where this system takes them), replace (%XX, U+FFFD,
                        _ for what Windows does not take), or fail
      --wasm            run every module as WebAssembly, the known ones too
      --no-sandbox      run the Wasm modules (the ones not known, or all with --wasm)
                        in this process, not in workers locked down by the system;
                        where the system cannot lock workers down, they run only so

General:
  -T THREADS            threads to use (default 0: as many as the machine has)
  -h, --help            show this help
  -V, --version         show the version

SIZE is a number of bytes, or of KiB, MiB or GiB with K, M or G.
";

/// The argument (the only one) that this program is run with as a worker of the sandbox.
const SANDBOX_WORKER: &str = "--sandbox-worker";

struct Args {
    decompress: bool,
    /// Decompressing, but nothing written.
    test: bool,
    /// Listing the archives' entries (`-L`).
    list: bool,
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
    /// Compressing, the paths found (in the directory of `-C` before each); decompressing, the
    /// archives, then the paths of one selected.
    inputs: Vec<PathBuf>,
    /// Compressing, by input, the path as given (from the directory of `-C` before it), which
    /// it is archived by.
    given: Vec<PathBuf>,
    /// Decompressing, the directory of `-C` (empty: the current one), where what is extracted
    /// goes.
    dir: PathBuf,
    /// Compressing, by input, the path in the archive that `--rename` gives it.
    renames: Vec<Option<PathBuf>>,
    /// What `--stats` asked for.
    stats: Option<stats::Stats>,
    /// Driven by a frontend ([`ipc`]).
    ipc: bool,
    /// Compressing, into archives of this format (`--format`, or `-o`'s name).
    format: Pack,
    /// `-l` as given (read as the format's level: [`Args::format`]'s, or Aqueous's).
    level: Option<String>,
    /// `-P`: compressing, the archives encrypted with a passphrase asked for; decompressing,
    /// the passphrase asked for before the archives need it ([`pass`]).
    passphrase: bool,
    /// `--each`: decompressing, every input an archive.
    each: bool,
    /// Decompressing some paths of the archive: each path (names as the tree has them), and
    /// where it goes where `--rename` tells.
    select: Option<extract::Selection>,
    /// Decompressing, how names are written (`-E`, `--normalize`, `--bad-names`).
    names: names::Opts,
}

/// The format of the archives made: Aqueous, or another of [`aqr_pack`] at its level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pack {
    Aqr,
    Other(aqr_pack::Format, Option<u8>),
}

impl Pack {
    /// The extension of its archives.
    fn ext(self) -> String {
        match self {
            Pack::Aqr => ".aqr".into(),
            Pack::Other(f, _) => f.ext(),
        }
    }
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
    if let Err(e) = pass::init(args.passphrase, !args.decompress && !args.list) {
        eprintln!("aqr: {e}");
        ipc::tell(serde_json::json!({"event": "error", "message": e}));
        return ExitCode::FAILURE;
    }
    // (before the pool is made, for its workers to be sampled)
    if args.stats.is_some() {
        aqr_common::trace::enable();
    }
    aqr_common::pool::init(args.opt.threads);
    let r = aqr_common::pool::run(|| match () {
        _ if args.list => list::list_all(&args),
        _ if args.decompress => decompress_all(&args),
        _ => compress_all(&args),
    });
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
        list: false,
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
        given: Vec::new(),
        dir: PathBuf::new(),
        renames: Vec::new(),
        stats: None,
        ipc: false,
        format: Pack::Aqr,
        level: None,
        passphrase: false,
        each: false,
        select: None,
        names: names::Opts::default(),
    };
    // (by the number of inputs before it, `--rename`'s place and value)
    let mut renames: Vec<(usize, PathBuf)> = Vec::new();
    let (mut stats, mut stats_to) = (false, Vec::new());
    let mut only_inputs = false;
    let mut format = None;
    // (`-C`: the directory so far, by input the one it was given in, and the last given)
    let (mut dir, mut dirs, mut last_dir) = (PathBuf::new(), Vec::new(), None);
    while let Some(arg) = argv.next() {
        let s = arg.to_string_lossy();
        if only_inputs || !s.starts_with('-') || s == "-" {
            a.inputs.push(arg.into());
            dirs.push(dir.clone());
            continue;
        }
        // (as given: a path need not be Unicode; [`text`] for the others)
        let mut value = |name: &str| argv.next().ok_or(format!("{name} takes a value"));
        match &*s {
            "--" => only_inputs = true,
            "-h" | "--help" => {
                print!("{HELP}");
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("aqr {}", env!("AQR_VERSION"));
                std::process::exit(0);
            }
            "-c" | "--compress" => (a.decompress, a.list) = (false, false),
            "-d" | "--decompress" => (a.decompress, a.test, a.list) = (true, false, false),
            "-t" | "--test" => (a.decompress, a.test, a.list) = (true, true, false),
            "-L" | "--list" => a.list = true,
            "--wasm" => a.wasm = true,
            "--no-sandbox" => a.sandbox = false,
            "-o" => a.output = Some(value("-o")?.into()),
            "-C" | "--directory" => {
                let to = dir.join(value("-C")?);
                if let Err(e) = fs::read_dir(&to) {
                    return Err(format!("-C {}: {e}", to.display()));
                }
                (dir, last_dir) = (to, Some(a.inputs.len()));
            }
            "--auto-dir" => a.auto_dir = true,
            "-f" | "--force" => a.force = true,
            "-p" | "--preserve" => a.preserve = true,
            "-q" | "--quiet" => a.quiet = true,
            "-v" | "--verbose" => a.verbose = true,
            "-T" => a.opt.threads = text(value("-T")?).parse().map_err(|_| "-T takes a number")?,
            "-l" => a.level = Some(text(value("-l")?)),
            "--format" => {
                format = Some(match &*text(value("--format")?) {
                    "aqr" => None,
                    f => match aqr_pack::Format::by_format_name(f) {
                        Some(f) => Some(f),
                        None => {
                            let names = aqr_pack::Format::names().collect::<Vec<_>>().join(", ");
                            return Err(format!("not a format to compress into (aqr, {names}): {f}"));
                        }
                    },
                })
            }
            "--depth" => a.opt.depth = text(value("--depth")?).parse().map_err(|_| "--depth takes a number")?,
            "--batch" => {
                let mib: usize = text(value("--batch")?).parse().map_err(|_| "--batch takes a number of MiB")?;
                a.opt.batch = mib.max(1) << 20;
            }
            "-B" => a.opt.batch = size_of(&text(value("-B")?)).ok_or("-B takes a size (64M, 1G, ...)")?.max(1),
            "-b" => a.opt.block = size_of(&text(value("-b")?)).ok_or("-b takes a size (64K, 1M, ...)")?.max(1),
            "-D" => a.opt.dictionaries = true,
            "--no-check" => a.opt.verify = false,
            "--stats" => stats = true,
            "--ipc" => a.ipc = true,
            "-P" | "--passphrase" => a.passphrase = true,
            "--each" => a.each = true,
            "--rename" => renames.push((a.inputs.len(), value("--rename")?.into())),
            "-E" | "--encoding" => a.names.encoding = aqr_unpack::Choice::parse(&text(value("-E")?))?,
            "--normalize" => a.names.normalize = names::Opts::normalize(&text(value("--normalize")?))?,
            "--bad-names" => a.names.bad = names::Opts::bad(&text(value("--bad-names")?))?,
            s if s.starts_with("--stats=") => {
                let to = &arg.as_encoded_bytes()["--stats=".len()..];
                // SAFETY: split after ASCII, of an `OsStr`'s bytes
                let to = PathBuf::from(unsafe { std::ffi::OsStr::from_encoded_bytes_unchecked(to) });
                if !stats::takes(&to) {
                    return Err("--stats= takes a file named .json or .html".into());
                }
                stats_to.push(to);
            }
            // -T8 and the like
            s if s.starts_with("-T") => a.opt.threads = s[2..].parse().map_err(|_| "-T takes a number")?,
            s if s.starts_with("-l") => a.level = Some(s[2..].to_string()),
            // -0 to -9, and whatever level of more digits there may be (one level, not several)
            s if s[1..].bytes().all(|b| b.is_ascii_digit()) => a.level = Some(s[1..].to_string()),
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
    if a.list {
        if a.output.is_some() || !renames.is_empty() || last_dir.is_some() {
            return Err("-L takes archives alone".into());
        }
        return Ok(a);
    }
    if !a.decompress {
        if a.each {
            return Err("--each is for decompressing".into());
        }
        let named = a.output.as_ref().and_then(|o| aqr_pack::Format::by_name(&os_bytes(o.as_os_str())));
        a.format = match format.unwrap_or(named) {
            None => Pack::Aqr,
            Some(f) => {
                let (range, _) = f.levels();
                let level = match &a.level {
                    None => None,
                    Some(l) => match l.parse::<u8>() {
                        Ok(n) if range.contains(&n) => Some(n),
                        _ => {
                            let (lo, hi) = (range.start(), range.end());
                            return Err(format!("not a level of {} ({lo}-{hi}): {l}", f.ext()));
                        }
                    },
                };
                if a.passphrase && !f.encrypts() {
                    return Err(format!("-P: {} archives are not encrypted here (.zip and .7z are)", f.ext()));
                }
                Pack::Other(f, level)
            }
        };
    }
    if a.format == Pack::Aqr
        && let Some(l) = &a.level
    {
        a.opt.level = l.parse()?;
    }
    if !a.decompress {
        if last_dir == Some(a.inputs.len()) {
            return Err(format!("-C {} not before an input", dir.display()));
        }
        a.given = std::mem::take(&mut a.inputs);
        a.inputs = a.given.iter().zip(&dirs).map(|(i, d)| d.join(i)).collect();
        a.renames = vec![None; a.inputs.len()];
        for (n, to) in renames {
            let Some(k) = n.checked_sub(1) else {
                return Err(format!("--rename {} not after an input", to.display()));
            };
            if a.renames[k].replace(to).is_some() {
                return Err(format!("{}: --rename given twice", a.inputs[k].display()));
            }
        }
    } else if !a.each && a.inputs.len() > 1 {
        a.select = Some(selection(a.inputs.split_off(1), renames)?);
        if a.auto_dir {
            return Err("--auto-dir with paths of an archive".into());
        }
    } else if let Some((_, to)) = renames.first() {
        return Err(format!("--rename {} not after a path of an archive", to.display()));
    }
    if a.decompress && a.output.is_some() && a.inputs.len() > 1 && !a.auto_dir {
        return Err("-o with several archives to decompress".into());
    }
    a.dir = dir;
    Ok(a)
}

/// An option's value as text (any of it not Unicode replaced), where it is not a path.
fn text(v: std::ffi::OsString) -> String {
    v.to_string_lossy().into_owned()
}

/// The paths of an archive given, `paths`, with `renames` (each by the number of inputs before
/// it, the archive's too): each path's names (encoded as the tree's), and where it goes.
fn selection(paths: Vec<PathBuf>, renames: Vec<(usize, PathBuf)>) -> Result<extract::Selection, String> {
    let mut out: Vec<(Vec<Vec<u8>>, Option<PathBuf>)> = Vec::new();
    for p in &paths {
        // (on Windows, `\` is `/` too)
        let bytes = os_bytes(p.as_os_str());
        let names: Vec<Vec<u8>> =
            bytes.split(|&b| b == b'/').filter(|n| !n.is_empty() && *n != b".").map(name_from).collect();
        if names.is_empty() || names.iter().any(|n| n == b"..") {
            return Err(format!("{}: not a path in an archive", p.display()));
        }
        out.push((names, None));
    }
    for (n, to) in renames {
        let Some(k) = n.checked_sub(2) else {
            return Err(format!("--rename {} not after a path of an archive", to.display()));
        };
        if out[k].1.replace(to).is_some() {
            return Err(format!("{}: --rename given twice", paths[k].display()));
        }
    }
    for (i, a) in out.iter().enumerate() {
        if let Some(j) = out.iter().enumerate().position(|(j, b)| j != i && a.0.starts_with(&b.0)) {
            return Err(match a.0 == out[j].0 {
                true => format!("{}: given twice", paths[i].display()),
                false => format!("{}: under {}, given too", paths[i].display(), paths[j].display()),
            });
        }
    }
    Ok(extract::Selection { paths: out })
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
    match ipc::get().and_then(|i| i.ask(aqr_ipc::ask::OVERWRITE, q)) {
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
        Some(out) => compress_into(a, &a.inputs, &a.given, &a.renames, out),
        None => {
            for ((input, given), to) in a.inputs.iter().zip(&a.given).zip(&a.renames) {
                if given.as_os_str() == "-" {
                    return Err("standard input is not taken yet".into());
                }
                let out = with_ext(input, &a.format.ext());
                use std::slice::from_ref as one;
                compress_into(a, one(input), one(given), one(to), &out)?;
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

/// Compresses `inputs` (`given` as they were given) into `out`, each in the archive at the path
/// `renames` gives it, if any.
fn compress_into(
    a: &Args,
    inputs: &[PathBuf],
    given: &[PathBuf],
    renames: &[Option<PathBuf>],
    out: &Path,
) -> Result<(), String> {
    if let Pack::Other(f, level) = a.format {
        return pack_into(a, f, level, inputs, given, renames, out);
    }
    let Some(force) = output(a, out)? else { return Ok(()) };
    begin("compress", inputs, Some(out));
    let start = Instant::now();
    let rec = stats::record(a.stats.as_ref());
    let mut opt = a.opt.clone();
    let sealing = pass::keys().sealing();
    opt.spill = spill_of(out, sealing.is_some())?;
    opt.size_hint = sealing.is_some();
    let opt = &opt;
    let meta = |p: &Path| fs::symlink_metadata(p).map_err(|e| format!("{}: {e}", p.display()));
    let mapper = Mapper::new().map_err(|e| format!("files not to be mapped: {e}"))?;
    // a single file: an archive of the file alone (but renamed: in a tree, at its name)
    if let ([input], [None]) = (inputs, renames)
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
            let r = match &sealing {
                None => viewed(a, &view, |tell| compress_formula_to(&d, kind, opt, file, tell))?,
                Some(p) => {
                    let mut s = sealer(file, p)?;
                    let (_, f) = viewed(a, &view, |tell| compress_formula_to(&d, kind, opt, &mut s, tell))?;
                    s.finish()?;
                    (file.metadata()?.len() as usize, f)
                }
            };
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
    // (as long as it takes: a bar of no end known, and how many files)
    let listing = view::Listing::new(bars());
    let gathered = ticking(a, &listing, || -> Result<(), String> {
        gather(inputs, given, renames, &mut g, &listing, a.opt.threads)?;
        listing.finish();
        Ok(())
    });
    listing.finish();
    gathered?;
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
    let r = write_whole(out, force, |file| match &sealing {
        None => {
            let mut w = std::io::BufWriter::with_capacity(8 << 20, file);
            viewed(a, &view, |tell| compress_tree_to(&g.tree, &g, opt, &mut w, tell).and_then(|r| w.flush().map(|_| r)))
        }
        // (the sealer writing a chunk at a time)
        Some(p) => {
            let mut s = sealer(file, p)?;
            let mut r = viewed(a, &view, |tell| compress_tree_to(&g.tree, &g, opt, &mut s, tell))?;
            s.finish()?;
            // (the overhead of the encryption the archive's)
            let len = file.metadata()?.len() as usize;
            (r.overhead, r.len) = (r.overhead + len - r.len, len);
            Ok(r)
        }
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

/// Compresses `inputs` into archive `out` of format `f` at `level` ([`aqr_pack`]), each at the
/// path `renames` gives it; a single one not renamed at its name.
fn pack_into(
    a: &Args,
    f: aqr_pack::Format,
    level: Option<u8>,
    inputs: &[PathBuf],
    given: &[PathBuf],
    renames: &[Option<PathBuf>],
    out: &Path,
) -> Result<(), String> {
    let Some(force) = output(a, out)? else { return Ok(()) };
    begin("compress", inputs, Some(out));
    let start = Instant::now();
    let renames = match (inputs, renames) {
        ([input], [None]) => vec![input.file_name().map(PathBuf::from)],
        _ => renames.to_vec(),
    };
    let mapper = Mapper::new().map_err(|e| format!("files not to be mapped: {e}"))?;
    let mut g = Gathered::new(mapper, a.preserve);
    let listing = view::Listing::new(bars());
    let gathered = ticking(a, &listing, || -> Result<(), String> {
        gather(inputs, given, &renames, &mut g, &listing, a.opt.threads)?;
        listing.finish();
        Ok(())
    });
    listing.finish();
    gathered?;
    if g.tree.entries.is_empty() {
        return Err(format!("nothing to archive in {}", inputs[0].display()));
    }
    if !a.quiet {
        g.skipped.iter().for_each(|s| eprintln!("skipped {s}"));
        g.warnings.iter().for_each(|w| eprintln!("{w}"));
    }
    let passphrase = pass::keys().sealing();
    let o = aqr_pack::Options { level, threads: a.opt.threads, batch: a.opt.batch as u64, passphrase };
    let view = view::View::new(&g, bars(), false);
    let w = write_whole(out, force, |file| {
        let mut w = std::io::BufWriter::with_capacity(8 << 20, file);
        let r = viewed(a, &view, |tell| {
            aqr_pack::write(f, &g.tree, &g, &o, &mut w, &|p| match p {
                aqr_pack::Progress::Started(c) => tell(aqr_compress::Event::Started(c)),
                aqr_pack::Progress::Ended(c) => tell(aqr_compress::Event::Ended(c)),
                aqr_pack::Progress::Done { done, total } => {
                    tell(aqr_compress::Event::Analysis { done, total });
                    tell(aqr_compress::Event::Compression { done, total });
                }
            })
        })?;
        w.flush()?;
        (0..g.contents.len()).try_for_each(|c| aqr_compress::Source::intact(&g, c))?;
        Ok(r)
    })?;
    let files = g.tree.entries.iter().filter(|e| matches!(e.node, EntryNode::File(_))).count();
    if !a.quiet {
        w.notes.iter().for_each(|n| eprintln!("{n}"));
        summary(out, files, w.read as usize, w.len as usize, start);
    }
    end(Some(out), files, w.read, w.len, start);
    Ok(())
}

/// The contents of `Gathered` as aqr-pack reads them.
impl aqr_pack::Contents for Gathered {
    fn size(&self, c: usize) -> u64 {
        self.contents[c].2
    }

    fn read(&self, c: usize) -> std::io::Result<Bytes> {
        aqr_compress::Source::read(self, c)
    }
}

/// An archive written to `file`, encrypted with passphrase `p` ([`aqr::envelope`]): with
/// mirrored headers, at age's work factor.
fn sealer<'f>(file: &'f mut fs::File, p: &[u8]) -> std::io::Result<aqr::envelope::Sealer<&'f mut fs::File>> {
    aqr::envelope::Sealer::new(file, p, aqr::envelope::WORK_FACTOR, true)
}

/// Where the large buffers of compressing into `out` are kept ([`aqr_compress::Options::spill`]):
/// in files beside it (gone once dropped), not on the heap; in the temporary directory where it
/// is on a drive of the network (too slow for them, and Windows there neither holds a file open
/// against deletion nor deletes it on close). None where the archive is `sealed` (encrypted):
/// its contents would be written out as they are (the mapping's pages, still on the disk once
/// the files are gone), as decompressing decrypts it into memory rather than a file. (A spill
/// encrypted with a key of its own was tried: its regions decrypted to the heap whole as they
/// are read, boot.wim took 2.25 GB of peak footprint and 18.9 s, against 2.41 GB and 12.0 s
/// without a spill and 1.20 GB and 11.3 s with one not encrypted.)
fn spill_of(out: &Path, sealed: bool) -> Result<Option<PathBuf>, String> {
    let name = out.file_name().ok_or(format!("{}: not a file's name", out.display()))?;
    if sealed {
        return Ok(None);
    }
    let dir = out.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let mut hidden = std::ffi::OsString::from(".");
    hidden.push(name);
    Ok(Some(if remote(dir) { std::env::temp_dir() } else { dir.to_owned() }.join(hidden)))
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

/// The most threads that read what is found ([`gather`]): more only contend in the file system
/// (on Windows, 402,844 files on an SSD: 11.0 s on 1, 4.2 s on 4, 4.5 s on 8, 4.4 s on 16 and
/// 5.3 s on 32).
const STAT_THREADS: usize = 8;

/// An entry found as the inputs are walked ([`walk`]): its path and name, the one found that
/// holds it (none: at the root), and whether it is a directory as it was listed; `plain`, a
/// directory on the way to an input's path in the archive, of no path and no metadata.
struct Found {
    path: PathBuf,
    name: OsString,
    parent: Option<usize>,
    dir: bool,
    plain: bool,
    /// Of a directory, what was read of it ([`meta::stat`]) before what it holds was listed:
    /// as listing it changes its access time (Linux's `relatime`, where it is a day old).
    stat: Option<std::io::Result<meta::Stat>>,
}

/// The names of the path in the archive of `input`: those `to` gives (`--rename`), exactly;
/// else those of a relative path as it is (`.` left out), and the last one alone of an absolute
/// path or one with `..` (as 7-Zip names them: the frontends give absolute paths).
fn archive_names(input: &Path, to: Option<&Path>) -> Result<Vec<OsString>, String> {
    use std::path::Component;
    if let Some(to) = to {
        let b = os_bytes(to.as_os_str());
        let names: Vec<&[u8]> = b.split(|&c| c == b'/').collect();
        if names.iter().any(|n| n.is_empty() || *n == b"." || *n == b"..") {
            return Err(format!("--rename {}: not a path in an archive", to.display()));
        }
        return Ok(names.into_iter().map(os_from).collect());
    }
    let last = || input.file_name().map(|n| vec![n.to_owned()]);
    let mut names = Vec::new();
    for c in input.components() {
        match c {
            Component::Normal(n) => names.push(n.to_owned()),
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) | Component::ParentDir => {
                names = last().unwrap_or_default();
                break;
            }
        }
    }
    if names.is_empty() {
        return Err(format!("{}: no name to archive it by", input.display()));
    }
    Ok(names)
}

/// Gathers `inputs` (a directory alone and not renamed: its entries at the root; else each at
/// the path [`archive_names`] gives it, as `given`, with `renames`) into `g`: walked first (what the
/// directories hold, a call for each), then what is read of each by its path ([`meta::stat`], a
/// handle or so each, the most of the time) read on `threads`, then made the entries in the
/// order they were found. The files of an inode archived already are of its content (and
/// forks), read once.
fn gather(
    inputs: &[PathBuf],
    given: &[PathBuf],
    renames: &[Option<PathBuf>],
    g: &mut Gathered,
    l: &view::Listing,
    threads: usize,
) -> Result<(), String> {
    let err = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    let privileged = g.reader.privileged;
    let mut found = Vec::new();
    match (inputs, renames) {
        ([dir], [None]) => {
            let s = meta::stat(dir, privileged).map_err(|e| err(dir, e))?;
            let (x, forks) = g.reader.extra(dir, &s, &mut |w| g.warnings.push(w));
            g.tree.root = x;
            for f in forks {
                g.tree.root.forks.push((f.name, g.contents.len()));
                g.contents.push((f.path.clone(), os_bytes(f.path.as_os_str()), f.size));
                l.bytes(f.size);
            }
            walk(dir, None, &mut found, l, privileged)?;
        }
        _ => {
            // (by their names in the archive, the directories found among the inputs or on the way)
            let mut dirs: std::collections::HashMap<Vec<OsString>, usize> = Default::default();
            for ((input, given), to) in inputs.iter().zip(given).zip(renames) {
                let names = archive_names(given, to.as_deref())?;
                let mut parent = None;
                for k in 1..names.len() {
                    let at = dirs.entry(names[..k].to_vec()).or_insert_with(|| {
                        let name = names[k - 1].clone();
                        found.push(Found { path: PathBuf::new(), name, parent, dir: true, plain: true, stat: None });
                        found.len() - 1
                    });
                    parent = Some(*at);
                }
                let t = fs::symlink_metadata(input).map_err(|e| err(input, e))?.file_type();
                // (a directory made on the way to an earlier input, given itself: that one)
                let at = match dirs.get(&names) {
                    Some(&i) if t.is_dir() && found[i].plain => {
                        (found[i].path, found[i].plain) = (input.clone(), false);
                        i
                    }
                    _ => {
                        let name = names.last().unwrap().clone();
                        found.push(Found {
                            path: input.clone(),
                            name,
                            parent,
                            dir: t.is_dir(),
                            plain: false,
                            stat: None,
                        });
                        found.len() - 1
                    }
                };
                if t.is_file() {
                    l.found();
                }
                if t.is_dir() {
                    dirs.entry(names).or_insert(at);
                    walk(input, Some(at), &mut found, l, privileged)?;
                }
            }
            taken_twice(&found)?;
        }
    }
    l.walked();
    let early: Vec<_> = found.iter_mut().map(|f| f.stat.take()).collect();
    let stats = aqr_common::pool::par_map(found.len(), threads.min(STAT_THREADS), |i| {
        if found[i].plain || early[i].is_some() {
            return None;
        }
        let s = meta::stat(&found[i].path, privileged);
        if let Ok(s) = &s
            && s.m.is_file()
        {
            l.read(s.m.len());
        }
        Some(s)
    });
    // (by the entries found, the entry made of each)
    let mut made: Vec<Option<usize>> = Vec::with_capacity(found.len());
    for ((f, s), e) in found.iter().zip(stats).zip(early) {
        let Some(s) = s.or(e) else {
            let parent = f.parent.map(|p| made[p].expect("a directory made"));
            let name = name_from(&os_bytes(&f.name));
            g.tree.entries.push(Entry { parent, name, node: EntryNode::Dir, extra: Default::default() });
            made.push(Some(g.tree.entries.len() - 1));
            continue;
        };
        let s = s.map_err(|e| err(&f.path, e))?;
        if s.m.is_dir() != f.dir {
            return Err(format!("{}: changed as it was listed", f.path.display()));
        }
        // (a directory never skipped)
        let parent = f.parent.map(|p| made[p].expect("a directory made"));
        made.push(entry(&f.path, &f.name, &s, parent, g, l)?);
    }
    Ok(())
}

/// An error where two of `found` would be archived by the same path, telling the inputs (or
/// what was found under them) that are.
fn taken_twice(found: &[Found]) -> Result<(), String> {
    let mut seen: std::collections::HashMap<(Option<usize>, &OsStr), usize> = Default::default();
    for (i, f) in found.iter().enumerate() {
        if let Some(j) = seen.insert((f.parent, &f.name), i) {
            let mut names = vec![f.name.to_string_lossy()];
            let mut at = f.parent;
            while let Some(p) = at {
                names.push(found[p].name.to_string_lossy());
                at = found[p].parent;
            }
            names.reverse();
            let by = |k: usize| match found[k].plain {
                true => "a directory on the way to another input".to_owned(),
                false => found[k].path.display().to_string(),
            };
            return Err(format!("{} and {}: both archived as {}", by(j), by(i), names.join("/")));
        }
    }
    Ok(())
}

/// Finds what directory `dir` (found as `parent`) holds, by name, and what those directories
/// hold in turn, each directory followed by what it holds. On Unix, each directory found is read
/// ([`meta::stat`]) before it is listed.
fn walk(
    dir: &Path,
    parent: Option<usize>,
    found: &mut Vec<Found>,
    l: &view::Listing,
    privileged: bool,
) -> Result<(), String> {
    // (not on Windows, where reading takes longer and listing a directory leaves its access
    // time, NTFS's, mostly as it was: there they are read with the rest, on threads)
    if cfg!(unix)
        && let Some(p) = parent
    {
        found[p].stat = Some(meta::stat(dir, privileged));
    }
    l.dir(dir);
    let err = |e: std::io::Error| format!("{}: {e}", dir.display());
    let mut names: Vec<(OsString, fs::FileType)> = fs::read_dir(dir)
        .map_err(err)?
        .map(|e| e.and_then(|e| Ok((e.file_name(), e.file_type()?))))
        .collect::<Result<_, _>>()
        .map_err(err)?;
    names.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, t) in names {
        let path = dir.join(&name);
        found.push(Found { path, name, parent, dir: t.is_dir(), plain: false, stat: None });
        if t.is_file() {
            l.found();
        }
        if t.is_dir() {
            let at = found.len() - 1;
            let path = found[at].path.clone();
            walk(&path, Some(at), found, l, privileged)?;
        }
    }
    Ok(())
}

/// The entry `name` in `parent` (`None`: the root) of `path`, as `s` read it: its index, none
/// where it is skipped.
fn entry(
    path: &Path,
    name: &OsStr,
    s: &meta::Stat,
    parent: Option<usize>,
    g: &mut Gathered,
    l: &view::Listing,
) -> Result<Option<usize>, String> {
    let m = &s.m;
    let name = name_from(&os_bytes(name));
    let (mut extra, forks) = g.reader.extra(path, s, &mut |w| g.warnings.push(w));
    let t = m.file_type();
    let inode = s.inode;
    let first = inode.and_then(|i| g.inodes.get(&i)).map(|f| f[0]);
    if let Some(first) = first {
        extra.forks = g.tree.entries[first].extra.forks.clone();
    } else {
        for f in forks {
            extra.forks.push((f.name, g.contents.len()));
            g.contents.push((f.path.clone(), os_bytes(f.path.as_os_str()), f.size));
            l.bytes(f.size);
        }
    }
    let node = if t.is_dir() {
        EntryNode::Dir
    } else if let Some(first) = first {
        g.tree.entries[first].node.clone()
    } else if t.is_file() {
        g.contents.push((path.to_owned(), os_bytes(path.as_os_str()), m.len()));
        EntryNode::File(g.contents.len() - 1)
    } else if let Some(node) = meta::special(m) {
        node
    } else if t.is_symlink() {
        let target = meta::link_target(fs::read_link(path).map_err(|e| format!("{}: {e}", path.display()))?);
        let target = name_from(&os_bytes(target.as_os_str()));
        if name_bytes(&target).0.is_empty() {
            g.skipped.push(format!("{} (an empty link)", path.display()));
            return Ok(None);
        }
        EntryNode::Link(target)
    } else {
        g.skipped.push(format!("{} (a socket, or of a kind not known)", path.display()));
        return Ok(None);
    };
    if let Some(i) = inode {
        g.inodes.entry(i).or_default().push(g.tree.entries.len());
    }
    g.tree.entries.push(Entry { parent, name, node, extra });
    Ok(Some(g.tree.entries.len() - 1))
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
    let open = || fs::OpenOptions::new().read(true).write(true).create_new(true).open(&part);
    let (mut file, stop) = stop::create(&part, open).map_err(|e| err(&part, e))?;
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
        // (in the directory of `-C` instead, and `-o` from there)
        let named = match a.dir.as_os_str().is_empty() {
            true => named,
            false => named.map(|n| a.dir.join(n.file_name().unwrap())),
        };
        let output = a.output.as_ref().map(|o| a.dir.join(o));
        let to = match (named, &output, a.auto_dir) {
            (Some(n), Some(o), true) => To { named: o.join(n.file_name().unwrap()), base: o.clone() },
            (Some(n), None, true) => To { base: n.parent().unwrap_or(Path::new("")).to_owned(), named: n },
            (_, Some(o), false) => To { named: o.clone(), base: o.clone() },
            (Some(n), None, false) => To { named: n, base: a.dir.clone() },
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
            // (some paths: into a directory as it is, made where something goes there)
            _ if a.select.is_some() => (extract::Place::Into(if foreign { &self.base } else { &self.named }), a.force),
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
    let mut opened = aqr_unpack::Opened::open_with(format, &c, &a.names.encoding).map_err(|e| err(&e))?;
    tell_names(a, input, opened.names_told());
    if a.select.is_some() && !opened.is_tree() {
        return Err(err(&"a single file, not an archive of paths"));
    }
    // (its entries encrypted without a passphrase taken are left out, told)
    if opened.encrypted()
        && pass::keys().open(input, |p| opened.unlock(p).map(|ok| ok.then_some(())).map_err(|e| err(&e)))?.is_none()
        && opened.sealed()
    {
        return Err(err(&pass::keys().none_opens()));
    }
    // (some paths: checked where the listing is known, else once the archive is read)
    if let Some(s) = &a.select {
        if let Some(t) = opened.tree() {
            s.check(t, &|k| opened.left_out(&s.unpacking(Some(k)))).map_err(|e| err(&e))?;
        }
        opened.select(s.unpacking(None));
    }
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
                Some(p) => Some(extract::Extractor::new(p, force, &ap, &seen, a.select.clone(), &a.names)?),
            };
            let sink =
                Unpacking { x: x.as_ref(), input, quiet: a.quiet, view: shown, seen: &seen, sizes: Default::default() };
            let done = match opened.unpack(threads, &sink) {
                Err(_) if x.as_ref().is_some_and(|x| x.skipped()) => return Ok(None),
                r => r.map_err(|e| err(&e))?,
            };
            // (of a tar archive; where entries were left out, told, and the error at the end)
            if let Some(s) = &a.select
                && done.skipped == 0
            {
                s.check(&done.tree, &|_| Vec::new()).map_err(|e| err(&e))?;
            }
            let picked = a.select.as_ref().map(|s| s.picked(&done.tree));
            let sizes = sink.sizes.into_inner().unwrap();
            if let Some(x) = &x {
                tell_names(a, input, x.names_told());
            }
            let (n, total, out) = match x {
                Some(x) => match x.finish(&done.tree)? {
                    Some((n, t, o)) => (n, t, Some(o)),
                    None => return Ok(None),
                },
                None => done.tree.entries.iter().enumerate().fold((0, 0, None), |(n, t, o), (i, e)| match e.node {
                    EntryNode::File(c) if picked.as_ref().is_none_or(|p| p[i]) => {
                        (n + 1, t + sizes.get(c).copied().unwrap_or(0), o)
                    }
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
    // an encrypted one decrypted into memory (not backed by a file, so that nothing decrypted
    // is written out but the output)
    let decrypted = match aqr::envelope::is_encrypted(&c) {
        true => Some(decrypt(&c, input, a.opt.threads)?),
        false => None,
    };
    let decoder =
        Decoder { native: Native { threads: a.opt.threads }, wasm: a.wasm, sandbox: a.sandbox.then(OnceLock::new) };
    let runner: &dyn aqr::Runner = &decoder;
    let mut archive = match &decrypted {
        Some(d) => aqr::open_payload(d, runner),
        None => aqr::open(&c, runner),
    }
    .map_err(|e| err(&e))?;
    // (some paths: by content, whether it is decoded)
    let wanted = match (&a.select, archive.tree()) {
        (None, _) => None,
        (Some(_), None) => return Err(err(&"an archive of a single file, not of paths")),
        (Some(s), Some(t)) => {
            s.check(t, &|_| Vec::new()).map_err(|e| err(&e))?;
            Some(s.contents(t, archive.contents_len()))
        }
    };
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
            let picked = a.select.as_ref().map(|s| s.picked(t));
            let files = t.entries.iter().enumerate().fold(vec![0; archive.contents_len()], |mut f, (i, e)| {
                if let EntryNode::File(c) = e.node
                    && picked.as_ref().is_none_or(|p| p[i])
                {
                    f[c] += 1;
                }
                f
            });
            ticking(a, &*view, || {
                archive.contents_of(a.opt.threads, wanted.as_deref(), |c, d| {
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
                let x = extract::Extractor::new(place.unwrap(), force, &ap, &seen, a.select.clone(), &a.names)?;
                match x.entries(t, &(0..t.entries.len()).collect::<Vec<_>>()) {
                    Err(_) if x.skipped() => return Ok(None),
                    r => r?,
                }
                archive
                    .contents_of(a.opt.threads, wanted.as_deref(), |c, d| x.content(c, d).map_err(Failure::Resource))
                    .map_err(|e| e.to_string())?;
                tell_names(a, input, x.names_told());
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

/// The payload of encrypted archive `c` (of `input`), decrypted on `threads` threads, by a
/// passphrase given ([`pass`]).
fn decrypt(c: &[u8], input: &Path, threads: usize) -> Result<memmap2::Mmap, String> {
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", input.display());
    let e = aqr::envelope::open(c).map_err(|e| err(&e))?;
    let unlocked = pass::keys().open(input, |p| match e.unlock(p) {
        Ok(u) => Ok(Some(u)),
        Err(Failure::Decryption(_)) => Ok(None),
        Err(f) => Err(err(&f)),
    })?;
    let Some(u) = unlocked else {
        return Err(err(&pass::keys().none_opens()));
    };
    let mut d = memmap2::MmapMut::map_anon(u.len()).map_err(|e| err(&e))?;
    u.decrypt_into(&mut d, threads).map_err(|e| err(&e))?;
    d.make_read_only().map_err(|e| err(&e))
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
/// What was told of the names of archive `input` ([`names`]), on standard error (but with `-q`).
fn tell_names(a: &Args, input: &Path, told: impl IntoIterator<Item = String>) {
    if !a.quiet {
        told.into_iter().for_each(|t| eprintln!("{}: {t}", input.display()));
    }
}

fn summary_d(input: &Path, out: Option<&Path>, files: usize, total: u64, start: Instant) {
    let t = start.elapsed().as_secs_f64();
    let to = match out {
        None => " (tested)".into(),
        Some(o) if o.as_os_str().is_empty() => " -> .".into(),
        Some(o) => format!(" -> {}", o.display()),
    };
    eprintln!(
        "{}{to}: {files} file{}, {} in {t:.2} s, {:.1} MB/s",
        input.display(),
        if files == 1 { "" } else { "s" },
        size(total as f64),
        total as f64 / 1e6 / t.max(1e-9)
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An archive is compressed with a spill beside it, but an encrypted one with none: nothing
    /// of its contents written out as it is.
    #[test]
    fn no_spill_sealed() {
        let out = std::env::temp_dir().join("x.aqr");
        assert!(spill_of(&out, false).unwrap().is_some_and(|p| p.ends_with(".x.aqr")));
        assert_eq!(spill_of(&out, true), Ok(None));
    }
}
