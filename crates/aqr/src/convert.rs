//! Archives written again as Aqueous archives (`--convert`; `docs/convert-design.md`).
//!
//! An Aqueous archive (standalone: from the start of its file, encrypted or not) is written
//! again with its encryption and mirror blocks put on, changed or taken off, nothing compressed
//! again: `-P` seals it with a new passphrase (asked for after the input's, [`pass::new`]),
//! `--no-passphrase` takes its encryption off, `-A` gives it mirror blocks, `--no-archival`
//! takes them away; what is not given is as the input has it (an encrypted input sealed again
//! with its own passphrase, under a new file key and nonce). The archive itself is kept as it
//! is ([`aqr_archive::History::Whole`]: its segments, entries replaced, tombstones, journal
//! points), but where `--keep-since` or `--drop-history` tell, as `--compact` has them.
//!
//! The input is opened as `-d` opens it, locked against another writer: an encrypted one
//! decrypted as it is copied, a chunk at a time ([`aqr_archive::rewrite_payload`]), where what
//! tells its blocks authenticates and its mirror blocks are as their blocks; else decrypted whole
//! into memory (at most [`IN_MEMORY`] bytes), and repaired where it is damaged ([`aqr_archive::mirror::repair`]:
//! blocks restored from their mirror blocks, a footer made again), not salvaged: a repair that
//! would lose a file, which every file decoding tells, fails. A repair is a change too: an
//! archive damaged is written again repaired with nothing else given. With nothing to change,
//! nothing is written (even with `-o`).
//!
//! An archive of another format (one `-d` takes by its name: ZIP, 7z, RAR, ALZ, EGG, tar and its
//! compressors, single-file streams) is written as an Aqueous archive of the same tree, as
//! aqr-unpack reads it ([`convert_foreign`]; not through the disk, so that what a file system
//! does not hold is kept): every file's content compressed again as `-c` compresses one (policy
//! A of the design: no body carried as it is), each option of `-c` taken; a single file's stream
//! as an archive of the file alone. It is unpacked whole first, the contents held as they come
//! (in the spill beside the output where large; in memory alone where the input or the output is
//! encrypted), then compressed. An entry left out (encrypted, of a method not known, out of the
//! tree) fails it: nothing is written. The output is named as the input without the suffix that
//! tells its format, `.aqr` after (`foo.tar.gz` -> `foo.aqr`), beside it, or `-o`; there already,
//! as of `-c` (`-f`).
//!
//! The output is written beside where it goes (`NAME.PID.part`), read back (decrypted with the
//! passphrase it was sealed with) and checked against the input's tree and journal, then put in
//! its place by a rename: in place of the input without `-o`, so that the file is the archive
//! as it was or as converted, whatever happens meanwhile.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::Instant;

use aqr_archive::envelope;
use aqr_archive::{History, Layers, Tree};

use crate::{Args, exit, pass, plural, stop};

/// The most bytes of an archive converted held in memory: a damaged encrypted archive's payload
/// decrypted whole, or an archive repaired.
const IN_MEMORY: u64 = 16 << 30;

/// Converts each input ([module](self)).
pub fn convert_all(a: &Args) -> Result<(), String> {
    for input in &a.inputs {
        convert(a, input)?;
    }
    Ok(())
}

fn convert(a: &Args, input: &Path) -> Result<(), String> {
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", input.display());
    let file = fs::File::open(input).map_err(|e| err(&exit::seen(e)))?;
    // SAFETY: the archive is locked against other writers (below); where it changes all the same,
    // its checksums tell
    let map = unsafe { memmap2::Mmap::map(&file) }.map_err(|e| err(&exit::seen(e)))?;
    let name = input.file_name().map(crate::os_bytes).unwrap_or_default();
    let aqueous = envelope::is_encrypted(&map)
        || envelope::is_damaged_encrypted(&map)
        || map.starts_with(&aqr_archive::HEADER_MAGIC)
        || aqr_archive::locate(&map).is_some()
        || name.ends_with(b".aqr");
    match aqr_unpack::by_name(&name) {
        _ if aqueous => {}
        Some((f, n)) => {
            drop(file);
            return convert_foreign(a, input, &map, f, n);
        }
        None => return Err(err(&"not an Aqueous archive, nor named as an archive known")),
    }
    match file.try_lock() {
        Ok(()) => {}
        Err(fs::TryLockError::WouldBlock) => return Err(err(&"being written by another")),
        Err(fs::TryLockError::Error(e)) => return Err(err(&e)),
    }
    if let Some(o) = a.recompressing.first() {
        return Err(err(&format!("{o}: an Aqueous archive converted is not compressed again")));
    }
    convert_aqr(a, input, &file, &map)
}

