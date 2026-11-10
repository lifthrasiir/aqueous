//! Compresses files and directories into Aqueous archives, and decompresses them, as zstd does.
//!
//! usage: aqr [-c | -d | -K | -t] [-o OUTPUT] [-L] [-f] [--rm] [-p] [-q | -v] [-T THREADS] [-l LEVEL | -0 ... -9] [--depth N]
//! [-B SIZE] [-b SIZE] [-S SIZE] [-D] [-A] [--no-check] [-P] [--wasm] [--no-sandbox] [--stats[=FILE]] [-h]
//! INPUT [--rename TO]...
//!
//! aqr -d \[OPTIONS\] ARCHIVE [PATH [--rename TO]]...; aqr -d --each \[OPTIONS\] ARCHIVE...; aqr -t [-q | -v] ARCHIVE \[PATH\]...;
//! aqr -t --each [-q | -v] ARCHIVE...; aqr --delete [--compact] [-q] ARCHIVE PATH...;
//! aqr --move [-q | -v] ARCHIVE PATH --rename TO...; aqr --compact [-q | -v] ARCHIVE
//!
//! - `-c` (the default; `aqr`): each input into `INPUT.aqr`, a file as an archive of a single
//!   file, a directory as an archive of what it holds; with `-o`, all the inputs into one
//!   archive (several of them, or a directory, as entries of their names). Several inputs, or
//!   one renamed, are archived by their paths: a relative one as it is given (`.` left out,
//!   the directories on the way made plainly, without their metadata), an absolute one or one
//!   with `..` by its last name alone, as 7-Zip does (tar, zip and RAR would keep an absolute
//!   path whole, but the frontends give absolute paths); `--rename TO` after an input has it at
//!   `TO` (`/` between names) instead. Two inputs of the same path are an error.
//! - `-x NAME` (`--exclude`): compressing, the entries of `NAME` left out wherever they are
//!   found (an input given too), or of a path in the archive where it has a `/` (but at its
//!   end), with what is under them ([`Exclude`]); as given, no wildcards. `--files-from FILE`
//!   takes the inputs (decompressing, the paths of the archive) listed in `FILE` (`-`: standard
//!   input) where it is given, a line each, or ended by NUL each after `--null`.
//! - `-L` (`--dereference`), as bsdtar's: compressing, the links given and found followed, each
//!   archived (at its own name) as what it leads to, read there; a link leading nowhere, or to a
//!   directory it is in, kept as a link and told. Without it a link is archived as a link, one
//!   given alone too. Not with `--rm` (what a link leads to is not an input to remove).
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
//!   Files are written sparse ([`sparse`]): runs of zeros of 256 KiB or more (of whole 64 KiB
//!   at offsets of a multiple of it) are left as holes where the file system has them.
//! - `-d ARCHIVE PATH...` (and `-K`): the paths of the archive given alone (`/` between names;
//!   a directory with what is under it), where the whole archive would have them, with the
//!   directories on the way (made plainly, without their metadata); `--rename TO` after a path
//!   has that one at `TO` instead (with what is under it; the directories before `TO` made).
//!   Only what they need is decoded ([`aqr::Archive::contents_of`], [`aqr_unpack::Select`]). A
//!   path not in the archive is an error, before anything is extracted where its listing tells
//!   (but in a tar archive, after); so is one under another.
//! - `-` (an input) is standard input, `-o -` standard output ([`stdio`]). Compressing, `-` is
//!   the only input, read whole before the work begins, an archive of a single file; written
//!   to standard output where no `-o` is given (never to a terminal, but with `-f`). An archive
//!   of several files is written to standard output as it is made (never committed: stopped,
//!   what was written is cut short); an archive of a single file, encrypted or of another
//!   format, made whole first. Decompressing, `-` is an Aqueous archive (read whole first, for
//!   now), extracted to standard output where no `-o` is given; standard output takes the file
//!   of an archive of a single file, or the one file of the paths given (`-d ARCHIVE PATH -o
//!   -`). No input given, and standard input not a terminal, it is the input (but listing).
//! - `-C DIR` (`--directory`), as tar has it: compressing, the inputs after it found in `DIR`
//!   (each `-C` from the one before) and archived as if this were run there, `INPUT.aqr`
//!   beside them; decompressing (given anywhere), what is extracted goes into `DIR` in place of
//!   the current directory or beside the archive, `-o` from there too. The archives (`-o`
//!   compressing, the inputs decompressing) are where they are given, as tar's `-f`.
//! - `--each`: decompressing (or listing), every input an archive, each extracted (or listed)
//!   whole (as the frontends extract a selection of archives).
//! - `--auto-dir` (as the frontends extract): a single entry at the top of an archive (of
//!   either kind) is extracted beside it as it is, where nothing is there of its name; else
//!   the archive is extracted into a directory of its name without its extension, beside it
//!   (`foo.zip` of `foo/` into `foo/`, never `foo/foo/`). `-o` is where it goes instead of
//!   beside it, a directory (with several archives too).
//! - `-K` (`--test`): each archive decoded as `-d` does, but nothing written (its checksums checked); and
//!   where it has mirror blocks (or with `-v`), told how far it conforms to the archival profile
//!   and how many of its blocks are mirrored ([`tell_archival`]).
//! - `-A` (`--archival`): compressing, an archive for long-term preservation: of the archival
//!   profile (as every archive is made: each file's content checksummed, nothing outside it), with
//!   a mirror block of each block whose damage would lose more than a file ([`aqr::Mirroring`]),
//!   the last metadata block's before the last 4 MiB of the archive (held back as it is made),
//!   so that a truncation shorter than that leaves it. Decompressing, an archive of a tree is
//!   read by its index ([`aqr::open_indexed`]): of the files taken alone (`-d ARCHIVE PATH...`),
//!   only their blocks and what tells where they are, the metadata blocks back from the last as
//!   far as they tell the paths ([`aqr::open_indexed_at`]), an encrypted archive's chunks
//!   decrypted as they are read ([`aqr::envelope::Decrypting`]). Where it does not open so, and for the files
//!   that do not decode so, and testing, it is read whole ([`Repair`]) and repaired in memory
//!   ([`aqr::mirror::repair`]): a damaged block restored from its mirror
//!   block, the footer made again where the archive is cut short after its last metadata block;
//!   told so. Cut short before it (its metadata block restored past the bytes lost), it is
//!   salvaged ([`aqr::open_salvaging`]): the files of the blocks lost are lost. Cut short in a
//!   segment whose metadata block is not there, that segment is cut off, the run failing; its
//!   leaf blocks still whole are evaluated each alone, their files of no name written into
//!   `#salvaged` (`leaf-N`) beside the tree extracted whole ([`salvage_unnamed`]). An archive
//!   whose footer is made again after a segment not meant to be the last fails the run too.
//! - A file whose evaluation fails (its checksum not verifying, say) is lost, told on a line of
//!   its own; the others are extracted (or tested), and the run fails at the end. A file covered
//!   by no checksum is given as it decodes.
//! - `-t` (`--list`): the entries of each archive listed on standard output, and what each file
//!   takes of the archive and how it is made ([`list`]); `-q` the paths alone, `-v` all that is
//!   told. `-t ARCHIVE PATH...`: the entries at those paths alone, with what is under them, as
//!   tar's `-t` has them; `--each` lists several archives whole. `--json`: a line of JSON for
//!   each archive instead, its entries with all their metadata ([`list`]).
//! - `-I` (`--info`): all that an archive records, for debugging ([`info`]): of the archive
//!   itself (its blocks and segments; of a single file, its formula too), or with paths, each
//!   path's entry in every segment (those before an update too), its metadata and formula in full.
//! - `--delete ARCHIVE PATH...`: the paths of the archive given (a directory with what is under
//!   it) taken out of it in place, by a segment of tombstones appended and committed as `-a`
//!   commits ([`edit`]); their bytes stay in the file. A path not in it is an error, before
//!   anything is written.
//! - `--compact ARCHIVE`: the archive written again of what it holds, segment by segment, the
//!   blocks it still needs copied as they are, those solid with what is gone made again of what
//!   is left ([`aqr::rewrite`]): what appends replaced or took away, and an append not
//!   committed, out of the file. Written beside it and checked, then
//!   put in its place by a rename. With `--delete`, of the tree without its paths (no
//!   tombstones). Not of an archive of mirror blocks (`-A`) for now. Of an archive with journal
//!   points, `--keep-since SPEC` keeps those from the one it picks on (the segments before it
//!   written as one state, the others as they are), `--drop-history` none ([`aqr::History`]).
//! - `--move ARCHIVE PATH --rename TO...`: each path moved to its `TO` (in the order given, the
//!   directories on the way made, without their metadata), the archive written again as
//!   `--compact` writes it, as a segment cannot take the contents of another but by an import
//!   (not read yet). `TO` there already, under the path or under what is not a directory, is
//!   an error, before anything is written.
//! - `-P`: compressing, the archives encrypted with a passphrase ([`aqr::envelope`]), asked for
//!   as the work begins (twice on a terminal); decompressing, asked for then rather than as an
//!   archive needs it. An encrypted archive (of either kind) is opened by the passphrases that
//!   opened those before it, else asks for one where there is a terminal or a frontend
//!   ([`pass`]). Without a terminal, `-P` takes a line of standard input. `--passphrase-from`
//!   (implying `-P`) takes it from a line of standard input (`stdin`), of a file descriptor
//!   (`fd:N`) or of a file (any other path), or an environment variable's value (`env:NAME`),
//!   nothing asked for (decompressing, the archives it does not open fail). An empty passphrase
//!   is a passphrase too.
//! - `-E`, `--normalize`, `--bad-names`: decompressing, how the archives' names are written
//!   ([`names`], `docs/names-design.md`): names of no declared encoding decoded from the set
//!   given, guessed (`auto`), or kept as bytes (`raw`); Unicode names composed (`auto`: but on
//!   macOS), `nfc`, `nfd` or kept; and names this system cannot write as they are kept as bytes
//!   where it takes them, replaced (`%XX`, U+FFFD, `_`), or failed. What was guessed or done is
//!   told.
//! - `--links`: decompressing, which links are made ([`extract::Links`]): by default those
//!   whose targets stay in the directory extracted into, through the archive's own links, the
//!   others told and not made (spec, "Extraction"); with `all`, every one as it is, those
//!   reaching out of it, going round in a loop of links or through names a file system may
//!   take as a link of the archive or not.
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
//!   the access, status change and backup times (the access times of the inputs put back once
//!   archived, which changes their status change times where they were changed by reading
//!   them); in decompressing, the ownership, the flags only the superuser can change, the
//!   set-user-ID and set-group-ID bits, FIFOs and devices, and the backup times. An archive of
//!   a single file has no metadata. On Linux, the inputs are read without updating their access
//!   times where this process may (`O_NOATIME`: files it owns).
//! - `-f` overwrites outputs. Extracting, what is where an entry goes is taken away for it
//!   (a link itself, never followed), a directory with what is under it only where it is in
//!   the directory extracted into and holds neither the archive nor the current directory
//!   ([`extract`]). `--rm` removes the inputs once their archive is made (and checked,
//!   but with `--no-check`), or an archive once it is extracted whole, as zstd's `--rm` does:
//!   compressing, what was archived of them (the directories last, where left empty; what was
//!   not archived, or what changed or was written in their place since it was read, is kept
//!   and told, and the run fails at the end); an input skipped (`--ipc`) is kept. With `--ipc`, the frontend is asked
//!   instead where an output exists ([`ipc`]), and skips it or has it overwritten. `-q` logs nothing but errors; `-v` logs
//!   each file archived too (below). An archive of a single file (and one with `--atomic`), or a
//!   single file decompressed, is written as `OUTPUT.PID.part` and renamed once done: removed
//!   where it fails or the process is stopped ([`stop`]), an output there before left as it was.
//! - An archive of several files is committed as it is written ([`stop`]; spec, "Appending to an
//!   archive"): written as `OUTPUT.PID.part`, it is renamed once a segment of it is committed
//!   (`-S`), and later commits are made in place, so that the file holds an archive of what was
//!   committed wherever the process is stopped (even killed), telling what it left out
//!   ([`aqr::Status`]). Ctrl-C (`SIGTERM`, `SIGHUP`, a frontend's `finish`) ends it as it is, of
//!   the files written, telling so, and the run fails (exit code 130); a second Ctrl-C stops it at
//!   once, the archive of its last commit left (nothing, where nothing was committed). `-f` over
//!   an archive there replaces it at the first commit. `--atomic` makes it whole or not at all:
//!   kept as `.part` until it is, removed where it is stopped. An archive encrypted (`-P`) or
//!   with mirror blocks (`-A`) is not committed as it is written, but ended as it is all the same
//!   where it is stopped once (but by being killed). An archive of a single file is always made
//!   whole or not at all.
//! - `-B` (`--batch`) is about the most bytes of files made at once (512M by default; `K`, `M`
//!   and `G` are KiB, MiB and GiB, a number alone bytes), which bounds the memory taken by
//!   compression (but for a file larger). `-b` is the most bytes of a block that files share (16M by default), which a file
//!   read alone may have to decompress: smaller for faster random access. `-S` is about the
//!   least bytes of a segment of an archive of several files (256M by default; 0, one segment):
//!   each closed by its metadata block after the batch that makes it so long, so that an archive
//!   cut short or damaged loses no more than a segment's names, each segment carrying its own
//!   modules and dictionaries (a file of the same content as one of a segment before stored
//!   again). `-D` compresses the blocks alike of a batch (many of a class of stream, of
//!   a few MiB each or less) with a dictionary trained for them where it pays.
//! - `-T` is the number of threads (0, the default: as many as the machine has): the size of
//!   the pool of threads that everything runs on ([`aqr_common::pool`]), this work itself too.
//! - `-M` (`--memory`) is about the most heap the work takes at once ([`budget`]): the leaves
//!   of an archive decoded (and of a batch decoded to check it), the batches of files made and
//!   the match finders of large LZMA dictionaries are let in as they fit in it beside the heap in
//!   use (its blocks of 64 KiB or more, counted always) or what those in have taken where more,
//!   but one of each at least. A size, `N%` of the system's memory, `avail` (most of what is
//!   available as it starts), or 0 (no limit); 2 GB by default, at most half the system's. The
//!   pages of files mapped (the inputs, the spill) are not counted: the system writes them out
//!   or drops them, rather than swapping, as memory runs short.
//!   A `-M` given also bounds the memories of an application of a module in WebAssembly (the
//!   instance's limit and, of each class of tasks, its concurrency times its task limit): a
//!   decoder lowers the concurrency of the classes to fit, and an application that does not
//!   fit with one unit each fails as a resource failure before it starts.
//! - `--application-bound SIZE` is about the most memory an application of a module may need at
//!   its least (its memory limit plus the sum of its task limits, `aqr -I -v` tells them), apart
//!   from `-M`, the budget of the whole run. Compressing (256 MiB by default;
//!   `unlimited` allowed), a recompressor whose formula has an application over it is not used for that data, which
//!   is kept as it would be if no recompressor took it (`-v` and `--stats` tell what fell back);
//!   decompressing (512 MiB by default, which the spec has every decoder support), an archive
//!   with an application over it fails as a resource failure before the module is applied.
//! - `--stats` tells, after each archive made or decoded, what stands out of its files by kind
//!   and of where the time went, sampled as it ran ([`stats`]); `--stats=FILE.json` writes all
//!   of it as well, and `--stats=FILE.html` a page showing it (opened on a terminal). Either
//!   may be given more than once; the files are written again every 10 seconds as it runs and
//!   as it is stopped (their `state`). The sampling slows the work a little. Given alone (no
//!   input nor other option), `--stats=FILE.html` writes the page with no data, to open files
//!   of the JSON in.
//!
//! Compressing, standard error has the summary of the archive at the end, and on a terminal a
//! bar of how far it is ([`view`]) until then. With `-v`, a log above the bar tells, for each
//! file, what it was taken for, the modules its formula applies and the bytes of the archive
//! that are its; the files stored together in a block (solid) or coded against each other
//! (sets); the files of the same content as another. It is written a batch at a time, as the
//! files are. Decompressing (and testing), there is a bar too, of the blocks decompressed and
//! the files evaluated ([`view::Decoding`]).
//!
//! A run that fails exits by what failed ([`exit`]): 1 but where the archive itself tells
//! (invalid 3, not decrypted 4, more than allowed 5, an extension not known 6, as the spec's
//! failures are), 7 where it is done but for what it told it left out, 2 of the arguments, 130
//! stopped.
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

