//! Compresses files and directories into Aqueous archives, and decompresses them, as zstd does.
//!
//! usage: aqr [-c | -d | -K | -t] [-o OUTPUT] [-L] [-f] [--rm] [-p] [-q | -v] [-T THREADS] [-l LEVEL | -0 ... -9] [--depth N]
//! [-B SIZE] [-b SIZE] [-S SIZE] [-D] [-A] [--no-check] [--lossy SPEC] [-P] [--stats[=FILE]] [-Z NAME[=VALUE]]... [-h]
//! INPUT [--rename TO]...
//!
//! aqr -d \[OPTIONS\] ARCHIVE [PATH [--rename TO]]...; aqr -d --each \[OPTIONS\] ARCHIVE...; aqr -t [-q | -v] ARCHIVE \[PATH\]...;
//! aqr -t --each [-q | -v] ARCHIVE...; aqr --delete [--compact] [-q] ARCHIVE PATH...;
//! aqr --move [-q | -v] ARCHIVE PATH --rename TO...; aqr --compact [-q | -v] ARCHIVE;
//! aqr --convert [-P | --no-passphrase] [-A | --no-archival] [-o OUTPUT] ARCHIVE...
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
//! - `--attributes FILE`: compressing, hints by path (docs/hints-design.md) in `FILE`, before
//!   those of the `.aqr/attributes` of each input that is a directory and of the
//!   `.gitattributes` under it ([`attributes`]): its patterns from the root of each input, the
//!   files its values name from its own directory.
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
//! - `--format sfx` (Windows x86-64): a self-extracting program, `INPUT.exe` without `-o` ([`sfx`]):
//!   the Aqueous archive (a single file in it by its name) after a stub that writes its files out
//!   when run, with the native blobs of the modules the archive names by their hashes merged into
//!   it (and the interpreter blob, for any other module); what the program does once run as
//!   the `.aqr/sfx/sfx.toml` of the input's root tells (docs/sfx-config-design.md). Not with `-a`,
//!   `-A`, `-P`, `-j` or standard input or output.
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
//!   Only what they need is decoded ([`aqr_archive::Archive::contents_of`], [`aqr_unpack::Select`]). A
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
//!   a mirror block of each block whose damage would lose more than a file ([`aqr_archive::Mirroring`]),
//!   the last metadata block's before the last 4 MiB of the archive (held back as it is made),
//!   so that a truncation shorter than that leaves it. Decompressing, an archive of a tree is
//!   read by its index ([`aqr_archive::open_indexed`]): of the files taken alone (`-d ARCHIVE PATH...`),
//!   only their blocks and what tells where they are, the metadata blocks back from the last as
//!   far as they tell the paths ([`aqr_archive::open_indexed_at`]), an encrypted archive's chunks
//!   decrypted as they are read ([`aqr_archive::envelope::Decrypting`]). Where it does not open so, and for the files
//!   that do not decode so, and testing, it is read whole ([`Repair`]) and repaired in memory
//!   ([`aqr_archive::mirror::repair`]): a damaged block restored from its mirror
//!   block, the footer made again where the archive is cut short after its last metadata block;
//!   told so. Cut short before it (its metadata block restored past the bytes lost), it is
//!   salvaged ([`aqr_archive::open_salvaging`]): the files of the blocks lost are lost. Cut short in a
//!   segment whose metadata block is not there, that segment is cut off, the run failing; its
//!   leaf blocks still whole are evaluated each alone, their files of no name written into
//!   `#salvaged` (`leaf-N`) beside the tree extracted whole ([`salvage_unnamed`]). An archive
//!   whose footer is made again after a segment not meant to be the last fails the run too.
//! - `--lossy SPEC`: compressing, the files that a rewriter of SPEC takes archived as other
//!   files that their readers take as the same ([`aqr_compress::lossy`], docs/lossy-design.md):
//!   what only their encoders chose lost, each entry marked so (an extension key, with the size
//!   it was; `-t -v` and `--json` tell it), `-a` and `-s` comparing a file with that size. A
//!   single file is archived as a tree of it alone, which has the mark. Not with `-A`, nor into
//!   archives of other formats.
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
//! - `-Z interpret ARCHIVE`: a verb, though of `-Z` (not stable): the archive value it is
//!   interpreted to, in EDN, as the conformance suite writes it ([`interpret`]).
//! - `--delete ARCHIVE PATH...`: the paths of the archive given (a directory with what is under
//!   it) taken out of it in place, by a segment of tombstones appended and committed as `-a`
//!   commits ([`edit`]); their bytes stay in the file. A path not in it is an error, before
//!   anything is written.
//! - `--compact ARCHIVE`: the archive written again of what it holds, segment by segment, the
//!   blocks it still needs copied as they are, those solid with what is gone made again of what
//!   is left ([`aqr_archive::rewrite`]): what appends replaced or took away, and an append not
//!   committed, out of the file. Written beside it and checked, then
//!   put in its place by a rename. With `--delete`, of the tree without its paths (no
//!   tombstones). Not of an archive of mirror blocks (`-A`) for now. Of an archive with journal
//!   points, `--keep-since SPEC` keeps those from the one it picks on (the segments before it
//!   written as one state, the others as they are), `--drop-history` none ([`aqr_archive::History`]).
//! - `--convert ARCHIVE...`: each Aqueous archive written again ([`convert`],
//!   `docs/convert-design.md`), nothing compressed again: encrypted (`-P`, a new passphrase
//!   asked for after the input's) or decrypted (`--no-passphrase`), mirrored (`-A`) or not
//!   (`--no-archival`), as the input is where neither is given; its segments and journal points as
//!   they are (but as `--keep-since` and `--drop-history` tell, as of `--compact`); repaired
//!   where damaged, its mirror blocks restoring what they can (what would lose a file is not
//!   written). In place (beside it and checked first), or to `-o`; nothing to change, nothing
//!   written. `--passphrase-from` gives as many sources as passphrases are needed, the input's
//!   then `-P`'s apart by `;`. An archive of another format (as `-d` takes them by name) is
//!   written as an Aqueous archive of the same tree as aqr-unpack reads it, its files
//!   compressed as `-c` compresses them (its options taken), into its name without the format's
//!   suffix and `.aqr` (or `-o`); an entry left out fails it.
//! - `--move ARCHIVE PATH --rename TO...`: each path moved to its `TO` (in the order given, the
//!   directories on the way made, without their metadata), the archive written again as
//!   `--compact` writes it, as a segment cannot take the contents of another but by an import
//!   (not read yet). `TO` there already, under the path or under what is not a directory, is
//!   an error, before anything is written.
//! - `-P`: compressing, the archives encrypted with a passphrase ([`aqr_archive::envelope`]), asked for
//!   as the work begins (twice on a terminal); decompressing, asked for then rather than as an
//!   archive needs it. An encrypted archive (of either kind) is opened by the passphrases that
//!   opened those before it, else asks for one where there is a terminal or a frontend
//!   ([`pass`]). Without a terminal, `-P` takes a line of standard input. `--passphrase-from`
//!   (implying `-P`) takes it from a line of standard input (`stdin`), of a file descriptor
//!   (`fd:N`) or of a file (any other path), or an environment variable's value (`env:NAME`),
//!   nothing asked for (decompressing, the archives it does not open fail). An empty passphrase
//!   is a passphrase too. One source alone: two, apart by `;`, are `--convert`'s (a path of a
//!   `;` is given as `fd:N`).
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
//! - `-Z`: developer options ([`aqr_common::zopt`]), declared by the crates where they are read:
//!   bounds, thresholds and switches of the stages, set without a rebuild (`-Z help` lists
//!   them, `-Z NS:help` those of a namespace). This crate's are [`WASM`] (`-Z wasm`) and
//!   [`SANDBOX`] (`-Z no-sandbox`). Those of `wasm:` are passed to the sandbox's workers
//!   (their last arguments), the others not.
//! - `-l`: the level of compression ([`aqr_archive::Level`]), `0` (nothing compressed: uncompressed
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
//! - What marks a file as come from elsewhere (Windows' `Zone.Identifier`, macOS' quarantine:
//!   [`meta::Mark`]) by `--mark WHEN` ([`meta::Marks`]): extracting, the archive's given to every
//!   file over those stored (`propagate`, the default), the archive's or one of the Internet's
//!   zone (`always`), those stored alone (`keep`) or none (`never`); compressing, the files' stored
//!   (`keep`, the default) or not (`never`).
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
//! - `--run-before CMD` and `--run-after CMD` run commands by the system's shell before and after
//!   each archive is written (`-c`, `-a` and the edits; signing a self-extracting program, say),
//!   the archive's path in `AQR_OUTPUT` ([`hooks`]).
//! - An archive of several files is committed as it is written ([`stop`]; spec, "Appending to an
//!   archive"): written as `OUTPUT.PID.part`, it is renamed once a segment of it is committed
//!   (`-S`), and later commits are made in place, so that the file holds an archive of what was
//!   committed wherever the process is stopped (even killed), telling what it left out
//!   ([`aqr_archive::Status`]). Ctrl-C (`SIGTERM`, `SIGHUP`, a frontend's `finish`) ends it as it is, of
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
//! options leave open. `--session` is that started before the work is known, as a compression
//! dialog does: asked how many files the inputs are while the options are chosen, then begun
//! ([`session`]). `--serve` has it serve another the entries of archives and the bytes of their
//! files, as a shell extension browsing them asks ([`serve`]).

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::OnceLock;
use std::time::Instant;