/// What an archive converted is sealed with.
enum Sealing {
    None,
    /// The input's passphrase, under a new file key and nonce.
    Same(Vec<u8>),
    New(Vec<u8>),
}

/// Converts `input`, an Aqueous archive (its file, and mapped: `map`).
fn convert_aqr(a: &Args, input: &Path, file: &fs::File, map: &[u8]) -> Result<(), String> {
    let start = Instant::now();
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", input.display());
    let tell = |what: &str| {
        if !a.quiet {
            eprintln!("{}: {what}", input.display());
        }
    };
    let threads = a.opt.threads;
    let sealed = envelope::is_encrypted(map) || envelope::is_damaged_encrypted(map);
    // (the passphrases needed, and as many sources given where any is)
    let needed = sealed as usize + a.passphrase as usize;
    if !a.pass_from.is_empty() && a.pass_from.len() != needed {
        let n = a.pass_from.len();
        return Err(err(&format!(
            "--passphrase-from: {n} source{} given, {needed} passphrase{} needed (the input's{}, then -P's)",
            plural(n),
            plural(needed),
            if sealed { "" } else { ": none, as it is not encrypted" }
        )));
    }
    crate::begin("convert", &[input.to_owned()], a.output.as_deref().or(Some(input)));
    // the archive's bytes: the payload decrypted as it is read where it is sound, else whole (its
    // chunks that do not authenticate erased), to be repaired
    let mut streaming: Option<envelope::Decrypting> = None;
    let mut decrypted = None;
    let mut erased = Vec::new();
    let mut header_from = None;
    let mut old_pass = None;
    let plain: &[u8] = match sealed {
        true => {
            let (e, opened) = envelope::open_damaged(map).map_err(|e| err(&exit::seen(e)))?;
            header_from = opened.header_from;
            if let Some(k) = header_from {
                tell(&format!("its encrypted header damaged, taken from mirrored header {k}"));
            }
            let u = pass::before(input, a.pass_from.first(), |p| match e.unlock(p) {
                Ok(u) => Ok(Some((u, p.to_vec()))),
                Err(aqr_archive::Failure::Decryption(_)) => Ok(None),
                Err(f) => Err(err(&f)),
            })?;
            let Some((u, p)) = u else {
                exit::note(exit::Code::Decryption);
                return Err(err(&"encrypted, and no passphrase given opens it"));
            };
            old_pass = Some(p);
            let mut d = u.decrypting();
            // SAFETY: of the file mapped read only
            unsafe { d.let_go_read() };
            // (sound: what tells its blocks authenticates, read to its footer, every mirror block
            // as its block; a data block that does not authenticate fails it as it is copied, as it
            // would not be repaired either)
            let sound = header_from.is_none() && !opened.cut && aqr_archive::decrypt_framing(&d).is_ok() && {
                // SAFETY: what is read of it got by `decrypt_framing`
                let p = unsafe { d.unread() };
                let c = aqr_archive::mirror::check_sealed(p, &[], false);
                c.walked && c.differing(p).next().is_none()
            };
            if sound {
                streaming = Some(d);
                &[]
            } else {
                if map.len() as u64 > IN_MEMORY {
                    return Err(err(&format!(
                        "a damaged encrypted archive over {} is not repaired yet",
                        bytes(IN_MEMORY)
                    )));
                }
                let u = d.unlocked();
                let mut m = memmap2::MmapMut::map_anon(u.len().max(1)).map_err(|e| err(&exit::seen(e)))?;
                let (failed, cut) = u.decrypt_lossy(&mut m[..u.len()], threads);
                if cut {
                    exit::note(exit::Code::Invalid);
                    return Err(err(&"cut short after an encrypted chunk: what came after it is lost (not converted)"));
                }
                if !failed.is_empty() {
                    tell(&format!("{} encrypted chunk{} not authenticating", failed.len(), plural(failed.len())));
                }
                erased = failed.iter().map(|&m| u.chunk_range(m)).collect();
                decrypted = Some((m, u.len()));
                &[]
            }
        }
        false => match aqr_archive::locate(map) {
            Some(r) if r.start > 0 => {
                return Err(err(&format!(
                    "an archive after {} of other data (a self-extracting program's, say) is not converted",
                    bytes(r.start as u64)
                )));
            }
            Some(r) => {
                if r.end < map.len() {
                    let n = bytes((map.len() - r.end) as u64);
                    tell(&format!("{n} after the archive left out (an append not committed)"));
                }
                &map[r]
            }
            // (damaged: repaired below)
            None => map,
        },
    };
    let plain = match (&decrypted, &streaming) {
        (Some((d, n)), _) => &d[..*n],
        // SAFETY: what is read of it but its data blocks got by `decrypt_framing`, those read only
        // by `rewrite_payload`, which gets them
        (None, Some(d)) => unsafe { d.unread() },
        (None, None) => plain,
    };
    // repaired where it is damaged, not salvaged
    let check = match sealed {
        true => aqr_archive::mirror::check_sealed(plain, &erased, false),
        false => aqr_archive::mirror::check(plain),
    };
    if !check.walked && plain.len() as u64 > IN_MEMORY {
        return Err(err(&format!("a damaged archive over {} is not repaired yet", bytes(IN_MEMORY))));
    }
    let restored = aqr_archive::mirror::repair(plain, &check, false, &erased)
        .map_err(|e| err(&format!("repairing: {}", exit::seen(e))))?;
    let repaired = restored.is_some() || header_from.is_some();
    if let Some(r) = &restored {
        if !r.lost.is_empty() {
            exit::note(exit::Code::Invalid);
            let n: usize = r.lost.iter().map(|l| l.len()).sum();
            return Err(err(&format!(
                "damaged, {} lost (aqr -d salvages what is left): not converted",
                bytes(n as u64)
            )));
        }
        if r.header {
            tell("its header magic restored");
        }
        if r.restored > 0 {
            tell(&format!("{} damaged block{} restored from mirror blocks", r.restored, plural(r.restored)));
        }
        if r.footer {
            tell("its footer made again");
        }
    }
    let read: &[u8] = restored.as_ref().map_or(plain, |r| &r.bytes);
    let opened = match sealed {
        true => aqr_archive::open_payload(read, &aqr_archive::NoModules),
        false => aqr_archive::open(read, &aqr_archive::NoModules),
    };
    let archive = opened.map_err(|e| err(&exit::seen(e)))?;
    let tree = archive.tree().cloned().ok_or_else(|| err(&"an archive of a single file is not converted yet"))?;
    let sizes: Vec<u64> = (0..archive.contents_len()).map(|c| archive.content_size(c).unwrap_or(u64::MAX)).collect();
    let journal: Vec<(u64, String)> = archive.journal().iter().map(|p| (p.time, p.label.clone())).collect();
    let history = match (&a.keep_since, a.drop_history) {
        (Some(at), _) => {
            let p = crate::journal::pick(archive.journal(), at).map_err(|e| err(&format!("--keep-since: {e}")))?;
            History::Keep { from: p.metadata, since: p.time }
        }
        (None, true) => History::Drop,
        (None, false) => History::Whole,
    };
    drop(archive);
    // (a repair that would lose a file is not taken: every file decoded, as -K does)
    if repaired || !erased.is_empty() {
        let decoder = crate::Decoder {
            native: aqr_decompress::Native { threads },
            wasm: crate::WASM.get(),
            sandbox: crate::SANDBOX.get().then(std::sync::OnceLock::new),
        };
        let opened = match sealed {
            true => aqr_archive::open_payload(read, &decoder),
            false => aqr_archive::open(read, &decoder),
        };
        let r = opened.and_then(|x| {
            let all = vec![true; x.leaves_len()];
            x.each_leaf_only(threads, &all, |_, r| r.map(drop))
        });
        if let Err(e) = r {
            exit::note(exit::Code::Invalid);
            return Err(err(&format!("damaged past what its mirror blocks restore ({e}): not converted")));
        }
    }

    // what changes
    let mirrored = check.mirrored(plain, sealed);
    let mirrors = match () {
        _ if a.opt.mirrors => true,
        _ if a.no_archival => false,
        _ => !check.mirrors.is_empty(),
    };
    let mut changes = Vec::new();
    if repaired || !erased.is_empty() {
        changes.push("repaired");
    }
    match () {
        _ if a.passphrase && sealed => changes.push("its passphrase changed"),
        _ if a.passphrase => changes.push("encrypted"),
        _ if a.no_passphrase && sealed => changes.push("decrypted"),
        _ => {}
    }
    match () {
        _ if a.opt.mirrors && !mirrored => changes.push("mirrored"),
        _ if a.no_archival && !check.mirrors.is_empty() => changes.push("its mirror blocks taken away"),
        _ => {}
    }
    match history {
        History::Keep { .. } => changes.push("journal points kept from --keep-since's"),
        History::Drop if !journal.is_empty() => changes.push("journal points dropped"),
        _ => {}
    }
    if changes.is_empty() {
        tell("nothing to change (as -P, --no-passphrase, -A and --no-archival tell)");
        crate::end(None, 0, map.len() as u64, 0, start);
        return Ok(());
    }
    let sealing = match (a.passphrase, a.no_passphrase, old_pass) {
        (true, _, _) => Sealing::New(pass::new(input, a.pass_from.get(sealed as usize))?),
        (false, true, _) | (false, false, None) => Sealing::None,
        (false, false, Some(p)) => Sealing::Same(p),
    };

    // written beside where it goes, checked, put in its place
    let target = match &a.output {
        Some(o) => match crate::output(a, o)? {
            Some(_) => o.clone(),
            None => return Ok(()),
        },
        // (a link to the archive: the archive written again where it is)
        None => fs::canonicalize(input).map_err(|e| err(&exit::seen(e)))?,
    };
    let part = crate::part_of(&target);
    let perr = |e: &dyn std::fmt::Display| format!("{}: {e}", part.display());
    let (mut out, _guard) =
        stop::create(&part, || fs::OpenOptions::new().read(true).write(true).create_new(true).open(&part))
            .map_err(|e| perr(&e))?;
    let layers = Layers { mirrors, size_hint: !matches!(sealing, Sealing::None) };
    let r = (|| -> Result<(aqr_archive::Rewritten, Option<envelope::Opener>), String> {
        let payload = match &streaming {
            Some(d) if restored.is_none() => aqr_archive::Payload::Sealed(d),
            _ => aqr_archive::Payload::Plain(read),
        };
        let rewrite = |w: &mut dyn Write| {
            aqr_archive::rewrite_payload(payload, &tree, a.opt.level, threads, w, history, layers).map_err(|e| err(&e))
        };
        let (r, opener) = match &sealing {
            Sealing::Same(p) | Sealing::New(p) => {
                // (mirrored headers, as -c -P writes them)
                let mut s = envelope::Sealer::new(&mut out, p, envelope::WORK_FACTOR, true).map_err(|e| perr(&e))?;
                let r = rewrite(&mut s)?;
                s.finish().map_err(|e| perr(&e))?;
                (r, Some(s.opener()))
            }
            Sealing::None => {
                let mut w = std::io::BufWriter::with_capacity(8 << 20, &out);
                let r = rewrite(&mut w)?;
                w.flush().map_err(|e| perr(&e))?;
                (r, None)
            }
        };
        crate::sync_file(&out).map_err(|e| perr(&e))?;
        Ok((r, opener))
    })();
    let r = r.and_then(|(r, opener)| {
        let back = Back { tree: &tree, sizes: &sizes, journal: (history == History::Whole).then_some(&journal[..]) };
        checked(a, &out, opener.as_ref(), mirrors, &back, &r.made).map_err(|e| perr(&e))?;
        let perms = file.metadata().map_err(|e| err(&exit::seen(e)))?.permissions();
        fs::set_permissions(&part, perms).map_err(|e| perr(&e))?;
        Ok(r)
    });
    drop(out);
    let r = match r {
        Ok(r) => {
            crate::replace(&part, &target)?;
            r
        }
        Err(e) => {
            _ = fs::remove_file(&part);
            return Err(e);
        }
    };
    let mut what = format!("{}, {} -> {} bytes", changes.join(", "), map.len(), out_len(&target));
    if a.output.is_some() {
        what += &format!(" ({})", target.display());
    }
    if r.segments.0 != r.segments.1 {
        what += &format!(", {} -> {} segments", r.segments.1, r.segments.0);
    }
    tell(&what);
    crate::end(Some(&target), tree.entries.len(), map.len() as u64, out_len(&target), start);
    Ok(())
}