mod append;
mod complete;
mod edit;
mod exit;
mod extract;
mod fold;
mod info;
mod ipc;
mod journal;
mod list;
mod meta;
mod names;
mod pass;
mod sparse;
mod stats;
mod stdio;
mod stop;
mod view;

use aqr_compress::mapped::Mapper;
use aqr_compress::{Options, compress_formula_to, detect, kind_by_name};
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
       aqr -t [-q | -v] ARCHIVE [PATH]...
       aqr -t --each [-q | -v] ARCHIVE...
       aqr -I [-v] ARCHIVE [PATH]...
       aqr --delete [--compact] [-q] ARCHIVE PATH...
       aqr --move [-q | -v] ARCHIVE PATH --rename TO...
       aqr --compact [-q | -v] [--keep-since SPEC | --drop-history] ARCHIVE
       unaqr [OPTIONS] ARCHIVE [PATH [--rename TO]]...

Modes:
  -c, --compress        each input into INPUT.aqr (the default as aqr)
  -d, --decompress      an archive into its name without .aqr (the default as unaqr);
                        .zip, .7z, .rar, .alz, .egg and .tar (.gz .tgz .bz2 .xz
                        .lzma .zst) too, as they are into the current directory
                        (or -o); .gz, .bz2, .xz, .lzma and .zst into the name
                        without them
  -t, --list            list the entries of an archive, or those of its PATHs (-q: the
                        paths alone; -v: the blocks each file takes, its formula and
                        all its metadata; --json: as a line of JSON each archive)
  -K, --test            decode and check an archive, writing nothing
  -I, --info            all that an archive records (its blocks and segments), or of
                        its PATHs in each segment (their formulas in full); -v: the
                        unknown metadata uncut
      --delete          take the PATHs of an archive (and what is under them) out of
                        it, appending to it what says so
      --move            move each PATH of an archive to the TO of its --rename (in order,
                        the directories on the way made), writing it again as --compact
      --compact         write an archive again of what it holds (with --delete, of what
                        is left), what was taken out or replaced out of the file
      --keep-since SPEC with --compact, of an archive with journal points (-j): keep
                        those from the one SPEC picks (as --at) on, with their states;
                        what was before is written as one state
      --drop-history    with --compact: keep no point, and none of the states between

Compressing (aqr -o ARCHIVE INPUT [--rename TO]...):
  INPUT                 a file or directory; several of them archived by their paths,
                        a relative one as given, an absolute one or one with .. by
                        its last name
      --rename TO       after an INPUT: that one archived at TO (names between /)
  -x, --exclude NAME    leave out the entries of NAME wherever they are, or of a path
                        in the archive where it has a / (with what is under them); as
                        given, no wildcards
      --files-from FILE the inputs (or decompressing, PATHs) listed in FILE (- standard
                        input), a line each
      --null            the lists of --files-from after it ended by NUL each, not lines
  -L, --dereference     follow links (given or found), archiving what they lead to;
                        those leading nowhere or to a directory they are in kept
      --format FORMAT   aqr (the default), zip, 7z, alz, egg, tar, tar.gz, tar.bz2,
                        tar.xz, tar.lzma, tar.zst, or gz, bz2, xz, lzma, zst of a
                        single file (also by -o's name); -l is the format's level
                        (zlib's 0-9; xz, lzma, 7z: 0-9; bz2: 1-9; zst: 1-22)

Decompressing some paths (aqr -d ARCHIVE PATH [--rename TO]...):
  PATH                  a path in the archive (a directory with what is under it),
                        extracted where the whole archive would have it
      --rename TO       after a PATH: that one extracted at TO instead (--move: moved)
      --each            every input an archive, each extracted (or listed) whole

Output:
  -o, --output OUTPUT   write to OUTPUT; compressing, all the inputs into one archive;
                        -o - is standard output (compressing, the archive; decompressing,
                        the file of a single file's archive or of the one PATH given), and
                        an INPUT - standard input (alone; its output standard output
                        unless -o), the input where none is given
  -C, --directory DIR   compressing, the inputs after it found in DIR, archived as if
                        run there; decompressing, extract into DIR (-o in it too);
                        the archives' own paths are taken from where aqr is run
      --auto-dir        decompressing, a single entry at an archive's top beside it,
                        else a directory of its name without its extension beside it
                        (-o: in that directory instead of beside it)
  -f, --force           overwrite outputs
      --rm              remove the inputs once archived, or archives once extracted
                        (what changed meanwhile kept, as directories not left empty)
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
      --passphrase-from SOURCE
                        -P, its passphrase a line of standard input (stdin), of a file
                        descriptor (fd:N) or of a file (a path; ./env:x for env:x), or
                        an environment variable's value (env:NAME); nothing asked
  -l, --level LEVEL, -0 ... -9
                        0 (uncompressed), 1 (zstd alone) to 9; low, mid (default), high
                        are 2, 5 and 8; zstdN is zstd level N with recompression as mid
      --depth N         recompress formats nested at most N deep (default 3)
  -B, --batch SIZE      make about SIZE of files at once (default 512M)
  -b, --block-size SIZE the most bytes of a block that files share (default 16M)
  -S, --segment-size SIZE
                        close a segment of the archive after about SIZE bytes of it
                        (default 256M; 0, one segment)
  -D, --dictionaries    compress blocks alike with dictionaries trained for them
  -A, --archival        for long-term preservation: mirror the blocks whose damage
                        would lose more than a file, to be restored from as it is read
      --no-check        do not decode the archive made to check it
      --atomic          make the archive whole or not at all: stopped (Ctrl-C), nothing
                        kept, rather than what was written ended as an archive
  -a, --append          append the inputs to an archive (the first input, or -o): what
                        is not in it, or changed (by size and modification time)
  -s, --sync            as -a, and take away what the archive holds that the inputs do
                        not: it then holds what -c of them would (the file still grows)
  -j, --journal LABEL   with -c, -a or -s: name the state the archive is left in as a
                        point to return to (--at), also where nothing is changed;
                        {today} and {now} in LABEL are the date and the date and time

Decompression:
      --at SPEC         with -t, -d or -K: the archive as it was at a journal point:
                        #N (from 1), #-N (from the last), @LABEL (the last of it), or
                        a date, to its end: 2026, 2026-11, 2026-11-01, 2026-11-01T09:30
      --history         list the journal points of the archives
      --diff            list what differs between two states of an archive, a line
                        each: + added, ~ changed (size or metadata), - removed. By
                        default the last point against the archive now; with --at, that
                        point against the one before it; --from SPEC picks the earlier
                        state (--at's choices) for either. With PATHs after the
                        archive (taken as -a takes its inputs), the archive (as --at
                        picks it) against them: + only in them, - only in the archive
  -E, --encoding ENC    names of no declared encoding: auto (the default: as bytes where
                        this system takes them, else decoded from a set guessed), raw
                        (as bytes), system, or sets (shift_jis, euc-kr, gbk, big5,
                        cp437, ...; several: guessed among them)
      --normalize FORM  auto (the default: composed, but on macOS), nfc, nfd or keep
      --bad-names WHAT  names not written as they are: auto (the default), keep (as
                        bytes, where this system takes them), replace (%XX, U+FFFD,
                        _ for what Windows does not take), or fail
      --links WHAT      links made: inside (the default: those whose targets stay in
                        the directory extracted into; the others told), or all (as
                        they are, those reaching out or going round in loops too)
      --wasm            run every module as WebAssembly, the known ones too
      --no-sandbox      run the Wasm modules (the ones not known, or all with --wasm)
                        in this process, not in workers locked down by the system;
                        where the system cannot lock workers down, they run only so

General:
  -T, --threads N       threads to use (default 0: as many as the machine has)
  -M, --memory MEM      about the most heap to take at once: a SIZE, N% of the
                        system's memory, avail (most of what is available now), or 0
                        (no limit); by default 2 GB, or half the system's if less
      --application-bound SIZE
                        the most memory an application of a module may need (its
                        memory limit and the sum of its task limits): compressing,
                        recompressions needing more are not used (default 256M, or
                        unlimited); decompressing, an archive needing more fails
                        (default 512M, which every decoder supports); independent of -M
  -h, --help            show this help
  -V, --version         show the version
      --setup-autocomplete[=SHELL]
                        alone: set up the completion of aqr's options for SHELL (zsh,
                        bash, fish, powershell; by default $SHELL's, PowerShell on
                        Windows): its script written, a line reading it added to the
                        shell's startup file

Exit status: 0 done; 1 failed otherwise; 2 the arguments wrong; 3 an archive invalid
(damaged, files lost to it); 4 not decrypted (no passphrase opens it); 5 needing more than
supported or allowed (-M, --application-bound); 6 an unknown critical extension; 7 done but
for what was told left out (not extracted, not removed); 130 stopped.

SIZE is a number of bytes, or of KiB, MiB or GiB with K, M or G. Short options may be
bundled (-cf, -T8, -oOUT); a long option's value may follow it after = (--memory=2G).
";

/// The argument (the only one) that this program is run with as a worker of the sandbox.
const SANDBOX_WORKER: &str = "--sandbox-worker";

struct Args {
    decompress: bool,
    /// Decompressing, but nothing written.
    test: bool,
    /// Listing the archives' entries (`-t`).
    list: bool,
    /// Telling all that an archive records (`-I`).
    info: bool,
    wasm: bool,
    /// The modules not known (all, with `wasm`) run in a sandbox.
    sandbox: bool,
    output: Option<PathBuf>,
    /// Decompressing, a single entry at the top beside the archive, else a directory of its name.
    auto_dir: bool,
    force: bool,
    /// `--rm`: the inputs removed once their archive is made, or an archive once extracted.
    rm: bool,
    /// `--atomic`: compressing, an archive made whole or not at all, rather than committed as it
    /// is written ([`stop`]).
    atomic: bool,
    /// `-j LABEL`: compressing, the state the archive is left in named as a journal point
    /// ([`journal`]), the label as given (`opt.journal` has it with its placeholders filled in).
    journal: Option<String>,
    /// `--at SPEC`: listing, testing or decompressing, the archive as it was at a journal point
    /// ([`journal`]).
    at: Option<journal::At>,
    /// `--history`: the journal points of the archives listed.
    history: bool,
    /// `--diff`: what differs between two states of the archives (`--from` and `--at` pick them).
    diff: bool,
    from: Option<journal::At>,
    /// `--keep-since SPEC`: `--compact` keeping the journal points from the one `SPEC` picks on
    /// (with the states between); `--drop-history`: dropping them all, with those states.
    keep_since: Option<journal::At>,
    drop_history: bool,
    /// `-a`: compressing, the inputs appended to an archive ([`append`]), the first input or `-o`.
    append: bool,
    /// `-s` (with `append`): what the archive holds that the inputs do not taken away too.
    sync: bool,
    /// Editing the archive (the first input) in place ([`edit`]), and writing it again of what
    /// it holds (`--compact`).
    edit: Option<edit::Edit>,
    compact: bool,
    /// `-L`: compressing, the links found (and given) followed, archived as what they lead to.
    dereference: bool,
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
    stats: Option<&'static stats::Stats>,
    /// Driven by a frontend ([`ipc`]).
    ipc: bool,
    /// Listing as JSON (`--json`).
    json: bool,
    /// Compressing, into archives of this format (`--format`, or `-o`'s name).
    format: Pack,
    /// `-l` as given (read as the format's level: [`Args::format`]'s, or Aqueous's).
    level: Option<String>,
    /// `-P`: compressing, the archives encrypted with a passphrase asked for; decompressing,
    /// the passphrase asked for before the archives need it ([`pass`]).
    passphrase: bool,
    /// Compressing, what is left out of the inputs (`-x`).
    exclude: Exclude,
    /// `--passphrase-from`: where `-P`'s passphrase is taken (implying it).
    pass_from: Option<pass::Source>,
    /// `--each`: decompressing or listing, every input an archive.
    each: bool,
    /// Decompressing (or listing) some paths of the archive: each path (names as the tree has them), and
    /// where it goes where `--rename` tells.
    select: Option<extract::Selection>,
    /// Decompressing, how names are written (`-E`, `--normalize`, `--bad-names`).
    names: names::Opts,
    /// Decompressing, which links are made (`--links`).
    links: extract::Links,
    /// The input is standard input (`-`, alone).
    stdin: bool,
    /// The output is standard output (`-o -`; or by default, of standard input): compressing,
    /// the archive; decompressing, the file of an archive of a single file, or the one of its
    /// paths given.
    stdout: bool,
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
    if let Err(e) = pass::init(args.passphrase, !args.decompress && !args.list && !args.info, args.pass_from.as_ref()) {
        eprintln!("aqr: {e}");
        ipc::tell(serde_json::json!({"event": "error", "message": e}));
        // (none given: none to open an archive with)
        return ExitCode::from(exit::Code::Decryption as u8);
    }
    // (before the pool is made, for its workers to be sampled)
    if args.stats.is_some() {
        aqr_common::trace::enable();
    }
    aqr_common::pool::init(args.opt.threads);
    let r = aqr_common::pool::run(|| match () {
        _ if args.edit.is_some() || args.compact => edit::edit(&args),
        _ if args.info => info::info(&args),
        _ if args.history => journal::history_all(&args),
        _ if args.diff => journal::diff_all(&args),
        _ if args.list => list::list_all(&args),
        _ if args.decompress => decompress_all(&args),
        _ => compress_all(&args),
    });
    // (of the archives done, whether or not all are)
    let r = match args.stats.as_ref().map(|s| s.write(stats::Status::Finished, std::io::stderr().is_terminal())) {
        Some(Err(e)) => r.and(Err(e)),
        _ => r,
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("aqr: {e}");
            ipc::tell(serde_json::json!({"event": "error", "message": e}));
            ExitCode::from(exit::code())
        }
    }
}

impl Args {
    /// The option appending as given, for messages: `-a` or `-s`.
    fn verb(&self) -> &'static str {
        if self.sync { "-s" } else { "-a" }
    }
}

fn parse() -> Result<Args, String> {
    let mut argv = std::env::args_os();
    let me = argv.next().map(PathBuf::from).unwrap_or_default();
    // (`--stats` alone, no input nor any other option: [`stats::alone`])
    let stats_alone = argv.len() > 0
        && std::env::args_os().skip(1).all(|a| a == "--stats" || a.to_string_lossy().starts_with("--stats="));
    let unaqr = me.file_stem().is_some_and(|s| s.to_string_lossy().starts_with("unaqr"));
    // (a command alone: [`complete`])
    let all: Vec<OsString> = std::env::args_os().skip(1).collect();
    if let Some(first) = all.first().map(|f| f.to_string_lossy().into_owned())
        && (first == "--setup-autocomplete" || first.starts_with("--setup-autocomplete="))
    {
        let shell = match (first.split_once('='), &all[1..]) {
            (Some((_, s)), []) => Some(s.to_owned()),
            (None, []) => None,
            (None, [s]) => Some(s.to_string_lossy().into_owned()),
            _ => return Err("--setup-autocomplete is a command alone (no other argument)".into()),
        };
        std::process::exit(match complete::setup(shell.as_deref()) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("aqr: {e}");
                1
            }
        });
    }
    let mut a = Args {
        decompress: unaqr,
        test: false,
        list: false,
        info: false,
        wasm: false,
        sandbox: true,
        output: None,
        auto_dir: false,
        force: false,
        rm: false,
        atomic: false,
        journal: None,
        at: None,
        history: false,
        diff: false,
        from: None,
        keep_since: None,
        drop_history: false,
        append: false,
        sync: false,
        edit: None,
        compact: false,
        dereference: false,
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
        json: false,
        format: Pack::Aqr,
        level: None,
        passphrase: false,
        pass_from: None,
        exclude: Exclude::default(),
        each: false,
        select: None,
        names: names::Opts::default(),
        links: extract::Links::Inside,
        stdin: false,
        stdout: false,
    };
    // (by the number of inputs before it, `--rename`'s place and value)
    let mut renames: Vec<(usize, PathBuf)> = Vec::new();
    let (mut stats, mut stats_to) = (false, Vec::new());
    let mut format = None;
    let mut null = false;
    // (`--files-from -`: standard input taken by a list)
    let (mut list_stdin, mut listed) = (false, false);
    let mut memory = None;
    let mut application_bound = None;
    // (`-C`: the directory so far, by input the one it was given in, and the last given)
    let (mut dir, mut dirs, mut last_dir) = (PathBuf::new(), Vec::new(), None);
    let mut lex = Lexer { argv, short: Default::default(), only_inputs: false };
    while let Some(tok) = lex.next() {
        let (s, mut inline) = match tok {
            Tok::Input(i) => {
                a.inputs.push(i.into());
                dirs.push(dir.clone());
                continue;
            }
            Tok::Opt(s, inline) => (s, inline),
        };
        // (as given: a path need not be Unicode; [`text`] for the others)
        let mut value = |name: &str| match inline.take() {
            Some(v) => Ok(v),
            None => lex.value().ok_or(format!("{name} takes a value")),
        };
        match &*s {
            "-h" | "--help" => {
                print!("{HELP}");
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("aqr {}", env!("AQR_VERSION"));
                std::process::exit(0);
            }
            "-c" | "--compress" => (a.decompress, a.list, a.info) = (false, false, false),
            "-d" | "--decompress" => (a.decompress, a.test, a.list, a.info) = (true, false, false, false),
            "-K" | "--test" => (a.decompress, a.test, a.list, a.info) = (true, true, false, false),
            "-t" | "--list" => (a.list, a.info) = (true, false),
            "-I" | "--info" => (a.info, a.list) = (true, false),
            "--delete" => a.edit = Some(edit::Edit::Delete),
            "--move" => a.edit = Some(edit::Edit::Move),
            "--compact" => a.compact = true,
            "--wasm" => a.wasm = true,
            "--no-sandbox" => a.sandbox = false,
            "-o" | "--output" => a.output = Some(value("-o")?.into()),
            "-C" | "--directory" => {
                let to = dir.join(value("-C")?);
                if let Err(e) = fs::read_dir(&to) {
                    return Err(format!("-C {}: {e}", to.display()));
                }
                (dir, last_dir) = (to, Some(a.inputs.len()));
            }
            "--auto-dir" => a.auto_dir = true,
            "-f" | "--force" => a.force = true,
            "--rm" => a.rm = true,
            "--atomic" => a.atomic = true,
            "-j" | "--journal" => a.journal = Some(text(value("-j")?)),
            "--at" => a.at = Some(journal::at_of(&text(value("--at")?))?),
            "--history" => a.history = true,
            "--diff" => a.diff = true,
            "--from" => a.from = Some(journal::at_of(&text(value("--from")?))?),
            "--keep-since" => a.keep_since = Some(journal::at_of(&text(value("--keep-since")?))?),
            "--drop-history" => a.drop_history = true,
            "-a" | "--append" => (a.append, a.sync) = (true, false),
            "-s" | "--sync" => (a.append, a.sync) = (true, true),
            "-L" | "--dereference" => a.dereference = true,
            "-p" | "--preserve" => a.preserve = true,
            "-q" | "--quiet" => a.quiet = true,
            "-v" | "--verbose" => a.verbose = true,
            "-T" | "--threads" => a.opt.threads = text(value("-T")?).parse().map_err(|_| "-T takes a number")?,
            "-M" | "--memory" => memory = Some(text(value("-M")?)),
            "-l" | "--level" => a.level = Some(text(value("-l")?)),
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
            "-B" | "--batch" => {
                a.opt.batch = size_of(&text(value("-B")?)).ok_or("-B takes a size (64M, 1G, ...)")?.max(1)
            }
            "-b" | "--block-size" => {
                a.opt.block = size_of(&text(value("-b")?)).ok_or("-b takes a size (64K, 1M, ...)")?.max(1)
            }
            "-S" | "--segment-size" => {
                a.opt.segment = size_of(&text(value("-S")?)).ok_or("-S takes a size (64M, 1G, ...)")?
            }
            "-D" | "--dictionaries" => a.opt.dictionaries = true,
            "-A" | "--archival" => a.opt.mirrors = true,
            "--no-check" => a.opt.verify = false,
            "--stats" => match inline.take() {
                None => stats = true,
                Some(to) => {
                    let to = PathBuf::from(to);
                    if !stats::takes(&to) {
                        return Err("--stats= takes a file named .json or .html".into());
                    }
                    stats_to.push(to);
                }
            },
            "--application-bound" => {
                let v = text(value("--application-bound")?);
                application_bound = Some(bound_of(&v)?);
            }
            "--ipc" => a.ipc = true,
            "--json" => a.json = true,
            "--setup-autocomplete" => return Err("--setup-autocomplete is a command alone (no other argument)".into()),
            "-P" | "--passphrase" => a.passphrase = true,
            "--passphrase-from" => {
                a.pass_from = Some(pass::Source::parse(&value("--passphrase-from")?)?);
                a.passphrase = true;
            }
            "--each" => a.each = true,
            "-x" | "--exclude" => a.exclude.add(&value("-x")?)?,
            "--null" => null = true,
            "--files-from" => {
                let from = value("--files-from")?;
                list_stdin |= stdio::is(Path::new(&from));
                listed = true;
                for p in files_from(&from, null)? {
                    if stdio::is(&p) {
                        return Err(format!("--files-from {}: - (standard input) among the paths", from.display()));
                    }
                    a.inputs.push(p);
                    dirs.push(dir.clone());
                }
            }
            "--rename" => renames.push((a.inputs.len(), value("--rename")?.into())),
            "-E" | "--encoding" => a.names.encoding = aqr_unpack::Choice::parse(&text(value("-E")?))?,
            "--normalize" => a.names.normalize = names::Opts::normalize(&text(value("--normalize")?))?,
            "--bad-names" => a.names.bad = names::Opts::bad(&text(value("--bad-names")?))?,
            "--links" => a.links = extract::Links::parse(&text(value("--links")?))?,
            // -0 to -9, and whatever level of more digits there may be (one level, not several)
            s if s.len() > 1 && s[1..].bytes().all(|b| b.is_ascii_digit()) => a.level = Some(s[1..].to_string()),
            s => return Err(format!("unknown option {s}")),
        }
        if inline.is_some() {
            return Err(format!("{s} takes no value"));
        }
    }
    if stats_alone {
        let r = stats::alone(stats, &stats_to, std::io::stderr().is_terminal());
        std::process::exit(match r {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("aqr: {e}");
                1
            }
        });
    }
    // (none given, of standard input where it is not a terminal: as zstd and gzip have it)
    if a.inputs.is_empty() && !listed && !a.list && !a.info && !a.ipc && !std::io::stdin().is_terminal() {
        a.inputs.push("-".into());
        dirs.push(dir.clone());
    }
    if a.inputs.is_empty() {
        return Err("no input".into());
    }
    if stats || !stats_to.is_empty() {
        a.stats = Some(stats::init(stats_to));
    }
    if a.opt.threads == 0 {
        a.opt.threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    }
    let system = (aqr_common::budget::total(), aqr_common::budget::available());
    let limit = budget(memory.as_deref(), system)?;
    aqr_common::budget::global().set_limit(limit);
    if memory.is_some() {
        // (and the heap of a recompression: it gives up past it, the data kept as it is)
        aqr_common::budget::set_attempt_limit(limit);
        // (what a user asked for also bounds the memories of an application of a module in Wasm:
        // its concurrency is lowered to fit, and it fails if it does not at 1)
        aqr_decompress::set_application_bound(limit);
    }
    match application_bound {
        // (compressing: the most an application may need to be made, which the archive's check
        // by decoding then has to allow; decompressing: the most one is let to need)
        Some(b) if !a.decompress => {
            a.opt.application_bound = Some(b);
            aqr_formula::set_application_limit(b.max(aqr_formula::SUPPORTED_BOUND));
        }
        Some(b) => aqr_formula::set_application_limit(b),
        None => {}
    }
    if a.edit.is_some() || a.compact {
        edit_args(&mut a, renames, last_dir.is_some())?;
        return Ok(a);
    }
    if a.keep_since.is_some() || a.drop_history {
        return Err("--keep-since and --drop-history are for --compact".into());
    }
    if a.history || a.diff {
        let what = if a.diff { "--diff" } else { "--history" };
        if a.history && a.diff {
            return Err("--history with --diff".into());
        }
        if a.decompress || a.list || a.info || a.journal.is_some() || a.output.is_some() || a.rm {
            return Err(format!("{what} takes an archive{} alone", if a.diff { " and paths" } else { "s" }));
        }
        if a.history && (a.at.is_some() || a.from.is_some()) {
            return Err("--history takes archives alone".into());
        }
        // (--diff with paths: they are taken as the inputs of -a are, below)
        if a.history {
            return Ok(a);
        }
    }
    if a.from.is_some() && !a.diff {
        return Err("--from is for --diff".into());
    }
    if a.at.is_some() && !(a.list || a.decompress || a.diff) {
        return Err("--at is for -t, -d, -K and --diff".into());
    }
    if let Some(label) = &a.journal {
        if a.decompress || a.list || a.info {
            return Err("-j is for compressing".into());
        }
        a.opt.journal = Some(journal::point(label, journal::now_ms()?)?);
    }
    if a.json && !a.list {
        return Err("--json is for -t".into());
    }
    if a.info {
        if a.rm || a.each || a.dereference || a.output.is_some() || !renames.is_empty() || last_dir.is_some() {
            return Err("-I takes an archive and its paths alone".into());
        }
        return Ok(a);
    }
    if a.list {
        if a.rm {
            return Err("--rm with -t".into());
        }
        if a.output.is_some() || !renames.is_empty() || last_dir.is_some() {
            return Err("-t takes an archive and its paths alone".into());
        }
        if !a.each && a.inputs.len() > 1 {
            a.select = Some(selection(a.inputs.split_off(1), Vec::new())?);
        }
    }
    if (!a.exclude.names.is_empty() || !a.exclude.paths.is_empty()) && (a.decompress || a.list || a.info) {
        return Err("-x is for compressing".into());
    }
    if a.dereference && (a.decompress || a.list) {
        return Err("-L is for compressing".into());
    }
    if a.dereference && a.rm {
        // (what a link leads to is not the input's to remove, and removing the link alone
        // would not take away what was archived)
        return Err("--rm with -L".into());
    }
    if a.list {
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
                if a.opt.mirrors {
                    return Err(format!("-A: {} archives are not made so here (.aqr only)", f.ext()));
                }
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
        a.inputs = a.given.iter().zip(&dirs).map(|(i, d)| if stdio::is(i) { i.clone() } else { d.join(i) }).collect();
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
    if a.rm && a.test {
        return Err("--rm with -K".into());
    }
    if a.rm && a.select.is_some() {
        return Err("--rm with paths of an archive (it is removed once extracted whole)".into());
    }
    if a.decompress && a.output.is_some() && a.inputs.len() > 1 && !a.auto_dir {
        return Err("-o with several archives to decompress".into());
    }
    if list_stdin && (a.inputs.iter().any(|i| stdio::is(i)) || a.pass_from == Some(pass::Source::Stdin)) {
        return Err("--files-from - with standard input taken otherwise too".into());
    }
    stdio_args(&mut a)?;
    a.dir = dir;
    Ok(a)
}

