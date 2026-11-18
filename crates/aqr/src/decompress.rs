//! Decompressing and testing (`-d`, `-t`): archives opened, decrypted, repaired where asked, and
//! extracted.

use super::*;

pub(crate) fn decompress_all(a: &Args) -> Result<(), String> {
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
pub(crate) struct To {
    pub(crate) named: PathBuf,
    base: PathBuf,
}

impl To {
    /// Where `a` has a tree extracted (`foreign`: of another format than Aqueous), and whether
    /// what is there is written over (`None`: the output skipped).
    pub(crate) fn place<'p>(
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

    /// Where `a` has what is decoded of an archive of several files (`is_tree`) or of one written
    /// (nowhere, with `--test` or `-o -`), and whether what is there is written over (`None`: the
    /// output skipped).
    pub(crate) fn destination<'p>(
        &'p self,
        a: &Args,
        foreign: bool,
        is_tree: bool,
        ask: &'p extract::Ask<'p>,
    ) -> Result<Option<(Option<extract::Place<'p>>, bool)>, String> {
        Ok(Some(match (a.test, is_tree) {
            (true, _) => (None, false),
            _ if a.stdout => (None, false),
            (false, true) => match self.place(a, foreign, ask)? {
                Some((p, f)) => (Some(p), f),
                None => return Ok(None),
            },
            (false, false) => match output(a, &self.named)? {
                Some(f) => (None, f),
                None => return Ok(None),
            },
        }))
    }

    /// What a frontend is told of the output at `place` as it begins, where that is known yet.
    pub(crate) fn told<'p>(&'p self, a: &Args, place: &Option<extract::Place<'p>>) -> Option<&'p Path> {
        let told = match place {
            Some(p) => shown(p),
            None => (!a.test).then_some(&*self.named),
        };
        told.map(|t| if a.stdout { Path::new("-") } else { t })
    }
}

/// What a frontend is told of the output of place `p` as it begins, where that is known yet.
pub(crate) fn shown<'p>(p: &extract::Place<'p>) -> Option<&'p Path> {
    match *p {
        extract::Place::Own(o) => Some(o),
        extract::Place::Into(d) if d.as_os_str().is_empty() => Some(Path::new(".")),
        extract::Place::Into(d) => Some(d),
        extract::Place::Auto { .. } => None,
    }
}

/// File `input`, an archive, mapped to be read; errors told as of `shown`.
pub(crate) fn map_archive(input: &Path, shown: &dyn std::fmt::Display) -> Result<memmap2::Mmap, String> {
    let err = |e: std::io::Error| format!("{shown}: {}", exit::seen(e));
    let file = fs::File::open(input).map_err(err)?;
    // SAFETY: the archive is not to change while it is read; where it does, its checksums tell
    unsafe { memmap2::Mmap::map(&file) }.map_err(err)
}