/// The bytes of file `p` (0 where it is not read).
fn out_len(p: &Path) -> u64 {
    fs::metadata(p).map_or(0, |m| m.len())
}

fn bytes(n: u64) -> String {
    crate::decompress::bytes_of(n)
}

/// What an archive converted is to hold, as it is read back ([`checked`]): the input's tree (its
/// contents of `sizes`) and, kept as it was, its journal points.
struct Back<'a> {
    tree: &'a Tree,
    sizes: &'a [u64],
    journal: Option<&'a [(u64, String)]>,
}

/// Whether the archive written to `out` (sealed, where `opener` opens it; mirrored where
/// `mirrors`) holds what `back` tells, and its leaves `made` again evaluate.
fn checked(
    a: &Args,
    out: &fs::File,
    opener: Option<&envelope::Opener>,
    mirrors: bool,
    back: &Back,
    made: &[usize],
) -> Result<(), String> {
    // SAFETY: a file of this process, not written to any more
    let map = unsafe { memmap2::Mmap::map(out) }.map_err(|e| e.to_string())?;
    // (an encrypted one decrypted as it is read: what tells its blocks, then its data as it is
    // evaluated)
    let decrypting = match opener {
        Some(o) => {
            let mut d = o.unseal(&map).map_err(|e| e.to_string())?.decrypting();
            // SAFETY: of the file mapped read only
            unsafe { d.let_go_read() };
            Some(d)
        }
        None => None,
    };
    let decoder = crate::Decoder {
        native: aqr_decompress::Native { threads: a.opt.threads },
        wasm: crate::WASM.get(),
        sandbox: crate::SANDBOX.get().then(std::sync::OnceLock::new),
    };
    let archive = match &decrypting {
        Some(d) => aqr_archive::open_decrypting(d, &decoder),
        None => aqr_archive::open(&map, &decoder),
    };
    let archive = archive.map_err(|e| e.to_string())?;
    // SAFETY: what the mirror blocks are checked by (their blocks, and those mirrored) got as it
    // was opened
    let c = decrypting.as_ref().map_or(&map[..], |d| unsafe { d.unread() });
    let check = match opener {
        Some(_) => aqr_archive::mirror::check_sealed(c, &[], false),
        None => aqr_archive::mirror::check(c),
    };
    if check.mirrored(c, opener.is_some()) != mirrors || check.broken > 0 {
        return Err("the archive written is not mirrored as meant (left as it was)".into());
    }
    if let Some(j) = back.journal {
        let got: Vec<(u64, String)> = archive.journal().iter().map(|p| (p.time, p.label.clone())).collect();
        if got != j {
            return Err("the archive written does not keep the journal points (left as it was)".into());
        }
    }
    crate::edit::holds(a, &archive, back.tree, back.sizes, made)
}