/// Of [`parse`], editing an archive ([`edit`]): the archive and its paths, checked against the
/// other options given (`renames` as [`selection`] takes them; `dir`: `-C` given).
fn edit_args(a: &mut Args, renames: Vec<(usize, PathBuf)>, dir: bool) -> Result<(), String> {
    let o = a.edit.map_or("--compact", edit::Edit::option);
    let modes = [
        (a.decompress && !a.test, "-d"),
        (a.test, "-K"),
        (a.list, "-t"),
        (a.info, "-I"),
        (a.append, a.verb()),
        (a.passphrase || a.pass_from.is_some(), "-P (an encrypted archive is not edited)"),
        (a.opt.mirrors, "-A"),
        (a.journal.is_some(), "-j"),
        (a.output.is_some(), "-o"),
        (dir, "-C"),
        (a.rm, "--rm"),
        (a.each, "--each"),
        (a.dereference, "-L"),
        (a.auto_dir, "--auto-dir"),
        (a.json, "--json"),
        (!a.exclude.names.is_empty() || !a.exclude.paths.is_empty(), "-x"),
    ];
    if let Some((_, what)) = modes.iter().find(|m| m.0) {
        return Err(format!("{o} with {what}"));
    }
    if a.keep_since.is_some() && a.drop_history {
        return Err("--keep-since with --drop-history".into());
    }
    if a.keep_since.is_some() && a.edit.is_some() {
        return Err("--keep-since is for --compact alone (the paths edited are not in the states between)".into());
    }
    if (a.keep_since.is_some() || a.drop_history) && a.edit == Some(edit::Edit::Delete) && !a.compact {
        return Err("--drop-history is for --compact (--delete alone only adds)".into());
    }
    if a.inputs.iter().any(|i| stdio::is(i)) {
        return Err(format!("{o}: an archive in place, not - (standard input)"));
    }
    if a.edit != Some(edit::Edit::Move)
        && let Some((_, to)) = renames.first()
    {
        return Err(format!("{o}: --rename {} (paths are taken out)", to.display()));
    }
    match a.edit {
        None if a.inputs.len() != 1 => return Err("--compact alone takes an archive alone".into()),
        None => {}
        Some(_) if a.inputs.len() < 2 => return Err(format!("{o} takes an archive and its paths")),
        Some(_) => a.select = Some(selection(a.inputs.split_off(1), renames)?),
    }
    if a.edit == Some(edit::Edit::Move)
        && let Some(sel) = &a.select
        && let Some(k) = sel.paths.iter().position(|p| p.1.is_none())
    {
        return Err(format!("--move: {} without --rename TO after it", sel.shown(k)));
    }
    if let Some(l) = &a.level {
        a.opt.level = l.parse()?;
    }
    Ok(())
}

/// An argument as [`Lexer`] reads it: an input, or an option (`-x` or `--long`) with the value
/// given in the same argument (`-xVALUE`, `--long=VALUE`).
enum Tok {
    Input(OsString),
    Opt(String, Option<OsString>),
}

/// The short options that take a value: in a bundle (`-cfo OUT`), the rest of the argument is
/// theirs (`-oOUT`), or else the next argument.
const SHORT_VALUES: &[u8] = b"oCTMlBbSEx";

/// Reads the arguments as options and inputs, getopt's way: short options bundled (`-cf`), a
/// value joined to its option (`-T8`, `--memory=2G`), `--` ending the options, `-` an input
/// (standard input or output). The digits of a level (`-10`) are one option.
struct Lexer {
    argv: std::env::ArgsOs,
    /// The options left of the bundle being read.
    short: std::collections::VecDeque<Tok>,
    only_inputs: bool,
}

impl Lexer {
    fn next(&mut self) -> Option<Tok> {
        if let Some(t) = self.short.pop_front() {
            return Some(t);
        }
        let arg = self.argv.next()?;
        let b = arg.as_encoded_bytes();
        if self.only_inputs || b.len() < 2 || b[0] != b'-' {
            return Some(Tok::Input(arg));
        }
        if b == b"--" {
            self.only_inputs = true;
            return self.next();
        }
        // SAFETY: split after ASCII, of an `OsStr`'s bytes
        let os = |b: &[u8]| unsafe { OsStr::from_encoded_bytes_unchecked(b) }.to_owned();
        if let Some(long) = b.strip_prefix(b"--") {
            return Some(match long.iter().position(|&c| c == b'=') {
                Some(i) => Tok::Opt(format!("--{}", String::from_utf8_lossy(&long[..i])), Some(os(&long[i + 1..]))),
                None => Tok::Opt(format!("--{}", String::from_utf8_lossy(long)), None),
            });
        }
        let mut i = 1;
        while i < b.len() {
            let c = b[i];
            if c.is_ascii_digit() {
                let n = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
                self.short.push_back(Tok::Opt(format!("-{}", String::from_utf8_lossy(&b[i..i + n])), None));
                i += n;
                continue;
            }
            let name = format!("-{}", if c.is_ascii() { char::from(c).to_string() } else { "?".into() });
            if SHORT_VALUES.contains(&c) {
                let rest = (i + 1 < b.len()).then(|| os(&b[i + 1..]));
                self.short.push_back(Tok::Opt(name, rest));
                break;
            }
            self.short.push_back(Tok::Opt(name, None));
            i += 1;
        }
        self.short.pop_front()
    }

    /// The next argument as it is, an option's value.
    fn value(&mut self) -> Option<OsString> {
        self.argv.next()
    }
}