/// `eprintln!` and `eprint!` of text a terminal shows as it is ([`term::safe`]): what is told
/// names files and archives' entries, whose names may hold escape sequences.
macro_rules! eprintln {
    () => { ::std::eprintln!() };
    ($($t:tt)*) => { ::std::eprintln!("{}", $crate::term::safe(&::std::format!($($t)*), true)) };
}
macro_rules! eprint {
    ($($t:tt)*) => { ::std::eprint!("{}", $crate::term::safe(&::std::format!($($t)*), true)) };
}

mod append;
mod args;
mod attributes;
mod complete;
mod compress;
mod convert;
mod decompress;
mod edit;
mod exit;
mod extract;
mod files;
mod fold;
mod foreign;
mod gather;
mod hooks;
mod info;
mod interpret;
mod ipc;
mod journal;
mod list;
mod meta;
mod names;
mod pass;
#[cfg(all(windows, target_vendor = "win7"))]
mod random;
mod serve;
mod session;
mod sfx;
mod sparse;
mod stats;
mod stdio;
mod stop;
mod term;
mod view;

use args::*;
use compress::*;
use decompress::*;
use files::*;
use foreign::*;
use gather::*;

use aqr_archive::{Entry, EntryNode, Failure, Tree, name_bytes, name_from};
pub(crate) use aqr_common::text::hex;
use aqr_compress::mapped::Mapper;
use aqr_compress::{Options, compress_formula_to, detect, kind_by_name};
use aqr_decompress::{Native, Wasm, sandbox};
use aqr_formula::Bytes;
use aqr_l10n::size;
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
       aqr --convert [-P | --no-passphrase] [-A | --no-archival] [-o OUTPUT] ARCHIVE...
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
                        all its metadata; --json: as a line of JSON each archive;
                        --history and --diff take --json too)
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
      --convert         write an Aqueous archive again (in place, or to -o), nothing
                        compressed again: encrypted or decrypted, mirrored or not, as
                        -P, --no-passphrase, -A and --no-archival tell (else as it is),
                        repaired where damaged; --keep-since and --drop-history as of
                        --compact; --passphrase-from SOURCE;SOURCE for the input's and
                        -P's. Of another format (.zip, .7z, .tar.gz, ... as -d takes
                        them): an Aqueous archive of its tree, compressed as -c does
      --no-passphrase   with --convert: the archive written decrypted (else no -P)
      --no-archival     with --convert: the archive written without mirror blocks (else
                        no -A)

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
      --attributes FILE hints by path in FILE (as an input's .aqr/attributes, before it;
                        its patterns from the root of each input)
  -L, --dereference     follow links (given or found), archiving what they lead to;
                        those leading nowhere or to a directory they are in kept
      --format FORMAT   aqr (the default), zip, 7z, alz, egg, tar, tar.gz, tar.bz2,
                        tar.xz, tar.lzma, tar.zst, or gz, bz2, xz, lzma, zst of a
                        single file (also by -o's name); -l is the format's level
                        (zlib's 0-9; xz, lzma, 7z: 0-9; bz2: 1-9; zst: 1-22); sfx, a
                        self-extracting program (Windows; its settings in the
                        input's .aqr/sfx/sfx.toml)

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
      --run-before CMD  run CMD by the system's shell before each archive is written,
      --run-after CMD   or once it is written (each as often as given, in order; the
                        archive's path in AQR_OUTPUT; a command failing fails aqr)
  -p, --preserve        keep and apply the metadata not commonly kept too
      --mark WHEN       the marks of files from elsewhere (Zone.Identifier, quarantine):
                        extracting, propagate (the default: the archive's over the files'),
                        always (the archive's, else the Internet's), keep (the files') or
                        never; compressing, keep (the default) or never
  -q, --quiet           log nothing but errors
  -v, --verbose         log each file archived
      --stats[=FILE]    tell what stands out after each archive; FILE.json or FILE.html
                        writes all that was sampled (may be given more than once)
      --ipc             be driven by a frontend through standard input and output
      --session         as --ipc, told what to do later (a compression dialog's)
      --serve           serve a frontend the entries and files of archives through
                        standard input and output (a shell extension's)

Compression:
  -P, --passphrase      encrypt the archives with a passphrase asked for (decompressing:
                        ask for it at once; an encrypted archive asks anyway where there
                        is a terminal); without a terminal, a line of standard input
      --passphrase-from SOURCE
                        -P, its passphrase a line of standard input (stdin), of a file
                        descriptor (fd:N) or of a file (a path; ./env:x for env:x), or
                        an environment variable's value (env:NAME); nothing asked
                        (no ; in it: --convert's two sources are apart by one)
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
      --lossy SPEC      archive files as others read the same: what their encoders chose
                        lost (coding: streams made again; slack: unused bytes zeroed,
                        deleted data lost); SPEC is classes by commas, each CLASS or
                        CLASS:FORMAT+..., or all
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
      --tag LABEL       name the state of an archive as a journal point (as -j does,
                        with nothing else changed)
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
  -Z NAME[=VALUE]       a developer option, not stable across versions (-Z help lists
                        them, -Z NS:help those of NS:; a switch also as -Z no-NAME):
                        -Z wasm runs every module as WebAssembly, -Z no-sandbox the
                        Wasm modules in this process, not in workers locked down;
                        -Z interpret ARCHIVE, a command, prints the archive value an
                        archive is interpreted to (the spec's Archive model) in EDN
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

aqr_common::zopt! {
    /// In decoding, every module run as WebAssembly, the known ones too (natively otherwise).
    pub(crate) static WASM: Opt<bool> =
        Opt::flag("wasm", false, "decoding, run every module as WebAssembly, the known ones too");
    /// In decoding, the modules not known (every one with [`WASM`]) run in worker processes locked
    /// down by the system ([`aqr_decompress::sandbox`]), started once the first of them is loaded,
    /// a sandbox for each archive; else in this process. Where the system cannot lock workers
    /// down, they do not run but without it (the known ones with [`WASM`] in this process).
    pub(crate) static SANDBOX: Opt<bool> = Opt::flag(
        "sandbox",
        true,
        "decoding, run the Wasm modules not known (all with wasm) in workers locked down by the system",
    );
}

/// The argument (the only one) that this program is run with as a worker of the sandbox.
const SANDBOX_WORKER: &str = "--sandbox-worker";

#[global_allocator]
static ALLOC: aqr_common::mem::Mapped = aqr_common::mem::Mapped;

fn main() -> ExitCode {
    // (the modules run as WebAssembly by the interpreter blob this program carries, of the engine
    // `-Z wasm:engine` picks, where none is linked: Windows x86-64; in a worker too)
    aqr_decompress::set_interpreter(sfx::interpreter);
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
        && let Err(e) = ipc::start(args.session)
    {
        eprintln!("aqr: {e}");
        return ExitCode::FAILURE;
    }
    // (a session: its requests served until it begins, and then as if run with what it began with)
    let args = match args.session {
        true => match session::serve(args) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("aqr: {e}");
                ipc::tell(serde_json::json!({"event": "error", "message": e}));
                return ExitCode::FAILURE;
            }
        },
        false => args,
    };
    // (of --convert, asked for as each archive needs them: [`convert`])
    let (given, from) = match args.convert {
        true => (false, None),
        false => (args.passphrase, args.pass_from.first()),
    };
    let compressing = !(args.decompress || args.list || args.info || args.history || args.diff || args.tag.is_some());
    if let Err(e) = pass::init(given, compressing, from) {
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
    // (served until the frontend is gone: not on a worker, which the requests' work takes)
    if args.serve {
        return match serve::serve(&args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("aqr: {e}");
                ExitCode::FAILURE
            }
        };
    }
    let r = aqr_common::pool::run(|| match () {
        _ if args.convert => convert::convert_all(&args),
        _ if args.edit.is_some() || args.compact => edit::edit(&args),
        _ if args.interpret => interpret::interpret(&args),
        _ if args.info => info::info(&args),
        _ if args.tag.is_some() => journal::tag_all(&args),
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
/// (and the applications kept over it), by module, those given up for their heap, by format, and
/// those that panicked, by format and message.
fn tell_fallbacks(fallbacks: &[aqr_compress::Fallback]) {
    type Tally<'a> = std::collections::BTreeMap<(&'a str, bool, bool, Option<&'a str>), (u64, u64, u64)>;
    let mut by_module = Tally::default();
    for f in fallbacks {
        let e = by_module.entry((f.module, f.kept, f.heap, f.panicked.then_some(&f.message[..]))).or_default();
        (e.0, e.1, e.2) = (e.0 + 1, e.1 + f.bytes, e.2.max(f.needed));
    }
    for ((module, kept, heap, message), (n, bytes, most)) in by_module {
        if let Some(message) = message {
            eprintln!(
                "panic: {module}: {n} recompressions panicked, {} of data kept as they are ({message})",
                size(bytes as f64)
            );
            continue;
        }
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

/// The ending of a noun counted `n`: `s` unless it is one.
pub(crate) fn plural<T: PartialEq + From<u8>>(n: T) -> &'static str {
    if n == T::from(1) { "" } else { "s" }
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
            ipc::between(serde_json::json!({"event": "skipped", "output": path.display().to_string()}));
            Ok(None)
        }
        None => Err(format!("{} exists (-f to overwrite)", path.display())),
    }
}

/// Tells the frontend that an archive is being made or decoded.
fn begin(mode: &str, inputs: &[PathBuf], out: Option<&Path>) {
    let inputs: Vec<String> = inputs.iter().map(|i| i.display().to_string()).collect();
    let output = out.map(|o| o.display().to_string());
    ipc::begin(serde_json::json!({"event": "begin", "mode": mode, "inputs": inputs, "output": output}));
}

/// Tells the frontend that an archive is done.
fn end(out: Option<&Path>, files: usize, read: u64, written: u64, start: Instant) {
    ipc::between(serde_json::json!({
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

#[cfg(test)]
mod tests;