/// The contents of an archive of another format as they are unpacked ([`convert_foreign`]):
/// held as they come, shared (in the store of large buffers where one is set: the spill), and
/// the notes of what is left out told.
struct Holding<'p> {
    input: &'p Path,
    quiet: bool,
    contents: std::sync::Mutex<Vec<Option<aqr_formula::Bytes>>>,
}

impl aqr_unpack::Sink for Holding<'_> {
    fn entries(&self, _: &Tree, _: &[usize]) -> Result<(), String> {
        Ok(())
    }

    fn content(&self, c: usize, d: Vec<u8>) -> Result<(), String> {
        let b = held(d);
        let mut all = self.contents.lock().unwrap();
        if all.len() <= c {
            all.resize(c + 1, None);
        }
        all[c] = Some(b);
        Ok(())
    }

    fn note(&self, what: &str) {
        if !self.quiet {
            eprintln!("{}: {what}", self.input.display());
        }
    }
}

/// `d` as bytes shared (not copied as they are read), moved to the store where it is large and
/// one is set.
fn held(d: Vec<u8>) -> aqr_formula::Bytes {
    let mut b = aqr_formula::Bytes::from(d);
    aqr_formula::store::spill(&mut b);
    match b.is_shared() {
        true => b,
        false => aqr_formula::Bytes::shared(std::sync::Arc::new(b.into_vec())),
    }
}