/// Of the arguments parsed, standard input and output (`-`): where they are taken, and what
/// they are not taken with.
fn stdio_args(a: &mut Args) -> Result<(), String> {
    a.stdin = a.inputs.iter().any(|i| stdio::is(i));
    let to = a.output.as_deref().is_some_and(stdio::is);
    if a.stdin && a.inputs.len() > 1 {
        return Err(match a.decompress {
            true => "- (standard input): an archive alone, of no paths given".into(),
            false => "- (standard input): the only input".into(),
        });
    }
    if a.stdin && a.rm {
        return Err("--rm with - (standard input)".into());
    }
    if a.stdin && a.passphrase && a.pass_from.as_ref().is_none_or(|f| *f == pass::Source::Stdin) {
        // (a line of standard input, or the terminal: neither to be taken where it is the input)
        return Err("-P with - (standard input): the passphrase is read from it (--passphrase-from)".into());
    }
    if a.stdin && !a.decompress && a.renames.iter().any(Option::is_some) {
        return Err("--rename of - (standard input): an archive of a single file has no names".into());
    }
    a.stdout = !a.test && (to || (a.stdin && a.output.is_none()));
    if !a.stdout {
        return Ok(());
    }
    for (bad, what) in [(a.append, a.verb()), (a.auto_dir, "--auto-dir"), (a.each, "--each"), (a.ipc, "--ipc")] {
        if bad {
            return Err(format!("{what} with -o - (standard output)"));
        }
    }
    if !a.decompress {
        if std::io::stdout().is_terminal() && !a.force {
            return Err("not writing an archive to a terminal (-f to)".into());
        }
        // (written as it is made, never written over: nothing to commit)
        a.atomic = true;
    }
    Ok(())
}

/// The paths listed in file `from` (`-`: standard input) for `--files-from`: a line each, its
/// end (`\n`, `\r\n`) left out; or, where `null`, ended by NUL each. Empty ones left out.
fn files_from(from: &OsStr, null: bool) -> Result<Vec<PathBuf>, String> {
    let err = |e: std::io::Error| format!("--files-from {}: {e}", from.display());
    let mut b = Vec::new();
    match stdio::is(Path::new(from)) {
        true => std::io::Read::read_to_end(&mut std::io::stdin().lock(), &mut b).map_err(err)?,
        false => std::io::Read::read_to_end(&mut fs::File::open(from).map_err(err)?, &mut b).map_err(err)?,
    };
    let end = if null { 0 } else { b'\n' };
    Ok(b.split(|&c| c == end)
        .map(|l| if null { l } else { l.strip_suffix(b"\r").unwrap_or(l) })
        .filter(|l| !l.is_empty())
        .map(|l| PathBuf::from(os_from(l)))
        .collect())
}

/// An option's value as text (any of it not Unicode replaced), where it is not a path.
fn text(v: std::ffi::OsString) -> String {
    v.to_string_lossy().into_owned()
}

/// Path `p` of an archive as given: its names, encoded as the tree's (`.` left out).
fn path_names(p: &Path) -> Result<Vec<Vec<u8>>, String> {
    // (on Windows, `\` is `/` too)
    let bytes = os_bytes(p.as_os_str());
    let names: Vec<Vec<u8>> =
        bytes.split(|&b| b == b'/').filter(|n| !n.is_empty() && *n != b".").map(name_from).collect();
    if names.is_empty() || names.iter().any(|n| n == b"..") {
        return Err(format!("{}: not a path in an archive", p.display()));
    }
    Ok(names)
}

