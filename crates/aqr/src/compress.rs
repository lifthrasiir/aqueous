//! Compressing (`-c`, `-a`): the inputs gathered and archived, as Aqueous archives, archives of
//! other formats or self-extracting programs, with the progress shown.

use super::*;

pub(crate) fn compress_all(a: &Args) -> Result<(), String> {
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
        return remove(&compress_into(a, Inputs::of(a).from(skip), &out)?);
    }
    match &a.output {
        Some(out) => remove(&compress_into(a, Inputs::of(a), out)?),
        None => {
            // (where some of an input is kept, the next ones still archived and removed)
            let mut kept = Ok(());
            for each in Inputs::of(a).each() {
                let out = with_ext(&each.paths[0], &a.format.ext());
                let gone = compress_into(a, each, &out)?;
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
    let gone = compress_into(a, Inputs { paths: &inputs, given: &given, renames: &a.renames }, &out)?;
    if a.stdout && !stdio::streamed() {
        stdio::copy_out(&out)?;
    }
    remove(&gone)
}

/// A path to be removed with `--rm` (an input archived, or an archive extracted), as it was
/// read: kept where it is found changed since (written over meanwhile, or the output put in
/// its place).
pub(crate) struct Gone {
    path: PathBuf,
    dir: bool,
    /// Its size, modification time and (on Unix) device and inode, as read.
    seen: (u64, Option<std::time::SystemTime>, (u64, u64)),
}

impl Gone {
    pub(crate) fn of(path: &Path, m: &fs::Metadata) -> Gone {
        Gone { path: path.to_owned(), dir: m.is_dir(), seen: Gone::seen(m) }
    }

    pub(crate) fn seen(m: &fs::Metadata) -> (u64, Option<std::time::SystemTime>, (u64, u64)) {
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
pub(crate) fn remove(gone: &[Gone]) -> Result<(), String> {
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
pub(crate) struct Gathered {
    pub(crate) tree: Tree,
    /// By content: the path (read as the archive is made), as bytes (the name telling its
    /// kind) and its size as gathered.
    pub(crate) contents: Vec<(PathBuf, Vec<u8>, u64)>,
    pub(crate) skipped: Vec<String>,
    /// What could not be read of the metadata.
    pub(crate) warnings: Vec<String>,
    /// What the files are read by, mapped into memory.
    mapper: Mapper,
    pub(crate) reader: meta::Reader,
    /// By inode (device and number) of more links than one, the files (entries) of it.
    pub(crate) inodes: std::collections::HashMap<(u64, u64), Vec<usize>>,
    /// With `--rm`, the paths archived (each directory before what it holds), as read.
    pub(crate) gone: Option<Vec<Gone>>,
    /// With `-p`, the access times of the paths read, put back once they are archived.
    pub(crate) atimes: Atimes,
    /// The archive appended to (`-a`, its path as canonicalized), not archived into itself where
    /// it is among the inputs (a file Windows has locked as it is appended to).
    pub(crate) itself: Option<PathBuf>,
}

/// The access times of the paths read as they were before (`-p`, as they are archived), put
/// back as this is dropped ([`meta::restore_atime`]): once the archive is made, or has failed.
#[derive(Default)]
pub(crate) struct Atimes(Option<Vec<(PathBuf, aqr_archive::Time)>>);

impl Atimes {
    /// Notes the access time of `path`, as `m` read it, where they are kept.
    pub(crate) fn note(&mut self, path: &Path, m: &fs::Metadata) {
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
    pub(crate) fn new(mapper: Mapper, preserve: bool) -> Gathered {
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
            itself: None,
        }
    }

    /// The gathering storing no marks of the files where `unmarked` (`--mark=never`).
    pub(crate) fn unmarked(mut self, unmarked: bool) -> Gathered {
        self.reader.unmarked = unmarked;
        self
    }

    /// The link groups of the files of an inode, where there are more than one of it: numbered
    /// from 0, in the order their inodes were first found.
    pub(crate) fn link_groups(&mut self) {
        let mut groups: Vec<&Vec<usize>> = self.inodes.values().filter(|f| f.len() > 1).collect();
        groups.sort_unstable_by_key(|f| f[0]);
        for (g, files) in groups.into_iter().enumerate() {
            files.iter().for_each(|&i| self.tree.entries[i].extra.link_group = Some(g as i128));
        }
    }

    /// The bytes of the files' contents (and forks') as decompressing writes them: each file of
    /// a link group for its size, though the content is read, and archived, once.
    pub(crate) fn input_bytes(&self) -> u64 {
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

/// Compresses `inputs` into `out`.
fn compress_into(a: &Args, inputs: Inputs, out: &Path) -> Result<Vec<Gone>, String> {
    match a.format {
        Pack::Other(..) if a.append => return Err(format!("{}: Aqueous archives alone are appended to", a.verb())),
        Pack::Other(..) if a.journal.is_some() => return Err("-j: Aqueous archives alone have a journal".into()),
        Pack::Sfx => drop(sfx::pack()?),
        _ => {}
    }
    // (whether it is written over decided first, so that the hooks run of an output written alone;
    // appending, the archive there already, to be appended to)
    let force = match a.append {
        true => false,
        false => match output(a, out)? {
            Some(f) => f,
            None => return Ok(Vec::new()),
        },
    };
    hooks::run(a, hooks::When::Before, out)?;
    let gone = match a.format {
        Pack::Aqr => compress_aqr(a, inputs, out, force),
        Pack::Other(f, level) => pack_into(a, f, level, inputs, out, force),
        Pack::Sfx => sfx_into(a, inputs, out, force),
    }?;
    hooks::run(a, hooks::When::After, out)?;
    if !matches!(a.format, Pack::Other(..)) {
        hooks::still_there(a, out)?;
    }
    Ok(gone)
}

/// Compresses `inputs` into self-extracting program `out` ([`sfx`]): the Aqueous archive made
/// beside it first (a single file in it by its name), then the program of it.
fn sfx_into(a: &Args, inputs: Inputs, out: &Path, force: bool) -> Result<Vec<Gone>, String> {
    let renames = match inputs.sole() {
        Some(input) if !input.is_dir() => vec![input.file_name().map(PathBuf::from)],
        _ => inputs.renames.to_vec(),
    };
    let mut tmp = out.as_os_str().to_owned();
    tmp.push(format!(".{}.aqr", std::process::id()));
    let tmp = PathBuf::from(tmp);
    let made = compress_aqr(a, Inputs { renames: &renames, ..inputs }, &tmp, a.force).and_then(|gone| {
        let archive = fs::read(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
        // (the settings of .aqr/sfx/, read back of the archive made: what makes it again)
        let settings = sfx::settings(&archive, a.opt.threads)?;
        let made = sfx::make(&archive, settings.as_ref())?;
        // (written beside it and renamed: not a program in part where the disk is full)
        files::write_whole(out, force, |f| std::io::Write::write_all(f, &made.exe))?;
        Ok((gone, made, archive.len()))
    });
    let _ = fs::remove_file(&tmp);
    let (gone, made, archived) = made?;
    if !a.quiet {
        eprintln!(
            "{}: self-extracting{}, {} module{} native ({} shared librar{}){}: {} ({} of the blobs, {} of the archive){}",
            out.display(),
            if made.settings { " (the settings of .aqr/sfx/)" } else { "" },
            made.natives,
            plural(made.natives),
            made.libs,
            if made.libs == 1 { "y" } else { "ies" },
            if made.interp { " and the interpreter" } else { "" },
            size(made.exe.len() as f64),
            size(made.bundle as f64),
            size(archived as f64),
            match a.verbose {
                true => format!(", pack {}", sfx::pack_id().get(..12).unwrap_or("-")),
                false => String::new(),
            }
        );
    }
    Ok(gone)
}

/// Compresses `inputs` into Aqueous archive `out` ([`compress_into`]).
fn compress_aqr(a: &Args, inputs: Inputs, out: &Path, force: bool) -> Result<Vec<Gone>, String> {
    if a.append && (a.passphrase || a.opt.mirrors) {
        return Err(format!(
            "{}: not with {}",
            a.verb(),
            if a.passphrase { "-P (an encrypted archive is not appended to)" } else { "-A" }
        ));
    }
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
        inputs.paths,
        Some(out),
    );
    let start = Instant::now();
    let rec = stats::record(a.stats, "compress", inputs.shown(), out.display().to_string(), a.opt.threads);
    let mut opt = a.opt.clone();
    let sealing = pass::keys().sealing();
    opt.spill = spill_of(out, sealing.is_some())?;
    opt.size_hint = sealing.is_some();
    let opt = &opt;
    let meta = |p: &Path| fs::symlink_metadata(p).map_err(|e| format!("{}: {e}", p.display()));
    let mapper = Mapper::new().map_err(|e| format!("files not to be mapped: {e}"))?;
    // a single file: an archive of the file alone (but renamed: in a tree, at its name)
    let followed = match inputs.sole() {
        Some(input) if a.dereference => Some(follow(input)?),
        _ => None,
    };
    let single = match followed.as_ref().or(inputs.sole()) {
        Some(input) if !a.append => Some((input, meta(input)?)).filter(|(_, m)| m.is_file()),
        _ => None,
    };
    if single.is_some() && a.journal.is_some() {
        return Err("-j: not with an archive of a single file (it has no segments to name)".into());
    }
    if single.is_some() && a.exclude.excludes(&archive_names(&inputs.given[0], None)?) {
        return Err(format!("nothing to archive in {} (-x)", inputs.paths[0].display()));
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
        let mut g = Gathered::new(mapper, a.preserve).unmarked(a.mark == Some(meta::Marks::Never));
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
                    // (not told of a file archived alone)
                    classes: Vec::new(),
                    same_as: None,
                    modules: f.applies().iter().map(|m| m.module.to_string()).collect(),
                }],
                fallbacks: fallbacks.clone(),
                trace: rec.finish(),
            });
        }
        return Ok(a.rm.then(|| Gone::of(input, &m)).into_iter().collect());
    }

    let mut g = Gathered::new(mapper, a.preserve).unmarked(a.mark == Some(meta::Marks::Never));
    g.gone = a.rm.then(Vec::new);
    g.itself = opened.as_ref().and_then(|_| fs::canonicalize(out).ok());
    let gathering = aqr_common::trace::span("gather");
    // (what a session's inventory gathered of the same, if any)
    if let Some(cached) = session::cached(a, inputs) {
        g = cached;
    } else {
        gather_listed(a, inputs, &mut g)?;
    }
    if g.tree.entries.is_empty() {
        return Err(format!("nothing to archive in {}", inputs.paths[0].display()));
    }
    g.link_groups();
    drop(gathering);
    if !a.quiet {
        for s in &g.skipped {
            eprintln!("skipped {s}");
        }
        g.warnings.iter().for_each(|w| eprintln!("{w}"));
        if a.format != Pack::Sfx && sfx::settings_dir(&g.tree).is_some() {
            eprintln!(".aqr/sfx/: archived as files (the settings of a self-extracting program: --format sfx)");
        }
    }
    // (a self-extracting program's settings first, a segment of their own: what its stub reads
    // alone, docs/sfx-config-design.md, 2.1)
    let led;
    let opt = match aqr_sfx::config::files_of(&g.tree) {
        f if a.format == Pack::Sfx && !f.is_empty() => {
            led = aqr_compress::Options { lead: f.into_iter().map(|(_, c)| c).collect(), ..opt.clone() };
            &led
        }
        _ => opt,
    };
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
                let w = std::io::BufWriter::with_capacity(8 << 20, crate::files::Held(&*file));
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
        if a.verbose && r.alternatives.0 > 0 {
            let (n, made, bytes) = r.alternatives;
            eprintln!("formulas kept beside their data to choose: {n} ({} alone), {made} taken", size(bytes as f64));
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
                classes: f.classes.clone(),
                same_as: f.same_as,
                modules: f.modules.iter().map(|m| m.to_string()).collect(),
            })
            .collect();
        s.add(stats::Run {
            mode: "compress",
            input: inputs.shown(),
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
    inputs: Inputs,
    out: &Path,
    force: bool,
) -> Result<Vec<Gone>, String> {
    begin("compress", inputs.paths, Some(out));
    let start = Instant::now();
    let renames = match inputs.sole() {
        Some(input) => vec![input.file_name().map(PathBuf::from)],
        None => inputs.renames.to_vec(),
    };
    let inputs = Inputs { renames: &renames, ..inputs };
    let mapper = Mapper::new().map_err(|e| format!("files not to be mapped: {e}"))?;
    let mut g = Gathered::new(mapper, a.preserve).unmarked(a.mark == Some(meta::Marks::Never));
    g.gone = a.rm.then(Vec::new);
    if let Some(cached) = session::cached(a, inputs) {
        g = cached;
    } else {
        gather_listed(a, inputs, &mut g)?;
    }
    if g.tree.entries.is_empty() {
        return Err(format!("nothing to archive in {}", inputs.paths[0].display()));
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

/// An archive written to `file`, encrypted with passphrase `p` ([`aqr_archive::envelope`]): with
/// mirrored headers, at age's work factor.
fn sealer<'f>(file: &'f mut fs::File, p: &[u8]) -> std::io::Result<aqr_archive::envelope::Sealer<&'f mut fs::File>> {
    aqr_archive::envelope::Sealer::new(file, p, aqr_archive::envelope::WORK_FACTOR, true)
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
pub(crate) fn spill_of(out: &Path, sealed: bool) -> Result<Option<PathBuf>, String> {
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
pub(crate) fn bars() -> bool {
    std::io::stderr().is_terminal() && view::takes_escapes()
}

/// `work` done telling `view` how far it is (nothing, with -q), its bar drawn as [`ticking`].
pub(crate) fn viewed<T>(
    a: &Args,
    view: &view::View,
    work: impl FnOnce(&(dyn Fn(aqr_compress::Event) + Sync)) -> T,
) -> T {
    ticking(a, view, || {
        let quiet = |_: aqr_compress::Event| {};
        let tell: &(dyn Fn(aqr_compress::Event) + Sync) = if a.quiet { &quiet } else { &|e| view.tell(e) };
        work(tell)
    })
}

/// `work` done with `view`'s bar drawn again now and then (for the time a file takes) until it
/// is done, and once more then (none, with -q).
pub(crate) fn ticking<T>(a: &Args, view: &dyn view::Ticking, work: impl FnOnce() -> T) -> T {
    let bars = !a.quiet && view.shows();
    let r = ticked(bars, &|| view.tick(), work);
    if bars {
        view.tick();
    }
    r
}

/// `work` done with `tick` called now and then (every 250 ms, where `on`) until it is done, or
/// panics: the ticker stopped as it unwinds too, which the scope waits for before passing the
/// panic up.
pub(crate) fn ticked<T>(on: bool, tick: &(dyn Fn() + Sync), work: impl FnOnce() -> T) -> T {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct Stop<'a>(&'a AtomicBool, Option<std::thread::Thread>);
    impl Drop for Stop<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
            if let Some(t) = &self.1 {
                t.unpark();
            }
        }
    }
    let done = AtomicBool::new(false);
    std::thread::scope(|s| {
        let ticker = on.then(|| {
            s.spawn(|| {
                while !done.load(Ordering::Relaxed) {
                    std::thread::park_timeout(std::time::Duration::from_millis(250));
                    if !done.load(Ordering::Relaxed) {
                        tick();
                    }
                }
            })
        });
        let _stop = Stop(&done, ticker.as_ref().map(|t| t.thread().clone()));
        work()
    })
}

fn summary(out: &Path, files: usize, total: usize, len: usize, start: Instant) {
    let t = start.elapsed().as_secs_f64();
    eprintln!(
        "{}: {files} file{}, {} -> {} ({}) in {}, {:.1} MB/s",
        stdio::shown(out),
        plural(files),
        size(total as f64),
        size(len as f64),
        percent(len as f64, total as f64),
        aqr_l10n::duration(t, 2),
        total as f64 / 1e6 / t.max(1e-9)
    );
}