/// The contents unpacked, as a [`aqr_compress::Source`]: each by the name of its first file.
struct Unpacked {
    names: Vec<Vec<u8>>,
    contents: Vec<aqr_formula::Bytes>,
}

impl aqr_compress::Source for Unpacked {
    fn len(&self) -> usize {
        self.contents.len()
    }

    fn name(&self, c: usize) -> &[u8] {
        &self.names[c]
    }

    fn size(&self, c: usize) -> u64 {
        self.contents[c].len() as u64
    }

    fn read(&self, c: usize) -> std::io::Result<aqr_formula::Bytes> {
        Ok(self.contents[c].clone())
    }
}

/// Converts `input` (mapped: `map`), an archive of format `f` (by the last `suffix` bytes of its
/// name), to an Aqueous archive ([module](self)).
fn convert_foreign(a: &Args, input: &Path, map: &[u8], f: aqr_unpack::Format, suffix: usize) -> Result<(), String> {
    let start = Instant::now();
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", input.display());
    let threads = a.opt.threads;
    let name = input.file_name().map(crate::os_bytes).unwrap_or_default();
    let out = match &a.output {
        Some(o) => o.clone(),
        None => {
            let mut stem = name[..name.len() - suffix].to_vec();
            stem.extend_from_slice(b".aqr");
            input.with_file_name(crate::os_from(&stem))
        }
    };
    let Some(force) = crate::output(a, &out)? else { return Ok(()) };
    let mut opened = aqr_unpack::Opened::open_with(f, map, &a.names.encoding).map_err(|e| err(&exit::seen(e)))?;
    if let Some(t) = opened.names_told()
        && !a.quiet
    {
        eprintln!("{}: {t}", input.display());
    }
    let encrypted = opened.encrypted();
    let needed = encrypted as usize + a.passphrase as usize;
    if !a.pass_from.is_empty() && a.pass_from.len() != needed {
        let n = a.pass_from.len();
        return Err(err(&format!(
            "--passphrase-from: {n} source{} given, {needed} passphrase{} needed (the input's{}, then -P's)",
            plural(n),
            plural(needed),
            if encrypted { "" } else { ": none, as it is not encrypted" }
        )));
    }
    if encrypted {
        let opens = |p: &[u8]| opened.unlock(p).map(|ok| ok.then_some(())).map_err(|e| err(&exit::seen(e)));
        if pass::before(input, a.pass_from.first(), opens)?.is_none() {
            exit::note(exit::Code::Decryption);
            return Err(err(&"encrypted, and no passphrase given opens it (its entries would be left out)"));
        }
    }
    let sealing = match a.passphrase {
        true => Some(pass::new(&out, a.pass_from.get(encrypted as usize))?),
        false => None,
    };
    crate::begin("convert", &[input.to_owned()], Some(&out));
    // (what is decrypted not written out: held in memory where the input or the output is encrypted)
    let sealed = encrypted || sealing.is_some();
    let spill = match crate::compress::spill_of(&out, sealed)? {
        Some(prefix) => Some(aqr_compress::spill::Spill::create(&prefix).map_err(|e| err(&e))?),
        None => None,
    };
    let _store = spill.map(|s| aqr_formula::store::set(Some(std::sync::Arc::new(s))));
    let mut opt = a.opt.clone();
    opt.spill = crate::compress::spill_of(&out, sealed)?;
    opt.size_hint = sealing.is_some();

    // a single file's stream: an archive of the file alone, of the kind of its name
    if !opened.is_tree() {
        let mut w = aqr_formula::store::Writer::new(None);
        opened
            .decompress(
                threads,
                &mut |d| {
                    w.write(d);
                    Ok(())
                },
                &|_| {},
            )
            .map_err(|e| err(&exit::seen(e)))?;
        let d = w.into_bytes();
        let kind = aqr_compress::kind_by_name(&name[..name.len() - suffix]);
        let (n, _) = crate::write_whole(&out, force, |file| match &sealing {
            None => aqr_compress::compress_formula_to(&d, kind, &opt, file, &|_| {}),
            Some(p) => {
                let mut s = crate::compress::sealer(file, p)?;
                aqr_compress::compress_formula_to(&d, kind, &opt, &mut s, &|_| {})?;
                s.finish()?;
                Ok((file.metadata()?.len() as usize, aqr_compress::Formula::default()))
            }
        })?;
        if !a.quiet {
            crate::compress::summary(&out, 1, d.len(), n, start);
        }
        crate::end(Some(&out), 1, map.len() as u64, n as u64, start);
        return Ok(());
    }

    // a tree: unpacked whole, then compressed
    let sink = Holding { input, quiet: a.quiet, contents: Default::default() };
    let done = opened.unpack(threads, &sink).map_err(|e| err(&exit::seen(e)))?;
    if done.skipped > 0 {
        exit::note(exit::Code::Invalid);
        let n = done.skipped;
        return Err(err(&format!("{n} entr{} left out (told above): not converted", if n == 1 { "y" } else { "ies" })));
    }
    let tree = done.tree;
    if tree.entries.is_empty() {
        return Err(err(&"nothing in it to archive"));
    }
    let mut names: Vec<Option<Vec<u8>>> = Vec::new();
    let mut name_of = |c: usize, n: &[u8]| {
        if names.len() <= c {
            names.resize(c + 1, None);
        }
        names[c].get_or_insert_with(|| n.to_vec());
    };
    for e in &tree.entries {
        if let aqr_archive::EntryNode::File(c) = e.node {
            name_of(c, &e.name);
        }
        e.extra.forks.iter().for_each(|f| name_of(f.1, &e.name));
    }
    tree.root.forks.iter().for_each(|f| name_of(f.1, b""));
    let mut held = sink.contents.into_inner().unwrap();
    held.resize(names.len().max(held.len()), None);
    let contents = held
        .into_iter()
        .enumerate()
        .map(|(c, b)| b.ok_or_else(|| err(&format!("content #{c} not unpacked"))))
        .collect::<Result<Vec<_>, String>>()?;
    names.resize(contents.len(), None);
    let src = Unpacked { names: names.into_iter().map(Option::unwrap_or_default).collect(), contents };
    let total: u64 = src.contents.iter().map(|b| b.len() as u64).sum();
    if !a.batch_given {
        opt.batch = crate::compress::fitted_batch(opt.batch, total);
    }
    let r = crate::write_placing(&out, force, |file, _| match &sealing {
        None => {
            let w = std::io::BufWriter::with_capacity(8 << 20, crate::files::Held(&*file));
            let placed = std::sync::atomic::AtomicBool::new(false);
            let mut o = crate::files::FileOut { w, commits: false, once: false, base: 0, place: None, placed: &placed };
            let r = aqr_compress::compress_tree_into(&tree, &src, &opt, &mut o, None, None, &|_| {})?;
            o.flush()?;
            Ok((r, false))
        }
        Some(p) => {
            let mut s = crate::files::Sealed(crate::compress::sealer(file, p)?);
            let mut r = aqr_compress::compress_tree_into(&tree, &src, &opt, &mut s, None, None, &|_| {})?;
            drop(s);
            let len = file.metadata()?.len() as usize;
            (r.overhead, r.len) = (r.overhead + len - r.len, len);
            Ok((r, false))
        }
    })?;
    let files = tree.entries.iter().filter(|e| matches!(e.node, aqr_archive::EntryNode::File(_))).count();
    if !a.quiet {
        crate::compress::summary(&out, files, total as usize, r.len, start);
    }
    crate::end(Some(&out), files, map.len() as u64, r.len as u64, start);
    Ok(())
}