/// The paths of an archive given, `paths`, with `renames` (each by the number of inputs before
/// it, the archive's too): each path's names (encoded as the tree's), and where it goes.
fn selection(paths: Vec<PathBuf>, renames: Vec<(usize, PathBuf)>) -> Result<extract::Selection, String> {
    let mut out: Vec<(Vec<Vec<u8>>, Option<PathBuf>)> = Vec::new();
    for p in &paths {
        out.push((path_names(p)?, None));
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

/// Of `-M` (the system's total and available memory, where known), the bytes of the budget of
/// the process ([`aqr_common::budget`]): by default [`aqr_common::budget::DEFAULT`], but at most
/// half the system's memory; `avail` is [`AVAIL_SHARE`] of what is available, but at least
/// [`AVAIL_FLOOR`] (or a quarter of the system's where less), as a budget smaller only has the
/// work wait for it the more and what is available may be taken by the page cache that the
/// archives and the spill are read through; `N%` the share of the system's memory; a size; 0
/// none.
fn budget(m: Option<&str>, (total, avail): (Option<u64>, Option<u64>)) -> Result<u64, String> {
    let default = total.map_or(aqr_common::budget::DEFAULT, |t| aqr_common::budget::DEFAULT.min(t / 2));
    match m {
        None => Ok(default),
        Some("avail") => {
            let floor = total.map_or(AVAIL_FLOOR, |t| AVAIL_FLOOR.min(t / 4));
            Ok(avail.map_or(default, |a| (a as f64 * AVAIL_SHARE) as u64).max(floor))
        }
        Some(p) if p.ends_with('%') => {
            let n: f64 =
                p[..p.len() - 1].parse().ok().filter(|n| *n > 0.0 && *n <= 100.0).ok_or("-M N% takes N of 0 to 100")?;
            let t = total.ok_or("-M N%: the system's memory is not known")?;
            Ok((t as f64 * n / 100.0) as u64)
        }
        Some(s) => match size_of(s).ok_or("-M takes a size (512M, 2G, ...), N% or avail")? {
            0 => Ok(u64::MAX),
            n => Ok(n as u64),
        },
    }
}

/// Of the memory available, the share that `-M avail` takes.
const AVAIL_SHARE: f64 = 0.8;

/// The least budget of `-M avail`.
const AVAIL_FLOOR: u64 = 1 << 30;

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

/// Tells on standard error the recompressions not used for the bound of `--application-bound`
/// (and the applications kept over it), by module, and those given up for their heap, by format.
fn tell_fallbacks(fallbacks: &[aqr_compress::Fallback]) {
    let mut by_module: std::collections::BTreeMap<(&str, bool, bool), (u64, u64, u64)> = Default::default();
    for f in fallbacks {
        let e = by_module.entry((f.module, f.kept, f.heap)).or_default();
        (e.0, e.1, e.2) = (e.0 + 1, e.1 + f.bytes, e.2.max(f.needed));
    }
    for ((module, kept, heap), (n, bytes, most)) in by_module {
        if heap {
            eprintln!(
                "heap: {module}: {n} recompressions given up, {} of data, {} held as they did (of {} each)",
                size(bytes as f64),
                size(most as f64),
                size(aqr_common::budget::attempt_limit() as f64)
            );
            continue;
        }
        let what = if kept { "kept over the bound" } else { "not used for the bound" };
        eprintln!(
            "application bound: {module}: {n} {what}, {} of data, up to {} needed (minimal bound)",
            size(bytes as f64),
            aqr_compress::bound_text(most)
        );
    }
}

/// Of `--application-bound`, the bytes: a size, or `unlimited`.
fn bound_of(v: &str) -> Result<u64, String> {
    match v {
        "unlimited" => Ok(u64::MAX),
        v => size_of(v).map(|n| n as u64).ok_or("--application-bound takes a size (256M, 1G, ...) or unlimited".into()),
    }
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
    if a.stdin || a.stdout {
        let r = compress_stdio(a);
        stdio::remove();
        return r;
    }
    if a.append {
        let (out, skip) = match &a.output {
            Some(o) => (o.clone(), 0),
            None => (a.inputs.first().cloned().ok_or_else(|| format!("{}: no archive given", a.verb()))?, 1),
        };
        if a.inputs.len() <= skip {
            return Err(format!("{}: no inputs given", a.verb()));
        }
        return remove(&compress_into(a, &a.inputs[skip..], &a.given[skip..], &a.renames[skip..], &out)?);
    }
    match &a.output {
        Some(out) => remove(&compress_into(a, &a.inputs, &a.given, &a.renames, out)?),
        None => {
            // (where some of an input is kept, the next ones still archived and removed)
            let mut kept = Ok(());
            for ((input, given), to) in a.inputs.iter().zip(&a.given).zip(&a.renames) {
                let out = with_ext(input, &a.format.ext());
                use std::slice::from_ref as one;
                let gone = compress_into(a, one(input), one(given), one(to), &out)?;
                kept = kept.and(remove(&gone));
            }
            kept
        }
    }
}

/// Compresses standard input (read whole first), or into standard output ([`stdio`]): an
/// archive of several files written there as it is made, others made whole and copied there.
fn compress_stdio(a: &Args) -> Result<(), String> {
    let (inputs, given) = match a.stdin {
        true => (vec![stdio::spool_in()?], vec![PathBuf::from("-")]),
        false => (a.inputs.clone(), a.given.clone()),
    };
    let out = match (a.stdout, &a.output) {
        (true, _) => stdio::temp(&format!("stdout{}", a.format.ext()))?,
        (false, Some(o)) => o.clone(),
        (false, None) => unreachable!("standard input to standard output by default"),
    };
    let gone = compress_into(a, &inputs, &given, &a.renames, &out)?;
    if a.stdout && !stdio::streamed() {
        stdio::copy_out(&out)?;
    }
    remove(&gone)
}

/// A path to be removed with `--rm` (an input archived, or an archive extracted), as it was
/// read: kept where it is found changed since (written over meanwhile, or the output put in
/// its place).
struct Gone {
    path: PathBuf,
    dir: bool,
    /// Its size, modification time and (on Unix) device and inode, as read.
    seen: (u64, Option<std::time::SystemTime>, (u64, u64)),
}

impl Gone {
    fn of(path: &Path, m: &fs::Metadata) -> Gone {
        Gone { path: path.to_owned(), dir: m.is_dir(), seen: Gone::seen(m) }
    }

    fn seen(m: &fs::Metadata) -> (u64, Option<std::time::SystemTime>, (u64, u64)) {
        #[cfg(unix)]
        let id = {
            use std::os::unix::fs::MetadataExt;
            (m.dev(), m.ino())
        };
        #[cfg(not(unix))]
        let id = (0, 0);
        (m.len(), m.modified().ok(), id)
    }
}

/// Removes `gone` (`--rm`), the last first (what a directory holds before it): a directory
/// where it is left empty, anything else where it is as it was read. What is kept is told on
/// standard error (a directory not left empty only where nothing under it is told), and the
/// result is an error then.
fn remove(gone: &[Gone]) -> Result<(), String> {
    let mut kept: Vec<&Path> = Vec::new();
    for g in gone.iter().rev() {
        let r = match fs::symlink_metadata(&g.path) {
            // (removed already)
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
            Ok(m) if m.is_dir() != g.dir || !g.dir && Gone::seen(&m) != g.seen => {
                Err("changed since it was read".into())
            }
            Ok(_) if g.dir => match fs::remove_dir(&g.path) {
                Err(e) if e.kind() == std::io::ErrorKind::DirectoryNotEmpty => {
                    match kept.iter().any(|k| k.starts_with(&g.path)) {
                        true => Err(String::new()),
                        false => Err("not empty: something else is in it".into()),
                    }
                }
                r => r.map_err(|e| e.to_string()),
            },
            Ok(_) => fs::remove_file(&g.path).map_err(|e| e.to_string()),
        };
        if let Err(e) = r {
            if !e.is_empty() {
                eprintln!("kept {} ({e})", g.path.display());
            }
            kept.push(&g.path);
        }
    }
    match kept.len() {
        0 => Ok(()),
        1 => {
            exit::note(exit::Code::Incomplete);
            Err(format!("--rm: {} not removed", kept[0].display()))
        }
        n => {
            exit::note(exit::Code::Incomplete);
            Err(format!("--rm: {n} paths not removed (told above)"))
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
    /// With `--rm`, the paths archived (each directory before what it holds), as read.
    gone: Option<Vec<Gone>>,
    /// With `-p`, the access times of the paths read, put back once they are archived.
    atimes: Atimes,
}

/// The access times of the paths read as they were before (`-p`, as they are archived), put
/// back as this is dropped ([`meta::restore_atime`]): once the archive is made, or has failed.
#[derive(Default)]
struct Atimes(Option<Vec<(PathBuf, aqr_formula::aqr::Time)>>);

impl Atimes {
    /// Notes the access time of `path`, as `m` read it, where they are kept.
    fn note(&mut self, path: &Path, m: &fs::Metadata) {
        if let (Some(v), Some(t)) = (&mut self.0, meta::atime_of(m)) {
            v.push((path.to_owned(), t));
        }
    }
}

impl Drop for Atimes {
    fn drop(&mut self) {
        for (path, t) in self.0.take().unwrap_or_default() {
            meta::restore_atime(&path, t);
        }
    }
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
            gone: None,
            atimes: Atimes(preserve.then(Vec::new)),
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

    /// The bytes of the files' contents (and forks') as decompressing writes them: each file of
    /// a link group for its size, though the content is read, and archived, once.
    fn input_bytes(&self) -> u64 {
        let size = |c: &usize| self.contents[*c].2;
        let files = self.tree.entries.iter().flat_map(|e| match e.node {
            EntryNode::File(c) => Some(c),
            _ => None,
        });
        let forks = self.tree.entries.iter().flat_map(|e| e.extra.forks.iter().map(|f| f.1));
        files.chain(forks).chain(self.tree.root.forks.iter().map(|f| f.1)).map(|c| size(&c)).sum()
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
        let mut f = aqr_compress::mapped::open(p).map_err(at)?;
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
) -> Result<Vec<Gone>, String> {
    if let Pack::Other(f, level) = a.format {
        if a.append {
            return Err(format!("{}: Aqueous archives alone are appended to", a.verb()));
        }
        if a.journal.is_some() {
            return Err("-j: Aqueous archives alone have a journal".into());
        }
        return pack_into(a, f, level, inputs, given, renames, out);
    }
    if a.append && (a.passphrase || a.opt.mirrors) {
        return Err(format!(
            "{}: not with {}",
            a.verb(),
            if a.passphrase { "-P (an encrypted archive is not appended to)" } else { "-A" }
        ));
    }
    // (appending, the archive there already, to be appended to)
    let force = match a.append {
        true => false,
        false => match output(a, out)? {
            Some(f) => f,
            None => return Ok(Vec::new()),
        },
    };
    // (the archive appended to opened first: one not to be appended to told before the inputs are read)
    let opened = match a.append {
        true => Some(append::open(out, a.quiet)?),
        false => None,
    };
    begin(
        match (a.append, a.sync) {
            (false, _) => "compress",
            (true, false) => "append",
            (true, true) => "sync",
        },
        inputs,
        Some(out),
    );
    let start = Instant::now();
    let rec = stats::record(
        a.stats,
        "compress",
        inputs.iter().map(|i| i.display().to_string()).collect::<Vec<_>>().join(" "),
        out.display().to_string(),
        a.opt.threads,
    );
    let mut opt = a.opt.clone();
    let sealing = pass::keys().sealing();
    opt.spill = spill_of(out, sealing.is_some())?;
    opt.size_hint = sealing.is_some();
    let opt = &opt;
    let meta = |p: &Path| fs::symlink_metadata(p).map_err(|e| format!("{}: {e}", p.display()));
    let mapper = Mapper::new().map_err(|e| format!("files not to be mapped: {e}"))?;
    // a single file: an archive of the file alone (but renamed: in a tree, at its name)
    let followed = match (inputs, renames) {
        ([input], [None]) if a.dereference => Some(follow(input)?),
        _ => None,
    };
    let single = match (followed.as_ref().map(std::slice::from_ref).unwrap_or(inputs), renames) {
        _ if a.append => None,
        ([input], [None]) => Some((input, meta(input)?)).filter(|(_, m)| m.is_file()),
        _ => None,
    };
    if single.is_some() && a.journal.is_some() {
        return Err("-j: not with an archive of a single file (it has no segments to name)".into());
    }
    if single.is_some() && a.exclude.excludes(&archive_names(&given[0], None)?) {
        return Err(format!("nothing to archive in {} (-x)", inputs[0].display()));
    }
    if let Some((input, m)) = single {
        let mut atimes = Atimes(a.preserve.then(Vec::new));
        atimes.note(input, &m);
        let d = mapper.read(input).map_err(|e| format!("{}: {e}", input.display()))?;
        // (what is cut out of it shared, not copied: an image's files, say; and where it is mapped,
        // as the mapper shares only what it maps, its pages let go of as it is read through)
        let _known = match d.is_shared() {
            true => aqr_formula::store::known_mapped(&d),
            false => aqr_formula::store::known(&d),
        };
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
        let fallbacks = aqr_compress::fallbacks();
        if !a.quiet && a.verbose {
            tell_fallbacks(&fallbacks);
            let modules: Vec<String> = f.applies().iter().map(|m| m.module.to_string()).collect();
            let format = detect(&d).map_or("-", |f| f.name());
            eprintln!(
                "{}: {}, {}, {} -> {} ({})",
                stdio::shown(input),
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
                fallbacks: fallbacks.clone(),
                trace: rec.finish(),
            });
        }
        return Ok(a.rm.then(|| Gone::of(input, &m)).into_iter().collect());
    }

    let mut g = Gathered::new(mapper, a.preserve);
    g.gone = a.rm.then(Vec::new);
    let gathering = aqr_common::trace::span("gather");
    // (as long as it takes: a bar of no end known, and how many files)
    let listing = view::Listing::new(bars());
    let gathered = ticking(a, &listing, || -> Result<(), String> {
        gather(inputs, given, renames, &mut g, &listing, a)?;
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
    // appending: what the archive holds already left out
    let prior = match opened {
        Some(o) => match o.against(&mut g, a.sync) {
            (p, true) => Some(p),
            (p, false) => {
                // (a point named all the same, by a segment of no entries)
                let grew = match &opt.journal {
                    Some(point) => Some(edit::journal(a, out, p, point)?),
                    None => None,
                };
                if !a.quiet {
                    match (&opt.journal, grew) {
                        (Some((_, label)), Some(n)) => {
                            eprintln!(
                                "{}: nothing to append, all there already; journal point {label:?} added (+{n} bytes)",
                                out.display()
                            )
                        }
                        _ => eprintln!("{}: nothing to append, all there already", out.display()),
                    }
                }
                end(Some(out), 0, 0, grew.unwrap_or(0), start);
                return Ok(g.gone.take().unwrap_or_default());
            }
        },
        None => None,
    };
    // (with -v, the log as the files are written)
    let view = view::View::new(&g, bars(), a.verbose);
    // written as it is made
    // (the files, as decompressing counts them: not their forks, each hard link)
    let files = g.tree.entries.iter().filter(|e| matches!(e.node, EntryNode::File(_))).count();
    // committed as it is written, and ended as it is where stopped, but with --atomic
    let placed = std::sync::atomic::AtomicBool::new(prior.is_some());
    let total = g.contents.iter().map(|c| c.2).sum::<u64>();
    let ended = |r: &aqr_compress::Report| stopped_tree(a, out, force, &placed, (files as u64, total), r);
    let stopping = (!a.atomic).then(|| aqr_compress::Stop { requested: stop::graceful(), ended: &ended });
    let r = match prior {
        Some(p) => appended(a, out, p, &view, &g, opt, stopping.as_ref())?,
        // (written out as it is made: not to be written over, nor read back)
        None if a.stdout && sealing.is_none() => {
            let err = |e: std::io::Error| format!("standard output: {e}");
            let mut w = std::io::BufWriter::with_capacity(8 << 20, std::io::stdout().lock());
            let r = viewed(a, &view, |tell| {
                let mut o = aqr_compress::Plain(&mut w);
                aqr_compress::compress_tree_into(&g.tree, &g, opt, &mut o, None, None, tell)
            })
            .map_err(err)?;
            w.flush().map_err(err)?;
            stdio::set_streamed();
            r
        }
        None => write_placing(out, force, |file, part| match &sealing {
            None => {
                let w = std::io::BufWriter::with_capacity(8 << 20, &*file);
                let place = Some((part.to_owned(), out.to_owned(), force));
                let mut o = FileOut { w, commits: !a.atomic, once: false, base: 0, place, placed: &placed };
                let r = viewed(a, &view, |tell| {
                    aqr_compress::compress_tree_into(&g.tree, &g, opt, &mut o, None, stopping.as_ref(), tell)
                })?;
                Ok((r, placed.load(std::sync::atomic::Ordering::SeqCst)))
            }
            // (the sealer writing a chunk at a time, the last sealed as the archive ends)
            Some(p) => {
                let mut s = Sealed(sealer(file, p)?);
                let mut r = viewed(a, &view, |tell| {
                    aqr_compress::compress_tree_into(&g.tree, &g, opt, &mut s, None, stopping.as_ref(), tell)
                })?;
                drop(s);
                // (the overhead of the encryption the archive's)
                let len = file.metadata()?.len() as usize;
                (r.overhead, r.len) = (r.overhead + len - r.len, len);
                Ok((r, false))
            }
        })?,
    };
    let fallbacks = aqr_compress::fallbacks();
    if !a.quiet {
        if a.verbose {
            tell_fallbacks(&fallbacks);
        }
        eprintln!("archive overhead (header, modules, metadata): {}", size(r.overhead as f64));
        if r.mirrors > 0 {
            eprintln!("mirror blocks: {}", size(r.mirrors as f64));
        }
        summary(out, files, g.input_bytes() as usize, r.len, start);
    }
    end(Some(out), files, g.input_bytes(), r.len as u64, start);
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
            fallbacks: fallbacks.clone(),
            trace: rec.finish(),
        });
    }
    Ok(g.gone.take().unwrap_or_default())
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
) -> Result<Vec<Gone>, String> {
    let Some(force) = output(a, out)? else { return Ok(Vec::new()) };
    begin("compress", inputs, Some(out));
    let start = Instant::now();
    let renames = match (inputs, renames) {
        ([input], [None]) => vec![input.file_name().map(PathBuf::from)],
        _ => renames.to_vec(),
    };
    let mapper = Mapper::new().map_err(|e| format!("files not to be mapped: {e}"))?;
    let mut g = Gathered::new(mapper, a.preserve);
    g.gone = a.rm.then(Vec::new);
    let listing = view::Listing::new(bars());
    let gathered = ticking(a, &listing, || -> Result<(), String> {
        gather(inputs, given, &renames, &mut g, &listing, a)?;
        listing.finish();
        Ok(())
    });
    listing.finish();
    gathered?;
    if g.tree.entries.is_empty() {
        return Err(format!("nothing to archive in {}", inputs[0].display()));
    }
    // (tar writes the later files of a link group as hard links to the first)
    g.link_groups();
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
        summary(out, files, g.input_bytes() as usize, w.len as usize, start);
    }
    end(Some(out), files, g.input_bytes(), w.len, start);
    Ok(g.gone.take().unwrap_or_default())
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
        "{}: {files} file{}, {} -> {} ({}) in {}, {:.1} MB/s",
        stdio::shown(out),
        if files == 1 { "" } else { "s" },
        size(total as f64),
        size(len as f64),
        percent(len as f64, total as f64),
        aqr_l10n::duration(t, 2),
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
/// handle or so each, the most of the time) read on `a`'s threads, then made the entries in the
/// order they were found. The files of an inode archived already are of its content (and
/// forks), read once. Where `a` dereferences (`-L`), the links given and found are followed
/// ([`Follow`]); what `a` excludes (`-x`) is left out.
fn gather(
    inputs: &[PathBuf],
    given: &[PathBuf],
    renames: &[Option<PathBuf>],
    g: &mut Gathered,
    l: &view::Listing,
    a: &Args,
) -> Result<(), String> {
    let (threads, dereference, x) = (a.opt.threads, a.dereference, &a.exclude);
    let err = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    let privileged = g.reader.privileged;
    let mut found = Vec::new();
    let mut follow = Follow { on: dereference, chain: Vec::new(), warnings: Vec::new() };
    // (each input as it is read: where followed, what it leads to)
    let read: Vec<(PathBuf, fs::FileType)> = inputs
        .iter()
        .map(|input| {
            let m = fs::symlink_metadata(input).map_err(|e| err(input, e))?;
            Ok(match follow.target(input, &m)? {
                Some((to, m)) => (to, m.file_type()),
                None => (input.clone(), m.file_type()),
            })
        })
        .collect::<Result<_, String>>()?;
    match (&read[..], renames) {
        // (a directory alone, its entries at the root; a link alone, not followed, is archived
        // as a link at its name)
        ([(dir, t)], [None]) if t.is_dir() => {
            let s = meta::stat(dir, privileged).map_err(|e| err(dir, e))?;
            let (extra, forks) = g.reader.extra(dir, &s, &mut |w| g.warnings.push(w));
            g.tree.root = extra;
            g.atimes.note(dir, &s.m);
            if let Some(gone) = &mut g.gone {
                gone.push(Gone::of(dir, &s.m));
            }
            for f in forks {
                g.tree.root.forks.push((f.name, g.contents.len()));
                g.contents.push((f.path.clone(), os_bytes(f.path.as_os_str()), f.size));
                l.bytes(f.size);
            }
            walk(dir, None, &mut found, l, privileged, &mut follow, x)?;
        }
        _ => {
            // (by their names in the archive, the directories found among the inputs or on the way)
            let mut dirs: std::collections::HashMap<Vec<OsString>, usize> = Default::default();
            for (((input, t), given), to) in read.iter().zip(given).zip(renames) {
                let names = archive_names(given, to.as_deref())?;
                if x.excludes(&names) {
                    continue;
                }
                let mut parent = None;
                for k in 1..names.len() {
                    let at = dirs.entry(names[..k].to_vec()).or_insert_with(|| {
                        let name = names[k - 1].clone();
                        found.push(Found { path: PathBuf::new(), name, parent, dir: true, plain: true, stat: None });
                        found.len() - 1
                    });
                    parent = Some(*at);
                }
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
                    walk(input, Some(at), &mut found, l, privileged, &mut follow, x)?;
                }
            }
            taken_twice(&found)?;
        }
    }
    g.warnings.append(&mut follow.warnings);
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
        let e = entry(&f.path, &f.name, &s, parent, g, l)?;
        g.atimes.note(&f.path, &s.m);
        if e.is_some()
            && let Some(gone) = &mut g.gone
        {
            gone.push(Gone::of(&f.path, &s.m));
        }
        made.push(e);
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
/// ([`meta::stat`]) before it is listed. Links are followed as `follow` has it.
fn walk(
    dir: &Path,
    parent: Option<usize>,
    found: &mut Vec<Found>,
    l: &view::Listing,
    privileged: bool,
    follow: &mut Follow,
    x: &Exclude,
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
    if follow.on {
        follow.chain.push(fs::canonicalize(dir).map_err(err)?);
    }
    // (the path in the archive of what it holds, where paths are excluded)
    let mut at: Vec<OsString> = Vec::new();
    if !x.paths.is_empty() {
        let mut p = parent;
        while let Some(i) = p {
            at.push(found[i].name.clone());
            p = found[i].parent;
        }
        at.reverse();
    }
    for (name, t) in names {
        at.push(name.clone());
        let out = x.excludes(&at);
        at.pop();
        if out {
            continue;
        }
        let mut path = dir.join(&name);
        let mut t = t;
        if follow.on && t.is_symlink() {
            let m = fs::symlink_metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            if let Some((to, m)) = follow.target(&path, &m)? {
                (path, t) = (to, m.file_type());
            }
        }
        found.push(Found { path, name, parent, dir: t.is_dir(), plain: false, stat: None });
        if t.is_file() {
            l.found();
        }
        if t.is_dir() {
            let at = found.len() - 1;
            let path = found[at].path.clone();
            walk(&path, Some(at), found, l, privileged, follow, x)?;
        }
    }
    follow.chain.pop();
    Ok(())
}

/// What `-x` (`--exclude`) leaves out of the inputs, with what is under it: the entries of a
/// name (a pattern with no `/` but at its end), wherever they are, and of a path in the archive
/// (`/` between names; `.` and a `/` at its start or end left out). Given as they are: no
/// wildcards yet.
#[derive(Default, Clone)]
struct Exclude {
    names: std::collections::HashSet<OsString>,
    paths: Vec<Vec<OsString>>,
}

impl Exclude {
    fn add(&mut self, p: &OsStr) -> Result<(), String> {
        let b = os_bytes(p);
        let b = b.strip_suffix(b"/").unwrap_or(&b);
        let names: Vec<OsString> =
            b.split(|&c| c == b'/').filter(|n| !n.is_empty() && *n != b".").map(os_from).collect();
        match names.len() {
            0 => return Err(format!("-x {}: no name", p.display())),
            1 if !b.contains(&b'/') => _ = self.names.insert(names.into_iter().next().unwrap()),
            _ => self.paths.push(names),
        }
        Ok(())
    }

    /// Whether the entry at path `names` in the archive is left out (what holds it not).
    fn excludes(&self, names: &[OsString]) -> bool {
        names.last().is_some_and(|n| self.names.contains(n)) || self.paths.iter().any(|p| p == names)
    }
}

/// Links followed (`-L`), where `on`: each archived as what it leads to, read by the path
/// that has no links in it ([`fs::canonicalize`]), so that what is read of it (metadata,
/// extended attributes, access times put back) is of that, not of the link. A link leading
/// nowhere, or to a directory it is in (which would be walked without end), is kept as a link
/// and told.
struct Follow {
    on: bool,
    /// The directories being walked, by such paths.
    chain: Vec<PathBuf>,
    warnings: Vec<String>,
}

impl Follow {
    /// What `path`, read (not followed) as `m`, leads to and its metadata, where it is a link
    /// to follow.
    fn target(&mut self, path: &Path, m: &fs::Metadata) -> Result<Option<(PathBuf, fs::Metadata)>, String> {
        if !self.on || !m.file_type().is_symlink() {
            return Ok(None);
        }
        let to = match fs::canonicalize(path).and_then(|to| Ok((fs::metadata(&to)?, to))) {
            Ok((m, to)) if m.is_dir() && self.chain.contains(&to) => {
                self.warnings.push(format!("{}: a link to a directory it is in, kept as a link", path.display()));
                return Ok(None);
            }
            Ok((m, to)) => (to, m),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.warnings.push(format!("{}: a link leading nowhere, kept as a link", path.display()));
                return Ok(None);
            }
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        Ok(Some(to))
    }
}

/// What input `path` leads to, where it is a link (`-L`): the path that has no links in it.
/// (A link kept is told by [`gather`], which takes it then.)
fn follow(path: &Path) -> Result<PathBuf, String> {
    let m = fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut f = Follow { on: true, chain: Vec::new(), warnings: Vec::new() };
    Ok(f.target(path, &m)?.map_or_else(|| path.to_owned(), |(to, _)| to))
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

/// The archive `out` appended to, as [`append::Opened::prior`] found it, of what `g` holds (to be
/// appended), committed as it is written (once as it ends, with `--atomic`): what became of it,
/// its length and overhead those of what was added (the archive's growth, as the summary tells
/// it against the bytes read).
fn appended(
    a: &Args,
    out: &Path,
    p: append::Prior,
    view: &view::View,
    g: &Gathered,
    opt: &Options,
    stopping: Option<&aqr_compress::Stop>,
) -> Result<aqr_compress::Report, String> {
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", out.display());
    let file = &p.file;
    // (a footer across a sector of the file written over, from where it starts)
    if !p.kill {
        if a.verbose {
            eprintln!("{}: its footer across a sector of the file: written over", out.display());
        }
        file.set_len(p.at.start + p.footer.start as u64).map_err(|e| err(&exit::seen(e)))?;
    }
    let end = p.at.end;
    let _undo = stop::undo_by(stop::Undo::Truncate(out.to_owned(), end));
    let mut f = file;
    std::io::Seek::seek(&mut f, std::io::SeekFrom::End(0)).map_err(|e| err(&exit::seen(e)))?;
    let from =
        aqr_compress::Appending { footer: p.footer, metadata: p.metadata, link_groups: p.link_groups, kill: p.kill };
    let placed = std::sync::atomic::AtomicBool::new(true);
    let w = std::io::BufWriter::with_capacity(8 << 20, file);
    let mut o = FileOut { w, commits: true, once: a.atomic, base: p.at.start, place: None, placed: &placed };
    // (failing, what was written after the last commit is left, cut off by the next append)
    let mut r =
        viewed(a, view, |tell| aqr_compress::compress_tree_into(&g.tree, g, opt, &mut o, Some(&from), stopping, tell))
            .map_err(|e| err(&exit::seen(e)))?;
    let old = (end - p.at.start) as usize;
    (r.len, r.overhead) = (r.len.saturating_sub(old), r.overhead.saturating_sub(old));
    Ok(r)
}

/// Of archive `out` of several files (`files` files of `total` bytes), stopped and ended as it
/// is ([`aqr_compress::Stop`]): placed where it was not (but where nothing was kept), what it
/// holds told, the process ended (exit code 130, as a shell tells a process stopped by Ctrl-C).
fn stopped_tree(
    a: &Args,
    out: &Path,
    force: bool,
    placed: &std::sync::atomic::AtomicBool,
    (files, total): (u64, u64),
    r: &aqr_compress::Report,
) -> ! {
    const STOPPED: i32 = 130;
    let s = r.stopped.expect("an archive stopped");
    if !s.kept {
        if !a.quiet {
            eprintln!("aqr: stopped: nothing archived yet, {} not made", out.display());
        }
        ipc::tell(serde_json::json!({"event": "stopped", "kept": null}));
        stop::now(STOPPED);
    }
    if !placed.swap(true, std::sync::atomic::Ordering::SeqCst) {
        let mut name = out.file_name().unwrap_or_default().to_owned();
        name.push(format!(".{}.part", std::process::id()));
        if let Err(e) = place(&out.with_file_name(name), out, force) {
            eprintln!("aqr: {e}");
            stop::now(1);
        }
    }
    let (kept, kept_bytes) = (files.saturating_sub(s.files), total.saturating_sub(s.bytes));
    if !a.quiet {
        let left = if s.files == 1 { "1 file".to_string() } else { format!("{} files", s.files) };
        let (kept_size, left_size) = (size(kept_bytes as f64), size(s.bytes as f64));
        match a.append {
            true => eprintln!(
                "aqr: stopped: {kept} of {files} files ({kept_size}) appended to {}; {left} ({left_size}) not (aqr {} adds them)",
                out.display(),
                a.verb()
            ),
            false => eprintln!(
                "aqr: stopped: {} holds {kept} of {files} files ({kept_size}); {left} ({left_size}) not archived (aqr -a adds them)",
                out.display()
            ),
        }
    }
    ipc::tell(serde_json::json!({"event": "stopped", "kept": {"files": kept, "bytes": kept_bytes}}));
    stop::exit(STOPPED)
}

/// An archive of several files written to a file, committed as it is written where `commits`
/// ([`aqr_compress::Output`]): `part`, placed at its output at its first commit ([`place`],
/// [`stop::placed`]), as `placed` then tells.
struct FileOut<'f> {
    w: std::io::BufWriter<&'f fs::File>,
    /// Whether it is committed as it is written, once as it ends (an archive appended to with
    /// `--atomic`), and where the archive starts in the file.
    commits: bool,
    once: bool,
    base: u64,
    /// Where it is to be placed (`part`, its output, whether over a file there): none where it is
    /// in place (an archive appended to).
    place: Option<(PathBuf, PathBuf, bool)>,
    placed: &'f std::sync::atomic::AtomicBool,
}

impl Write for FileOut<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.w.write(buf)
    }

    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        self.w.write_all(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.w.flush()
    }
}

impl aqr_compress::Output for FileOut<'_> {
    fn base(&self) -> u64 {
        self.base
    }

    fn commits(&self) -> bool {
        self.commits
    }

    fn commits_once(&self) -> bool {
        self.once
    }

    fn sync(&mut self) -> std::io::Result<()> {
        self.w.flush()?;
        sync_file(self.w.get_ref())?;
        if self.commits
            && let Some((part, out, force)) = &self.place
            && !self.placed.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            place(part, out, *force).map_err(std::io::Error::other)?;
            stop::placed(stop::Undo::Remove(out.clone()));
        }
        Ok(())
    }

    fn patch(&mut self, at: u64, bytes: &[u8]) -> std::io::Result<()> {
        self.w.flush()?;
        write_at(self.w.get_ref(), self.base + at, bytes)
    }
}