/// The archive in file `c` (`input`) as it is located (spec, "Locating an archive": an archive
/// of a file appended to and not committed, one after other bytes, as a self-extracting
/// executable has it), the bytes before or after it told where not `quiet`; all of `c` where
/// none is found (to be repaired), or where it is encrypted.
pub(crate) fn located<'c>(c: &'c [u8], input: &Path, quiet: bool) -> &'c [u8] {
    let Some(r) = aqr_archive::locate(c) else { return c };
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
pub(crate) fn tell_incomplete(status: Option<aqr_archive::Status>, input: &Path) {
    if let Some(aqr_archive::Status::Incomplete { files, bytes }) = status {
        let left = match (files, bytes) {
            (Some(1), Some(b)) => format!(": 1 file ({}) not archived", size(b as f64)),
            (Some(n), Some(b)) => format!(": {n} files ({}) not archived", size(b as f64)),
            (Some(n), None) => format!(": {n} file{} not archived", plural(n)),
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
    /// Whether the payload decrypted is cut short after a chunk (its last authenticating only as
    /// not the last): what came after it lost, as an archive rolled back to an earlier commit
    /// would be.
    cut: bool,
    mirrors: aqr_archive::mirror::Check,
    restored: Option<aqr_archive::mirror::Repaired>,
}

impl Repair {
    /// The archive of `input` (`plain`, or the payload `decrypting`) read whole and repaired, once
    /// (into `cell`), its repair told.
    pub(crate) fn of<'r>(
        cell: &'r std::cell::OnceCell<Repair>,
        input: &Path,
        plain: &[u8],
        decrypting: Option<&aqr_archive::envelope::Decrypting>,
        threads: usize,
    ) -> Result<&'r Repair, String> {
        if let Some(r) = cell.get() {
            return Ok(r);
        }
        let (decrypted, erased, cut) = match decrypting {
            Some(d) => {
                let (d, erased, cut) = decrypt_whole(d, input, threads)?;
                (Some(d), erased, cut)
            }
            None => (None, Vec::new(), false),
        };
        let read = decrypted.as_deref().unwrap_or(plain);
        // (of a payload, no mirror block found by its magic: it may be a file's content)
        let mirrors = match decrypted {
            Some(_) => aqr_archive::mirror::check_sealed(read, &erased, cut),
            None => aqr_archive::mirror::check(read),
        };
        let restored = aqr_archive::mirror::repair(read, &mirrors, true, &erased);
        if let Some(r) = &restored {
            if r.header {
                eprintln!("{}: its header magic restored", input.display());
            }
            if r.restored > 0 {
                let s = plural(r.restored);
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
        Ok(cell.get_or_init(|| Repair { decrypted, erased, cut, mirrors, restored }))
    }

    /// The archive's bytes as they are (`plain`, or the payload decrypted).
    pub(crate) fn damaged<'r>(&'r self, plain: &'r [u8]) -> &'r [u8] {
        self.decrypted.as_deref().unwrap_or(plain)
    }

    /// The archive's bytes repaired.
    pub(crate) fn read<'r>(&'r self, plain: &'r [u8]) -> &'r [u8] {
        self.restored.as_ref().map_or(self.damaged(plain), |r| &r.bytes)
    }

    /// The bytes of the segment cut short, where one is.
    pub(crate) fn cut_off(&self) -> Option<usize> {
        self.restored.as_ref().and_then(|r| r.cut_off.as_ref()).map(|c| c.len())
    }

    /// The archive opened, of its bytes repaired: salvaged where bytes were lost (their files
    /// lost, the others given back as they check).
    pub(crate) fn open<'r, 'n>(
        &'r self,
        plain: &'r [u8],
        runner: &'n dyn aqr_archive::Runner,
        upto: Option<usize>,
    ) -> Result<aqr_archive::Archive<'r, 'n>, Failure> {
        let read = self.read(plain);
        match (self.restored.as_ref().is_some_and(|r| !r.lost.is_empty()), self.decrypted.is_some(), upto) {
            (true, sealed, _) => aqr_archive::open_salvaging(read, runner, sealed),
            (false, true, _) => aqr_archive::open_payload(read, runner),
            (false, false, Some(m)) => aqr_archive::open_upto(read, runner, m),
            (false, false, None) => aqr_archive::open(read, runner),
        }
    }
}

/// Archive `p` opened by its index ([`aqr_archive::open_indexed`]), of `paths` alone where they are given
/// ([`aqr_archive::open_indexed_at`]).
pub(crate) fn by_index<'c, 'n>(
    p: aqr_archive::Payload<'c>,
    runner: &'n dyn aqr_archive::Runner,
    threads: usize,
    paths: Option<&[Vec<Vec<u8>>]>,
    upto: Option<usize>,
) -> Result<aqr_archive::Archive<'c, 'n>, Failure> {
    match (upto, paths) {
        // (the archive ended at the point: all of what is before it read)
        (Some(m), _) => aqr_archive::open_indexed_upto(p, runner, threads, m),
        (None, Some(paths)) => aqr_archive::open_indexed_at(p, runner, threads, paths),
        (None, None) => aqr_archive::open_indexed(p, runner, threads),
    }
}

aqr_common::zopt! {
    /// For tests: an extraction stopped, as a signal would, once so many contents are written.
    static TEST_STOP_AFTER: Opt<Option<usize>> =
        Opt::value("test:stop-after", None, "stop an extraction as a signal would, after N contents are written");
}

/// Decodes (or tests) `input`, an Aqueous archive, to `to`: whether it was (not skipped, as a
/// frontend answers).
fn decompress_into(a: &Args, input: &Path, to: &To) -> Result<bool, String> {
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", stdio::shown(input));
    // (mapped, so that what is read of it is the system's to drop)
    let c = map_archive(input, &stdio::shown(input))?;
    // an encrypted one decrypted as it is read, into memory (not backed by a file, so that
    // nothing decrypted is written out but the output)
    let decrypting = match aqr_archive::envelope::is_encrypted(&c) || aqr_archive::envelope::is_damaged_encrypted(&c) {
        true => Some(unlock(&c, input)?),
        false if pass::keys().given() => {
            exit::note(exit::Code::Decryption);
            return Err(err(&"not encrypted, though a passphrase was given (-P)"));
        }
        false => None,
    };
    let decoder = Decoder {
        native: Native { threads: a.opt.threads },
        wasm: WASM.get(),
        sandbox: SANDBOX.get().then(OnceLock::new),
    };
    let runner: &dyn aqr_archive::Runner = &decoder;
    let plain: &[u8] = match decrypting {
        Some(_) => &[],
        None => located(&c, input, a.quiet),
    };
    // of a tree, read by its index: the blocks of the files taken alone, tested too (so that what
    // is tested is what is extracted; its mirror blocks then checked apart), and of a single
    // file, read whole anyway
    let payload = match &decrypting {
        Some(d) => aqr_archive::Payload::Sealed(d),
        None => aqr_archive::Payload::Plain(plain),
    };
    // (of paths selected, its metadata blocks read back from the last as far as they tell them)
    let paths: Option<Vec<Vec<Vec<u8>>>> = a.select.as_ref().map(|s| s.paths.iter().map(|p| p.0.clone()).collect());
    // (the archive as it was at a journal point: ended after the segment of the point)
    let upto = match &a.at {
        Some(at) => Some(journal::upto(input, if decrypting.is_some() { &c } else { plain }, at, a.quiet)?),
        None => None,
    };
    let indexed = match payload.of_tree() {
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
    let maybe_cut = footer_made && cut_off.is_none() && archive.status() != Some(aqr_archive::Status::Complete);
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
    let Some((place, force)) = to.destination(a, false, archive.tree().is_some(), &ask)? else { return Ok(false) };
    let told = to.told(a, &place);
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
    } else if rec.is_some() {
        // (a single file: all of the leaves are its)
        archive.set_items(vec![Some(0); archive.leaves_len()]);
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
            meta::Applier::new(false, a.quiet).marking(input).mark(&to.named, meta::Kind::File);
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
            let give = |c: usize, d: Vec<u8>| {
                seen.saw(c, &d, d.len() as u64);
                given.lock().unwrap()[c] = true;
                n.fetch_add(files[c], std::sync::atomic::Ordering::Relaxed);
                total.fetch_add(files[c] as u64 * d.len() as u64, std::sync::atomic::Ordering::Relaxed);
                Ok(())
            };
            let r = ticking(a, &*view, || -> Result<_, String> {
                let mut r = archive.contents_of(a.opt.threads, wanted.as_deref(), give);
                // (read by its index, the files that do not decode so, of it read whole and
                // repaired, as extracting does)
                if fast && r.is_err() {
                    let again: Vec<bool> = {
                        let given = given.lock().unwrap();
                        (0..given.len()).map(|c| !given[c] && wanted.as_ref().is_none_or(|w| w[c])).collect()
                    };
                    let repaired = Repair::of(&repair, input, plain, decrypting.as_ref(), a.opt.threads)?;
                    let read = repaired.read(plain);
                    let whole = match by_index(
                        aqr_archive::Payload::Plain(read),
                        runner,
                        a.opt.threads,
                        paths.as_deref(),
                        upto,
                    ) {
                        Ok(whole) => Some(whole),
                        Err(_) if paths.is_none() => repaired.open(plain, runner, upto).ok(),
                        Err(_) => None,
                    };
                    if let Some(mut whole) = whole.filter(|w| w.contents_len() == again.len()) {
                        whole.set_budget(aqr_common::budget::global());
                        r = whole.contents_of(a.opt.threads, Some(&again), give);
                    }
                }
                Ok(r)
            })?;
            lost = not_given(r, &given.into_inner().unwrap(), wanted.as_deref()).map_err(|e| err(&exit::seen(e)))?;
            (n.into_inner(), total.into_inner(), None)
        }
        Some(t) => {
            let ap = meta::Applier::new(a.preserve, a.quiet).marking(input);
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
                let after = TEST_STOP_AFTER.get();
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
                    let whole = match by_index(
                        aqr_archive::Payload::Plain(read),
                        runner,
                        a.opt.threads,
                        paths.as_deref(),
                        upto,
                    ) {
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
    // (tested: its mirror blocks, of it as read, whole)
    let whole_payload;
    let checked: Option<(std::borrow::Cow<aqr_archive::mirror::Check>, &[u8])> = match repair.get() {
        _ if !a.test => None,
        Some(r) => Some((std::borrow::Cow::Borrowed(&r.mirrors), r.read(plain))),
        None => {
            let read = match &decrypting {
                Some(d) => {
                    whole_payload = d.get(0..d.len()).map_err(|e| err(&exit::seen(e)))?;
                    whole_payload
                }
                None => plain,
            };
            let m = match decrypting {
                Some(_) => aqr_archive::mirror::check_sealed(read, &[], false),
                None => aqr_archive::mirror::check(read),
            };
            Some((std::borrow::Cow::Owned(m), read))
        }
    };
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
            let s = plural(n);
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
            && let Some((m, read)) = &checked
            && (a.verbose || !m.mirrors.is_empty() || m.broken > 0)
        {
            // (as repaired, where some of it was: its blocks read by its index as damaged)
            let repaired = match repair.get() {
                Some(r) if fast => Some(r.open(plain, runner, upto).map_err(|e| err(&exit::seen(e)))?),
                _ => None,
            };
            tell_archival(
                input,
                repaired.as_ref().unwrap_or(&archive),
                m,
                read,
                decrypting.is_some().then_some(&c[..]),
            )
            .map_err(|e| err(&exit::seen(e)))?;
        }
    }
    // (tested by its index, its blocks taken as they are: a mirror block intact but differing
    // from its block, which decodes, is of an archive not as it was written, the block extracted
    // not the one a repair would take)
    if fast
        && repair.get().is_none()
        && let Some((m, read)) = &checked
        && let n @ 1.. = m.differing(read).count()
    {
        exit::note(exit::Code::Invalid);
        return Err(err(&format!("{n} block{} differing from intact mirror blocks", plural(n))));
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
        return Err(err(&format!("{n} file{} lost", plural(n))));
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
    if repair.get().is_some_and(|r| r.cut) {
        exit::note(exit::Code::Invalid);
        return Err(err(&"cut short after an encrypted chunk: what came after it lost"));
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
/// ([`aqr_archive::mirror::cut_segment`]): each output a file of no name, written into directory `into`
/// (made) where given, as `leaf-N` (`leaf-N-K` of a leaf of several outputs). Returns how many,
/// and their bytes.
fn salvage_unnamed(
    a: &Args,
    damaged: &[u8],
    cut: &std::ops::Range<usize>,
    erased: &[std::ops::Range<usize>],
    runner: &dyn aqr_archive::Runner,
    into: Option<&Path>,
) -> Result<(usize, u64), String> {
    let Some(c) = aqr_archive::mirror::cut_segment(damaged, cut, erased) else { return Ok((0, 0)) };
    let mut archive = aqr_archive::open_salvaging(&c, runner, false).map_err(|e| e.to_string())?;
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
    archive: &aqr_archive::Archive,
    mirrors: &aqr_archive::mirror::Check,
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
        && envelope.is_none_or(|e| e.starts_with(&aqr_archive::envelope::ENCRYPTED_MIRRORED_HEADER_MAGIC));
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
/// passphrase given ([`pass`]); damaged ([`aqr_archive::envelope::open_damaged`]), its header from a
/// mirrored header, and its chunks that do not authenticate decrypted without it, told: their
/// bytes in the payload, not to be trusted.
pub(crate) fn decrypt(
    c: &[u8],
    input: &Path,
    threads: usize,
) -> Result<(memmap2::Mmap, Vec<std::ops::Range<usize>>), String> {
    let (d, erased, cut) = decrypt_whole(&unlock(c, input)?, input, threads)?;
    if cut {
        exit::note(exit::Code::Invalid);
        return Err(format!("{}: cut short after an encrypted chunk: what came after it lost", input.display()));
    }
    Ok((d, erased))
}

/// The payload of encrypted archive `c` (of `input`) unlocked by a passphrase given ([`pass`]),
/// to be decrypted as it is read; damaged ([`aqr_archive::envelope::open_damaged`]), its header from a
/// mirrored header, told.
pub(crate) fn unlock<'c>(c: &'c [u8], input: &Path) -> Result<aqr_archive::envelope::Decrypting<'c>, String> {
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", input.display());
    let (e, opened) = aqr_archive::envelope::open_damaged(c).map_err(|e| err(&exit::seen(e)))?;
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
/// authenticate decrypted without it, told: their bytes in the payload, not to be trusted; and
/// whether it is cut short after its last chunk (told).
fn decrypt_whole(
    d: &aqr_archive::envelope::Decrypting,
    input: &Path,
    threads: usize,
) -> Result<(memmap2::Mmap, Vec<std::ops::Range<usize>>, bool), String> {
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", input.display());
    let u = d.unlocked();
    let mut d = memmap2::MmapMut::map_anon(u.len()).map_err(|e| err(&exit::seen(e)))?;
    let (failed, cut) = u.decrypt_lossy(&mut d, threads);
    if cut {
        eprintln!("{}: cut short after an encrypted chunk: what came after it lost", input.display());
    }
    if !failed.is_empty() {
        let s = plural(failed.len());
        eprintln!("{}: {} encrypted chunk{s} not authenticating, not trusted", input.display(), failed.len());
    }
    let erased = failed.iter().map(|&m| u.chunk_range(m)).collect();
    Ok((d.make_read_only().map_err(|e| err(&exit::seen(e)))?, erased, cut))
}

/// By content of `archive` (decoded into `out`), the path of its first file in the archive (of
/// a single file, `out`).
fn content_paths(archive: &aqr_archive::Archive, out: &Path) -> Vec<String> {
    let Some(t) = archive.tree() else { return vec![out.display().to_string()] };
    let mut paths = tree_content_paths(t);
    paths.resize(archive.contents_len(), String::new());
    paths
}

/// By content of tree `t` (as far as its files tell), the path of its first file, as the
/// archive has it (its names between `/`, as on any system).
pub(crate) fn tree_content_paths(t: &Tree) -> Vec<String> {
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
fn leaf_names(archive: &aqr_archive::Archive, out: &Path) -> Vec<String> {
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
pub(crate) struct Decoder {
    pub(crate) native: Native,
    pub(crate) wasm: bool,
    pub(crate) sandbox: Option<OnceLock<Result<sandbox::Sandbox, String>>>,
}

impl aqr_archive::Runner for Decoder {
    fn load(&self, wasm: &[u8]) -> aqr_archive::Decoded<Box<dyn aqr_archive::Loaded + '_>> {
        let sandboxed = aqr_decompress::known(wasm).is_none() || self.wasm && sandbox::SUPPORTED;
        let Some(s) = self.sandbox.as_ref().filter(|_| sandboxed) else {
            return if self.wasm { Wasm.load(wasm) } else { self.native.load(wasm) };
        };
        let s = s.get_or_init(|| match sandbox::SUPPORTED {
            true => sandbox::Sandbox::current_exe([SANDBOX_WORKER])
                .map(|s| s.title("aqr: sandbox worker"))
                .map_err(|e| e.to_string()),
            false => Err("this system does not lock workers down (-Z no-sandbox to run the modules not known in \
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
pub(crate) fn tell_names(a: &Args, input: &Path, told: impl IntoIterator<Item = String>) {
    if !a.quiet {
        told.into_iter().for_each(|t| eprintln!("{}: {t}", stdio::shown(input)));
    }
}

pub(crate) fn summary_d(input: &Path, out: Option<&Path>, files: usize, total: u64, start: Instant) {
    let t = start.elapsed().as_secs_f64();
    let to = match out {
        None => " (tested)".into(),
        Some(o) if o.as_os_str().is_empty() => " -> .".into(),
        Some(o) => format!(" -> {}", stdio::shown(o)),
    };
    eprintln!(
        "{}{to}: {files} file{}, {} in {}, {:.1} MB/s",
        stdio::shown(input),
        plural(files),
        size(total as f64),
        aqr_l10n::duration(t, 2),
        total as f64 / 1e6 / t.max(1e-9)
    );
}