/// Makes what was written to `f` durable, in the order written (on macOS, where `fsync` leaves
/// it to the drive, through its cache).
fn sync_file(f: &fs::File) -> std::io::Result<()> {
    #[cfg(target_vendor = "apple")]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: a descriptor of a file held
        if unsafe { libc::fcntl(f.as_raw_fd(), libc::F_FULLFSYNC) } == 0 {
            return Ok(());
        }
    }
    f.sync_data()
}

/// Writes `bytes` at `at` of `f` (written over), its position left at its end.
fn write_at(f: &fs::File, at: u64, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    std::os::unix::fs::FileExt::write_all_at(f, bytes, at)?;
    #[cfg(windows)]
    {
        let (mut at, mut b) = (at, bytes);
        while !b.is_empty() {
            let n = std::os::windows::fs::FileExt::seek_write(f, b, at)?;
            (at, b) = (at + n as u64, &b[n..]);
        }
    }
    // (Windows moves it)
    std::io::Seek::seek(&mut &*f, std::io::SeekFrom::End(0)).map(|_| ())
}

/// An encrypted archive ([`sealer`]) as an [`aqr_compress::Output`]: not committed, its last
/// chunk sealed as it ends.
struct Sealed<'f>(aqr::envelope::Sealer<&'f mut fs::File>);

impl Write for Sealed<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

impl aqr_compress::Output for Sealed<'_> {
    fn end(&mut self) -> std::io::Result<()> {
        self.0.finish()
    }
}

/// [`write_whole`], but where `write` places the file itself (it tells so: [`FileOut`]), not
/// placed again, and left there where it fails then (holding what was committed).
fn write_placing<T>(
    path: &Path,
    force: bool,
    write: impl FnOnce(&mut fs::File, &Path) -> std::io::Result<(T, bool)>,
) -> Result<T, String> {
    let err = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    let mut name = path.file_name().ok_or(format!("{}: not a file's name", path.display()))?.to_owned();
    name.push(format!(".{}.part", std::process::id()));
    let part = path.with_file_name(name);
    let open = || fs::OpenOptions::new().read(true).write(true).create_new(true).open(&part);
    let (mut file, stop) = stop::create(&part, open).map_err(|e| err(&part, e))?;
    let mut placed = false;
    let r = write(&mut file, &part).map_err(|e| err(path, e)).and_then(|(t, p)| {
        placed = p;
        drop(file);
        if !p {
            place(&part, path, force)?;
        }
        Ok(t)
    });
    if r.is_err() && !placed {
        let _ = fs::remove_file(&part);
    }
    drop(stop);
    r
}

/// Puts `from` in place of the file `to`, which keeps its identity (its attributes, creation
/// time, ACL and named streams) as it takes `from`'s bytes: `ReplaceFileW` on Windows, a rename
/// elsewhere (which keeps nothing of `to` but its name, on Windows too).
pub(crate) fn replace(from: &Path, to: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{
            REPLACEFILE_IGNORE_ACL_ERRORS, REPLACEFILE_IGNORE_MERGE_ERRORS, ReplaceFileW,
        };
        let wide = |p: &Path| p.as_os_str().encode_wide().chain([0]).collect::<Vec<u16>>();
        let (f, t) = (wide(from), wide(to));
        // SAFETY: both are NUL-terminated paths; the other pointers are null (no backup)
        let ok = unsafe {
            ReplaceFileW(
                t.as_ptr(),
                f.as_ptr(),
                std::ptr::null(),
                REPLACEFILE_IGNORE_MERGE_ERRORS | REPLACEFILE_IGNORE_ACL_ERRORS,
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        if ok != 0 {
            return Ok(());
        }
        // (a file system without it, such as FAT: what a rename does)
        place(from, to, true)
    }
    #[cfg(not(windows))]
    place(from, to, true)
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
    let r = decompress_each(a);
    stdio::remove();
    r
}

fn decompress_each(a: &Args) -> Result<(), String> {
    // (standard input read whole first, for now)
    let spooled = match a.stdin {
        true => vec![stdio::spool_in()?],
        false => Vec::new(),
    };
    for input in if a.stdin { &spooled } else { &a.inputs } {
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
            _ if a.stdout && foreign.is_some() => {
                return Err(format!("{}: -o - (standard output) of Aqueous archives alone", input.display()));
            }
            _ if a.stdout => To { named: PathBuf::from("-"), base: a.dir.clone() },
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
        // (as it is before it is read: kept where the output is put in its place)
        let gone = match a.rm {
            true => {
                Some(Gone::of(input, &fs::symlink_metadata(input).map_err(|e| format!("{}: {e}", input.display()))?))
            }
            false => None,
        };
        let done = match foreign {
            Some((f, _)) => decompress_foreign(a, input, &to, f)?,
            None => decompress_into(a, input, &to)?,
        };
        if done && let Some(g) = gone {
            remove(&[g])?;
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
/// is ([`extract::Extractor`]); an error at the end where entries were left out. Whether it
/// was (not skipped, as a frontend answers).
fn decompress_foreign(a: &Args, input: &Path, to: &To, format: aqr_unpack::Format) -> Result<bool, String> {
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", stdio::shown(input));
    let file = fs::File::open(input).map_err(|e| err(&exit::seen(e)))?;
    // SAFETY: the archive is not to change while it is read; where it does, its checksums tell
    let c = unsafe { memmap2::Mmap::map(&file) }.map_err(|e| err(&exit::seen(e)))?;
    let mut opened = aqr_unpack::Opened::open_with(format, &c, &a.names.encoding).map_err(|e| err(&exit::seen(e)))?;
    tell_names(a, input, opened.names_told());
    if a.select.is_some() && !opened.is_tree() {
        return Err(err(&"a single file, not an archive of paths"));
    }
    // (its entries encrypted without a passphrase taken are left out, told)
    if opened.encrypted()
        && pass::keys()
            .open(input, |p| opened.unlock(p).map(|ok| ok.then_some(())).map_err(|e| err(&exit::seen(e))))?
            .is_none()
        && opened.sealed()
    {
        exit::note(exit::Code::Decryption);
        return Err(err(&pass::keys().none_opens()));
    }
    // (some paths: checked where the listing is known, else once the archive is read)
    if let Some(s) = &a.select {
        if let Some(t) = opened.tree() {
            s.check(t, &|k| opened.left_out(&s.unpacking(Some(k)))).map_err(|e| err(&exit::seen(e)))?;
        }
        opened.select(s.unpacking(None));
    }
    let ask = |p: &Path| output(a, p);
    let (place, force) = match (a.test, opened.is_tree()) {
        (true, _) => (None, false),
        (false, true) => match to.place(a, true, &ask)? {
            Some((p, f)) => (Some(p), f),
            None => return Ok(false),
        },
        (false, false) => match output(a, &to.named)? {
            Some(f) => (None, f),
            None => return Ok(false),
        },
    };
    let told = match &place {
        Some(p) => shown(p),
        None => (!a.test).then_some(&*to.named),
    };
    let told = told.map(|t| if a.stdout { Path::new("-") } else { t });
    begin(if a.test { "test" } else { "decompress" }, &[input.to_owned()], told);
    let start = Instant::now();
    let rec = stats::record(
        a.stats,
        if a.test { "test" } else { "decompress" },
        input.display().to_string(),
        told.map_or(String::new(), |o| o.display().to_string()),
        a.opt.threads,
    );
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
            .map_err(|e| err(&exit::seen(e)))?
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
                Some(p) => Some(
                    extract::Extractor::new(p, force, a.links, &ap, &seen, a.select.clone(), &a.names)?.keeping(input),
                ),
            };
            let sink =
                Unpacking { x: x.as_ref(), input, quiet: a.quiet, view: shown, seen: &seen, sizes: Default::default() };
            let done = match opened.unpack(threads, &sink) {
                Err(_) if x.as_ref().is_some_and(|x| x.skipped()) => return Ok(None),
                r => r.map_err(|e| err(&exit::seen(e)))?,
            };
            // (of a tar archive; where entries were left out, told, and the error at the end)
            if let Some(s) = &a.select
                && done.skipped == 0
            {
                s.check(&done.tree, &|_| Vec::new()).map_err(|e| err(&exit::seen(e)))?;
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
            None => return Ok(false),
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
            fallbacks: Vec::new(),
            trace: rec.finish(),
        });
    }
    if skipped > 0 {
        let what = if skipped == 1 { "entry" } else { "entries" };
        exit::note(exit::Code::Incomplete);
        return Err(format!("{}: {skipped} {what} not extracted (told above)", input.display()));
    }
    Ok(true)
}

/// The archive in file `c` (`input`) as it is located (spec, "Locating an archive": an archive
/// of a file appended to and not committed, one after other bytes, as a self-extracting
/// executable has it), the bytes before or after it told where not `quiet`; all of `c` where
/// none is found (to be repaired), or where it is encrypted.
pub(crate) fn located<'c>(c: &'c [u8], input: &Path, quiet: bool) -> &'c [u8] {
    let Some(r) = aqr::locate(c) else { return c };
    if !quiet && r.start > 0 {
        eprintln!("{}: {} before the archive left out", input.display(), bytes_of(r.start as u64));
    }
    if !quiet && r.end < c.len() {
        let n = bytes_of((c.len() - r.end) as u64);
        eprintln!(
            "{}: {n} after the archive left out (bytes added to it, or an append not committed)",
            input.display()
        );
    }
    &c[r]
}

/// `n` bytes, told.
fn bytes_of(n: u64) -> String {
    if n == 1 { "1 byte".into() } else { format!("{n} bytes") }
}

/// What the last segment of `archive` (`input`) tells where its writing did not complete
/// (`segment-status`), told: the files it left out.
fn tell_incomplete(status: Option<aqr::Status>, input: &Path) {
    if let Some(aqr::Status::Incomplete { files, bytes }) = status {
        let left = match (files, bytes) {
            (Some(1), Some(b)) => format!(": 1 file ({}) not archived", size(b as f64)),
            (Some(n), Some(b)) => format!(": {n} files ({}) not archived", size(b as f64)),
            (Some(n), None) => format!(": {n} file{} not archived", if n == 1 { "" } else { "s" }),
            _ => String::new(),
        };
        eprintln!("{}: incomplete, its writing stopped{left} (aqr -a adds them)", stdio::shown(input));
    }
}

/// An archive read whole to be repaired where it is damaged: of an encrypted archive, its payload
/// decrypted (its chunks that do not authenticate too, erased), its mirror blocks checked, and its
/// bytes repaired from them where they differ (its footer made again where it is cut short after
/// its last metadata block).
struct Repair {
    decrypted: Option<memmap2::Mmap>,
    erased: Vec<std::ops::Range<usize>>,
    mirrors: aqr::mirror::Check,
    restored: Option<aqr::mirror::Repaired>,
}

impl Repair {
    /// The archive of `input` (`plain`, or the payload `decrypting`) read whole and repaired, once
    /// (into `cell`), its repair told.
    fn of<'r>(
        cell: &'r std::cell::OnceCell<Repair>,
        input: &Path,
        plain: &[u8],
        decrypting: Option<&aqr::envelope::Decrypting>,
        threads: usize,
    ) -> Result<&'r Repair, String> {
        if let Some(r) = cell.get() {
            return Ok(r);
        }
        let (decrypted, erased) = match decrypting {
            Some(d) => {
                let (d, erased) = decrypt_whole(d, input, threads)?;
                (Some(d), erased)
            }
            None => (None, Vec::new()),
        };
        let read = decrypted.as_deref().unwrap_or(plain);
        let mirrors = aqr::mirror::check(read);
        let restored = aqr::mirror::repair(read, &mirrors, true, &erased);
        if let Some(r) = &restored {
            if r.header {
                eprintln!("{}: its header magic restored", input.display());
            }
            if r.restored > 0 {
                let s = if r.restored == 1 { "" } else { "s" };
                eprintln!("{}: {} damaged block{s} restored from mirror blocks", input.display(), r.restored);
            }
            if r.footer {
                eprintln!("{}: its footer made again", input.display());
            }
            if !r.lost.is_empty() {
                let n: usize = r.lost.iter().map(|l| l.len()).sum();
                eprintln!("{}: {n} bytes lost; salvaging what is left", input.display());
            }
        }
        Ok(cell.get_or_init(|| Repair { decrypted, erased, mirrors, restored }))
    }

    /// The archive's bytes as they are (`plain`, or the payload decrypted).
    fn damaged<'r>(&'r self, plain: &'r [u8]) -> &'r [u8] {
        self.decrypted.as_deref().unwrap_or(plain)
    }

    /// The archive's bytes repaired.
    fn read<'r>(&'r self, plain: &'r [u8]) -> &'r [u8] {
        self.restored.as_ref().map_or(self.damaged(plain), |r| &r.bytes)
    }

    /// The bytes of the segment cut short, where one is.
    fn cut_off(&self) -> Option<usize> {
        self.restored.as_ref().and_then(|r| r.cut_off.as_ref()).map(|c| c.len())
    }

    /// The archive opened, of its bytes repaired: salvaged where bytes were lost (their files
    /// lost, the others given back as they check).
    fn open<'r, 'n>(
        &'r self,
        plain: &'r [u8],
        runner: &'n dyn aqr::Runner,
        upto: Option<usize>,
    ) -> Result<aqr::Archive<'r, 'n>, Failure> {
        let read = self.read(plain);
        match (self.restored.as_ref().is_some_and(|r| !r.lost.is_empty()), self.decrypted.is_some(), upto) {
            (true, sealed, _) => aqr::open_salvaging(read, runner, sealed),
            (false, true, _) => aqr::open_payload(read, runner),
            (false, false, Some(m)) => aqr::open_upto(read, runner, m),
            (false, false, None) => aqr::open(read, runner),
        }
    }
}

/// Archive `p` opened by its index ([`aqr::open_indexed`]), of `paths` alone where they are given
/// ([`aqr::open_indexed_at`]).
fn by_index<'c, 'n>(
    p: aqr::Payload<'c>,
    runner: &'n dyn aqr::Runner,
    threads: usize,
    paths: Option<&[Vec<Vec<u8>>]>,
    upto: Option<usize>,
) -> Result<aqr::Archive<'c, 'n>, Failure> {
    match (upto, paths) {
        // (the archive ended at the point: all of what is before it read)
        (Some(m), _) => aqr::open_indexed_upto(p, runner, threads, m),
        (None, Some(paths)) => aqr::open_indexed_at(p, runner, threads, paths),
        (None, None) => aqr::open_indexed(p, runner, threads),
    }
}

/// Decodes (or tests) `input`, an Aqueous archive, to `to`: whether it was (not skipped, as a
/// frontend answers).
fn decompress_into(a: &Args, input: &Path, to: &To) -> Result<bool, String> {
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", stdio::shown(input));
    let file = fs::File::open(input).map_err(|e| err(&exit::seen(e)))?;
    // (mapped, so that what is read of it is the system's to drop)
    // SAFETY: the archive is not to change while it is read; where it does, its checksums tell
    let c = unsafe { memmap2::Mmap::map(&file) }.map_err(|e| err(&exit::seen(e)))?;
    // an encrypted one decrypted as it is read, into memory (not backed by a file, so that
    // nothing decrypted is written out but the output)
    let decrypting = match aqr::envelope::is_encrypted(&c) || aqr::envelope::is_damaged_encrypted(&c) {
        true => Some(unlock(&c, input)?),
        false => None,
    };
    let decoder =
        Decoder { native: Native { threads: a.opt.threads }, wasm: a.wasm, sandbox: a.sandbox.then(OnceLock::new) };
    let runner: &dyn aqr::Runner = &decoder;
    let plain: &[u8] = match decrypting {
        Some(_) => &[],
        None => located(&c, input, a.quiet),
    };
    // of a tree, read by its index: the blocks of the files taken alone (but tested: read whole,
    // its mirror blocks checked too; and of a single file, read whole anyway)
    let payload = match &decrypting {
        Some(d) => aqr::Payload::Sealed(d),
        None => aqr::Payload::Plain(plain),
    };
    // (of paths selected, its metadata blocks read back from the last as far as they tell them)
    let paths: Option<Vec<Vec<Vec<u8>>>> = a.select.as_ref().map(|s| s.paths.iter().map(|p| p.0.clone()).collect());
    // (the archive as it was at a journal point: ended after the segment of the point)
    let upto = match &a.at {
        Some(at) => Some(journal::upto(input, if decrypting.is_some() { &c } else { plain }, at, a.quiet)?),
        None => None,
    };
    let indexed = match !a.test && payload.of_tree() {
        true => by_index(payload, runner, a.opt.threads, paths.as_deref(), upto).ok(),
        false => None,
    };
    let fast = indexed.is_some();
    // read whole and repaired where it is damaged: where it is tested, where it does not open by
    // its index, and for its files that do not decode so
    let repair: std::cell::OnceCell<Repair> = std::cell::OnceCell::new();
    let mut archive = match indexed {
        Some(archive) => archive,
        None => Repair::of(&repair, input, plain, decrypting.as_ref(), a.opt.threads)?
            .open(plain, runner, upto)
            .map_err(|e| err(&exit::seen(e)))?,
    };
    let cut_off = repair.get().and_then(|r| r.cut_off());
    let footer_made = repair.get().is_some_and(|r| r.restored.as_ref().is_some_and(|r| r.footer));
    // (its footer made again after a metadata block not of the segment meant to be the last:
    // cut short just after it, maybe, segments after it lost whole; after a commit of a writing
    // that went on, too, as one stopped there is not told apart)
    let maybe_cut = footer_made && cut_off.is_none() && archive.status() != Some(aqr::Status::Complete);
    if !a.quiet {
        tell_incomplete(archive.status(), input);
    }
    if maybe_cut {
        eprintln!(
            "{}: its last segment is not the one meant to be the last: segments after it may be lost",
            stdio::shown(input)
        );
    }
    // (some paths: by content, whether it is decoded)
    let wanted = match (&a.select, archive.tree()) {
        (None, _) => None,
        (Some(_), None) => return Err(err(&"an archive of a single file, not of paths")),
        (Some(s), Some(t)) => {
            s.check(t, &|_| Vec::new()).map_err(|e| err(&exit::seen(e)))?;
            Some(s.contents(t, archive.contents_len()))
        }
    };
    let ask = |p: &Path| output(a, p);
    let (place, force) = match (a.test, archive.tree().is_some()) {
        (true, _) => (None, false),
        _ if a.stdout => (None, false),
        (false, true) => match to.place(a, false, &ask)? {
            Some((p, f)) => (Some(p), f),
            None => return Ok(false),
        },
        (false, false) => match output(a, &to.named)? {
            Some(f) => (None, f),
            None => return Ok(false),
        },
    };
    let told = match &place {
        Some(p) => shown(p),
        None => (!a.test).then_some(&*to.named),
    };
    let told = told.map(|t| if a.stdout { Path::new("-") } else { t });
    begin(if a.test { "test" } else { "decompress" }, &[input.to_owned()], told);
    let start = Instant::now();
    let rec = stats::record(
        a.stats,
        if a.test { "test" } else { "decompress" },
        stdio::shown(input).to_string(),
        told.map_or(String::new(), |o| o.display().to_string()),
        a.opt.threads,
    );
    let names = if !a.quiet && (bars() || a.ipc) { leaf_names(&archive, &to.named) } else { Vec::new() };
    let view = std::sync::Arc::new(view::Decoding::new(bars(), names));
    if !a.quiet && view.shows() {
        let view = view.clone();
        archive.on_progress(Some(std::sync::Arc::new(move |r| view.tell(r))));
    }
    // (of a single file, the leaves all held for the file anyway)
    if archive.tree().is_some() {
        archive.set_budget(aqr_common::budget::global());
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
    // (the contents lost, each with why)
    let mut lost: Vec<(usize, Failure)> = Vec::new();
    let (files, total, out) = match archive.tree() {
        // (written as it is evaluated: a file of parts is not held whole)
        None if a.test => {
            let n = ticking(a, &*view, || {
                archive.write_file(a.opt.threads, &mut |d: &[u8]| {
                    seen.saw(0, d, 0);
                    Ok(())
                })
            })
            .map_err(|e| err(&exit::seen(e)))?;
            (1, n, None)
        }
        None if a.stdout => {
            let mut w = std::io::BufWriter::with_capacity(1 << 20, std::io::stdout());
            let mut failed = None;
            let r = ticking(a, &*view, || {
                archive.write_file(a.opt.threads, &mut |d: &[u8]| {
                    seen.saw(0, d, 0);
                    w.write_all(d).map_err(|e| {
                        let m = e.to_string();
                        failed = Some(e);
                        Failure::Resource(m)
                    })
                })
            });
            let n = match (r, failed) {
                (_, Some(e)) => return Err(format!("standard output: {e}")),
                (r, None) => r.map_err(|e| err(&exit::seen(e)))?,
            };
            w.flush().map_err(|e| format!("standard output: {e}"))?;
            (1, n, Some(PathBuf::from("-")))
        }
        Some(t) if a.stdout => {
            // the one file of the paths given
            let picked = a.select.as_ref().map(|s| s.picked(t));
            let picked = picked.ok_or_else(|| err(&"an archive of several files: -o - (standard output) of a PATH"))?;
            let files: Vec<usize> = (t.entries.iter().zip(&picked))
                .filter_map(|(e, &p)| match e.node {
                    EntryNode::File(c) if p => Some(c),
                    _ => None,
                })
                .collect();
            let [c] = files[..] else {
                return Err(err(&format!("{} files given: -o - (standard output) of one", files.len())));
            };
            let mut only = vec![false; archive.contents_len()];
            only[c] = true;
            let given = std::sync::Mutex::new(vec![false; archive.contents_len()]);
            let w = std::sync::Mutex::new(std::io::BufWriter::with_capacity(1 << 20, std::io::stdout()));
            let total = std::sync::atomic::AtomicU64::new(0);
            // (failing to write it: an error of its own, not the file lost)
            let failed = std::sync::Mutex::new(None);
            let r = ticking(a, &*view, || {
                archive.contents_of(a.opt.threads, Some(&only), |c, d| {
                    seen.saw(c, &d, d.len() as u64);
                    w.lock().unwrap().write_all(&d).map_err(|e| {
                        let m = format!("standard output: {e}");
                        failed.lock().unwrap().get_or_insert(m.clone());
                        Failure::Resource(m)
                    })?;
                    given.lock().unwrap()[c] = true;
                    total.fetch_add(d.len() as u64, std::sync::atomic::Ordering::Relaxed);
                    Ok(())
                })
            });
            if let Some(m) = failed.into_inner().unwrap() {
                return Err(m);
            }
            w.into_inner().unwrap().flush().map_err(|e| format!("standard output: {e}"))?;
            lost = not_given(r, &given.into_inner().unwrap(), Some(&only)).map_err(|e| err(&exit::seen(e)))?;
            (1, total.into_inner(), Some(PathBuf::from("-")))
        }
        None => {
            let n = write_whole(&to.named, force, |f| {
                // (runs of zeros left as holes)
                let mut w = sparse::Sparse::new(f);
                let mut failed = None;
                let r = ticking(a, &*view, || {
                    archive.write_file(a.opt.threads, &mut |d: &[u8]| {
                        seen.saw(0, d, 0);
                        let _s = aqr_common::trace::span("write");
                        w.write(d).map_err(|e| {
                            let m = e.to_string();
                            failed = Some(e);
                            Failure::Resource(m)
                        })
                    })
                });
                match (r, failed) {
                    (_, Some(e)) => Err(e),
                    (Ok(n), None) => w.finish().map(|()| n),
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
            let given = std::sync::Mutex::new(vec![false; archive.contents_len()]);
            let r = ticking(a, &*view, || {
                archive.contents_of(a.opt.threads, wanted.as_deref(), |c, d| {
                    seen.saw(c, &d, d.len() as u64);
                    given.lock().unwrap()[c] = true;
                    n.fetch_add(files[c], std::sync::atomic::Ordering::Relaxed);
                    total.fetch_add(files[c] as u64 * d.len() as u64, std::sync::atomic::Ordering::Relaxed);
                    Ok(())
                })
            });
            lost = not_given(r, &given.into_inner().unwrap(), wanted.as_deref()).map_err(|e| err(&exit::seen(e)))?;
            (n.into_inner(), total.into_inner(), None)
        }
        Some(t) => {
            let ap = meta::Applier { privileged: a.preserve, quiet: a.quiet };
            let r = ticking(a, &*view, || {
                let x =
                    extract::Extractor::new(place.unwrap(), force, a.links, &ap, &seen, a.select.clone(), &a.names)?
                        .keeping(input);
                match x.entries(t, &(0..t.entries.len()).collect::<Vec<_>>()) {
                    Err(_) if x.skipped() => return Ok(None),
                    r => r?,
                }
                // (a file of a leaf failing lost, the others extracted; but where writing fails)
                let given = std::sync::Mutex::new(vec![false; archive.contents_len()]);
                let writing = std::sync::Mutex::new(None);
                // (stopped as a signal or the frontend asks: the files written whole kept)
                let (_stopping, requested) = stop::extracting();
                let stopped = || requested.load(std::sync::atomic::Ordering::SeqCst);
                // (for tests: stopped once so many contents are written, as a signal would)
                let after = std::env::var("AQR_TEST_STOP_AFTER").ok().and_then(|v| v.parse::<usize>().ok());
                let written = std::sync::atomic::AtomicUsize::new(0);
                let give = |c, d| {
                    if stopped() {
                        return Err(Failure::Resource("stopped".into()));
                    }
                    given.lock().unwrap()[c] = true;
                    x.content(c, d).map_err(|m| {
                        writing.lock().unwrap().get_or_insert(m.clone());
                        Failure::Resource(m)
                    })?;
                    if after.is_some_and(|n| written.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1 >= n) {
                        requested.store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                    Ok(())
                };
                let mut r = archive.contents_of(a.opt.threads, wanted.as_deref(), give);
                // (read by its index, the files that do not decode so, of it read whole and repaired)
                let missing = |given: &[bool]| -> Vec<bool> {
                    (0..given.len()).map(|c| !given[c] && wanted.as_ref().is_none_or(|w| w[c])).collect()
                };
                if fast && r.is_err() && writing.lock().unwrap().is_none() && !stopped() {
                    let again = missing(&given.lock().unwrap());
                    let repaired = Repair::of(&repair, input, plain, decrypting.as_ref(), a.opt.threads)?;
                    // (by its index too, so that its contents are numbered as they were, the files
                    // of blocks lost failing alone; of the whole tree, else salvaged)
                    let read = repaired.read(plain);
                    let whole = match by_index(aqr::Payload::Plain(read), runner, a.opt.threads, paths.as_deref(), upto)
                    {
                        Ok(whole) => Some(whole),
                        Err(_) if paths.is_none() => repaired.open(plain, runner, upto).ok(),
                        Err(_) => None,
                    };
                    if let Some(mut whole) = whole.filter(|w| w.contents_len() == again.len()) {
                        whole.set_budget(aqr_common::budget::global());
                        r = whole.contents_of(a.opt.threads, Some(&again), give);
                    }
                }
                if let Some(m) = writing.into_inner().unwrap() {
                    return Err(m);
                }
                if stopped() {
                    let files = t.entries.iter().filter(|e| matches!(e.node, EntryNode::File(_))).count();
                    let (done, removed) = x.stop(t)?;
                    let (n, bytes, out) = done.unwrap_or_default();
                    if !a.quiet {
                        eprintln!(
                            "aqr: stopped: {n} of {files} files ({}) extracted into {}; {removed} not written whole removed",
                            size(bytes as f64),
                            out.display()
                        );
                    }
                    ipc::tell(serde_json::json!({"event": "stopped", "kept": {"files": n, "bytes": bytes}}));
                    stop::exit(130);
                }
                let lost = not_given(r, &given.into_inner().unwrap(), wanted.as_deref())
                    .map_err(|e| exit::seen(e).to_string())?;
                lost.iter().try_for_each(|&(c, _)| x.lose(c))?;
                tell_names(a, input, x.names_told());
                Ok(x.finish(t)?.map(|r| (r, lost)))
            })?;
            match r {
                Some(((n, total, out), l)) => {
                    lost = l;
                    (n, total, Some(out))
                }
                None => return Ok(false),
            }
        }
    };
    let out = out.as_deref();
    // the files of a segment cut short, of no name known: each leaf's outputs (into a directory
    // of their own beside the tree, where the archive is extracted whole)
    if let Some(r) = repair.get()
        && let Some(cut) = r.restored.as_ref().and_then(|r| r.cut_off.as_ref())
    {
        let into = match (out, archive.tree()) {
            (Some(o), Some(t)) if a.select.is_none() && !a.test => Some(unnamed_dir(o, t)),
            _ => None,
        };
        let (n, bytes) = salvage_unnamed(a, r.damaged(plain), cut, &r.erased, runner, into.as_deref())
            .map_err(|e| err(&exit::seen(e)))?;
        if n > 0 {
            let s = if n == 1 { "" } else { "s" };
            let (what, bytes) = (stdio::shown(input), size(bytes as f64));
            match &into {
                Some(d) => eprintln!(
                    "{what}: {n} file{s} of no name ({bytes}) salvaged from the segment cut short, into {}",
                    d.display()
                ),
                None => eprintln!("{what}: {n} file{s} of no name ({bytes}) salvaged from the segment cut short"),
            }
        }
    }
    if !a.quiet {
        summary_d(input, out, files, total, start);
        // (of an archive whose files are all there)
        if a.test
            && lost.is_empty()
            && let Some(r) = repair.get()
            && (a.verbose || !r.mirrors.mirrors.is_empty() || r.mirrors.broken > 0)
        {
            tell_archival(input, &archive, &r.mirrors, r.read(plain), decrypting.is_some().then_some(&c[..]))
                .map_err(|e| err(&exit::seen(e)))?;
        }
    }
    end(out, files, c.len() as u64, total, start);
    if !lost.is_empty() {
        // (each told, by the path of its first file, however quiet)
        let paths = content_paths(&archive, &to.named);
        for (c, f) in &lost {
            exit::note(exit::Code::of(f));
            eprintln!("{}: {}: lost ({f})", stdio::shown(input), paths[*c]);
        }
        let n = lost.len();
        return Err(err(&format!("{n} file{} lost", if n == 1 { "" } else { "s" })));
    }
    // (salvaged, its files given back, but the blocks of a segment cut short lost with it, of
    // files of no name known)
    if let Some(n) = cut_off {
        exit::note(exit::Code::Invalid);
        return Err(err(&format!("a segment cut short lost ({n} bytes), its files not known")));
    }
    if maybe_cut {
        exit::note(exit::Code::Invalid);
        return Err(err(&"cut short after a segment, maybe: segments after it not known"));
    }
    if let (Some(s), Some(rec)) = (&a.stats, rec) {
        let mut contents = seen.contents(content_paths(&archive, &to.named));
        if archive.tree().is_none()
            && let Some(c) = contents.first_mut()
        {
            c.len = total;
        }
        s.add(stats::Run {
            mode: if a.test { "test" } else { "decompress" },
            input: stdio::shown(input).to_string(),
            output: out.map_or(String::new(), |o| o.display().to_string()),
            seconds: start.elapsed().as_secs_f64(),
            threads: a.opt.threads,
            read: c.len() as u64,
            written: total,
            overhead: None,
            contents,
            fallbacks: Vec::new(),
            trace: rec.finish(),
        });
    }
    Ok(true)
}

/// A directory in `out` (where tree `t` was extracted) for the files of no name of a segment cut
/// short: `#salvaged`, or `#salvaged-2` and on where the tree or `out` has that name.
fn unnamed_dir(out: &Path, t: &Tree) -> PathBuf {
    (1..)
        .map(|k| if k == 1 { "#salvaged".to_string() } else { format!("#salvaged-{k}") })
        .find(|n| !t.entries.iter().any(|e| e.parent.is_none() && e.name == n.as_bytes()) && !out.join(n).exists())
        .map(|n| out.join(n))
        .expect("a name free")
}

/// The leaves of the segment of `damaged` (an archive as read, before repair) that repair cut
/// off at `cut`, its bytes `erased` not to be trusted, evaluated each alone
/// ([`aqr::mirror::cut_segment`]): each output a file of no name, written into directory `into`
/// (made) where given, as `leaf-N` (`leaf-N-K` of a leaf of several outputs). Returns how many,
/// and their bytes.
fn salvage_unnamed(
    a: &Args,
    damaged: &[u8],
    cut: &std::ops::Range<usize>,
    erased: &[std::ops::Range<usize>],
    runner: &dyn aqr::Runner,
    into: Option<&Path>,
) -> Result<(usize, u64), String> {
    let Some(c) = aqr::mirror::cut_segment(damaged, cut, erased) else { return Ok((0, 0)) };
    let mut archive = aqr::open_salvaging(&c, runner, false).map_err(|e| e.to_string())?;
    archive.set_budget(aqr_common::budget::global());
    if let Some(d) = into {
        fs::create_dir(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    let got = std::sync::Mutex::new((0usize, 0u64, None::<String>));
    // (a leaf failing lost, as its files would be: nothing tells which they were)
    let _ = archive.each_leaf(a.opt.threads, |i, r| {
        let Ok(outputs) = r else { return Ok(()) };
        let several = outputs.len() > 1;
        for (k, d) in outputs.iter().enumerate() {
            if let Some(dir) = into {
                let name = if several { format!("leaf-{}-{}", i + 1, k + 1) } else { format!("leaf-{}", i + 1) };
                let p = dir.join(name);
                if let Err(e) = fs::File::create_new(&p).and_then(|mut f| f.write_all(d)) {
                    got.lock().unwrap().2.get_or_insert(format!("{}: {e}", p.display()));
                    return Err(Failure::Resource(e.to_string()));
                }
            }
            let mut g = got.lock().unwrap();
            (g.0, g.1) = (g.0 + 1, g.1 + d.len() as u64);
        }
        Ok(())
    });
    let (n, bytes, failed) = got.into_inner().unwrap();
    match failed {
        Some(m) => Err(m),
        None => Ok((n, bytes)),
    }
}

/// The contents wanted (all, or those `wanted` marks) not given (`given`) where evaluating them
/// failed (`r`): each lost, with the failure. The failure, where it is of no content.
fn not_given(
    r: Result<(), Failure>,
    given: &[bool],
    wanted: Option<&[bool]>,
) -> Result<Vec<(usize, Failure)>, Failure> {
    let Err(f) = r else { return Ok(Vec::new()) };
    let lost: Vec<(usize, Failure)> = (0..given.len())
        .filter(|&c| !given[c] && wanted.is_none_or(|w| w.get(c) == Some(&true)))
        .map(|c| (c, f.clone()))
        .collect();
    if lost.is_empty() { Err(f) } else { Ok(lost) }
}

/// Tells (on standard error) how far `archive` (of `input`, its bytes `read` as decoded)
/// conforms to the archival profile (spec, "Archival profile"), and whether it is a mirrored
/// archive (spec, "Mirrored archives"; of an encrypted archive's payload where there is an
/// `envelope`, the archive as stored): how many of its blocks have intact mirror blocks
/// (`mirrors`), how many have them only nearer than the spec asks, and how many mirror blocks
/// are not intact.
fn tell_archival(
    input: &Path,
    archive: &aqr::Archive,
    mirrors: &aqr::mirror::Check,
    read: &[u8],
    envelope: Option<&[u8]>,
) -> Result<(), Failure> {
    let p = archive.archival()?;
    let profile = match (p.unchecked_leaves, p.unchecked_formulas) {
        (0, 0) => "conforms".to_string(),
        (l, f) => {
            format!("does not conform ({l} leaf blocks without checksums, {f} formula blocks without output checksums)")
        }
    };
    let sealed = envelope.is_some();
    let mirrored = mirrors.mirrored(read, sealed)
        && envelope.is_none_or(|e| e.starts_with(&aqr::envelope::ENCRYPTED_MIRRORED_HEADER_MAGIC));
    let mut told = format!(
        "{}: archival profile: {profile}; mirrored archive: {}",
        input.display(),
        if mirrored { "yes" } else { "no" }
    );
    if mirrors.walked {
        let n = mirrors.targets.len();
        told += &format!(" ({} of {n} blocks to mirror mirrored", n - mirrors.unmirrored());
    } else {
        told += &format!(" ({} mirror blocks found by their magic", mirrors.mirrors.len());
    }
    let near = mirrors.near(sealed);
    if near > 0 {
        told += &format!(", {near} only nearer than the spec asks");
    }
    match mirrors.broken {
        0 => {}
        1 => told += ", 1 damaged mirror block",
        n => told += &format!(", {n} damaged mirror blocks"),
    }
    told += ")";
    eprintln!("{told}");
    Ok(())
}

/// The payload of encrypted archive `c` (of `input`), decrypted on `threads` threads, by a
/// passphrase given ([`pass`]); damaged ([`aqr::envelope::open_damaged`]), its header from a
/// mirrored header, and its chunks that do not authenticate decrypted without it, told: their
/// bytes in the payload, not to be trusted.
fn decrypt(c: &[u8], input: &Path, threads: usize) -> Result<(memmap2::Mmap, Vec<std::ops::Range<usize>>), String> {
    decrypt_whole(&unlock(c, input)?, input, threads)
}

/// The payload of encrypted archive `c` (of `input`) unlocked by a passphrase given ([`pass`]),
/// to be decrypted as it is read; damaged ([`aqr::envelope::open_damaged`]), its header from a
/// mirrored header, told.
fn unlock<'c>(c: &'c [u8], input: &Path) -> Result<aqr::envelope::Decrypting<'c>, String> {
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", input.display());
    let (e, opened) = aqr::envelope::open_damaged(c).map_err(|e| err(&exit::seen(e)))?;
    if let Some(k) = opened.header_from {
        eprintln!("{}: its encrypted header damaged, taken from mirrored header {k}", input.display());
    }
    if opened.cut {
        eprintln!("{}: cut short in its last encrypted chunk", input.display());
    }
    let unlocked = pass::keys().open(input, |p| match e.unlock(p) {
        Ok(u) => Ok(Some(u)),
        Err(Failure::Decryption(_)) => Ok(None),
        Err(f) => Err(err(&f)),
    })?;
    match unlocked {
        Some(u) => Ok(u.decrypting()),
        None => {
            exit::note(exit::Code::Decryption);
            Err(err(&pass::keys().none_opens()))
        }
    }
}

/// The payload of `d` (of `input`) decrypted whole on `threads` threads, its chunks that do not
/// authenticate decrypted without it, told: their bytes in the payload, not to be trusted.
fn decrypt_whole(
    d: &aqr::envelope::Decrypting,
    input: &Path,
    threads: usize,
) -> Result<(memmap2::Mmap, Vec<std::ops::Range<usize>>), String> {
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", input.display());
    let u = d.unlocked();
    let mut d = memmap2::MmapMut::map_anon(u.len()).map_err(|e| err(&exit::seen(e)))?;
    let failed = u.decrypt_lossy(&mut d, threads);
    if !failed.is_empty() {
        let s = if failed.len() == 1 { "" } else { "s" };
        eprintln!("{}: {} encrypted chunk{s} not authenticating, not trusted", input.display(), failed.len());
    }
    let erased = failed.iter().map(|&m| u.chunk_range(m)).collect();
    Ok((d.make_read_only().map_err(|e| err(&exit::seen(e)))?, erased))
}

/// By content of `archive` (decoded into `out`), the path of its first file in the archive (of
/// a single file, `out`).
fn content_paths(archive: &aqr::Archive, out: &Path) -> Vec<String> {
    let Some(t) = archive.tree() else { return vec![out.display().to_string()] };
    let mut paths = tree_content_paths(t);
    paths.resize(archive.contents_len(), String::new());
    paths
}

/// By content of tree `t` (as far as its files tell), the path of its first file, as the
/// archive has it (its names between `/`, as on any system).
fn tree_content_paths(t: &Tree) -> Vec<String> {
    let mut first: Vec<Option<String>> = Vec::new();
    let mut paths: Vec<String> = Vec::with_capacity(t.entries.len());
    for e in &t.entries {
        let name = os_from(name_bytes(&e.name).0);
        let name = name.to_string_lossy();
        let path = e.parent.map_or_else(|| name.to_string(), |p| format!("{}/{name}", paths[p]));
        if let EntryNode::File(c) = e.node {
            if first.len() <= c {
                first.resize(c + 1, None);
            }
            first[c].get_or_insert_with(|| path.clone());
        }
        paths.push(path);
    }
    first.into_iter().map(Option::unwrap_or_default).collect()
}

/// By leaf of `archive` (decoded into `out`), the files of it as they are shown where it takes
/// long: the first by its path in the archive, and how many more.
fn leaf_names(archive: &aqr::Archive, out: &Path) -> Vec<String> {
    let Some(t) = archive.tree() else { return vec![out.display().to_string(); archive.leaves_len()] };
    let mut files: Vec<(Option<String>, usize)> = vec![(None, 0); archive.leaves_len()];
    // (as the archive has them: names between `/`, [`tree_content_paths`])
    let mut paths: Vec<String> = Vec::with_capacity(t.entries.len());
    for e in &t.entries {
        let name = os_from(name_bytes(&e.name).0);
        let name = name.to_string_lossy();
        let path = e.parent.map_or_else(|| name.to_string(), |p| format!("{}/{name}", paths[p]));
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
            (Some(p), 1) => p,
            (Some(p), n) => format!("{p} and {} more files", n - 1),
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
            true => sandbox::Sandbox::current_exe([SANDBOX_WORKER])
                .map(|s| s.title("aqr: sandbox worker"))
                .map_err(|e| e.to_string()),
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
        told.into_iter().for_each(|t| eprintln!("{}: {t}", stdio::shown(input)));
    }
}

fn summary_d(input: &Path, out: Option<&Path>, files: usize, total: u64, start: Instant) {
    let t = start.elapsed().as_secs_f64();
    let to = match out {
        None => " (tested)".into(),
        Some(o) if o.as_os_str().is_empty() => " -> .".into(),
        Some(o) => format!(" -> {}", stdio::shown(o)),
    };
    eprintln!(
        "{}{to}: {files} file{}, {} in {}, {:.1} MB/s",
        stdio::shown(input),
        if files == 1 { "" } else { "s" },
        size(total as f64),
        aqr_l10n::duration(t, 2),
        total as f64 / 1e6 / t.max(1e-9)
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The options `--help` tells of, as given (`-x`, `--long`).
    pub(crate) fn help_options() -> Vec<String> {
        let mut out = Vec::new();
        for line in HELP.lines() {
            // (the lines of options: indented by 2 to 6, the option first, its words to two spaces)
            let indent = line.len() - line.trim_start().len();
            if !(2..=6).contains(&indent) || !line.trim_start().starts_with('-') {
                continue;
            }
            let column = line.trim_start().split("  ").next().unwrap();
            for word in column.split([' ', ',']) {
                let name = word.split('[').next().unwrap();
                if name.len() < 2 || !name.starts_with('-') || name == "..." {
                    continue;
                }
                out.push(name.to_owned());
            }
        }
        out
    }

    /// Every option `--help` tells of is in the manual page (`aqr.1`), as mdoc's `Fl`.
    #[test]
    fn man_page_has_every_option() {
        let man = include_str!("../aqr.1");
        let missing: Vec<String> =
            help_options().into_iter().filter(|o| !man.contains(&format!("Fl {}", &o[1..]))).collect();
        assert!(missing.is_empty(), "not in aqr.1: {missing:?}");
    }

    /// `-M`: by default 2 GB but at most half the system's; `avail` most of what is available,
    /// but not too little; a share of the system's; a size; none.
    #[test]
    fn memory() {
        const G: u64 = 1 << 30;
        let (sys, unknown) = ((Some(16 * G), Some(10 * G)), (None, None));
        assert_eq!(budget(None, sys), Ok(aqr_common::budget::DEFAULT));
        assert_eq!(budget(None, (Some(2 * G), Some(G))), Ok(G));
        assert_eq!(budget(None, unknown), Ok(aqr_common::budget::DEFAULT));
        assert_eq!(budget(Some("avail"), sys), Ok(8 * G));
        assert_eq!(budget(Some("avail"), (Some(16 * G), Some(G / 2))), Ok(G));
        assert_eq!(budget(Some("avail"), (Some(2 * G), Some(0))), Ok(G / 2));
        assert_eq!(budget(Some("avail"), unknown), Ok(aqr_common::budget::DEFAULT));
        assert_eq!(budget(Some("25%"), sys), Ok(4 * G));
        assert!(budget(Some("25%"), unknown).is_err());
        assert!(budget(Some("0%"), sys).is_err() && budget(Some("101%"), sys).is_err());
        assert_eq!(budget(Some("3G"), sys), Ok(3 * G));
        assert_eq!(budget(Some("0"), sys), Ok(u64::MAX));
        assert!(budget(Some("lots"), sys).is_err());
    }

    /// An archive is compressed with a spill beside it, but an encrypted one with none: nothing
    /// of its contents written out as it is.
    #[test]
    fn no_spill_sealed() {
        let out = std::env::temp_dir().join("x.aqr");
        assert!(spill_of(&out, false).unwrap().is_some_and(|p| p.ends_with(".x.aqr")));
        assert_eq!(spill_of(&out, true), Ok(None));
    }
}
