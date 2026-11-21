//! Files written whole or in place: under a name of their own until written, synced, and
//! renamed into place.

use super::*;

/// Writes a new file `path` (or one in place of it, where `force`) by `write`: under a name of
/// its own beside it, renamed once it is written (so that `path` is never written in part),
/// removed where it is not (`write` failing, or the process stopped: [`stop`]). The file is
/// opened to be read as well (an archive read back to be verified, [`aqr_compress::Sink`]).
pub(crate) fn write_whole<T>(
    path: &Path,
    force: bool,
    write: impl FnOnce(&mut fs::File) -> std::io::Result<T>,
) -> Result<T, String> {
    write_placing(path, force, |file, _| write(file).map(|t| (t, false)))
}

/// The name under which `path` is written before it is renamed: beside it, of the process.
pub(crate) fn part_of(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(format!(".{}.part", std::process::id()));
    path.with_file_name(name)
}

/// The most of an archive's footer (and its padding) kept in memory to be written back as an
/// append is cancelled.
const FOOTER_RESTORE: u64 = 16 << 20;

/// The archive `out` appended to, as [`append::Opened::prior`] found it, of what `g` holds (to be
/// appended), committed as it is written (once as it ends, with `--atomic`): what became of it,
/// its length and overhead those of what was added (the archive's growth, as the summary tells
/// it against the bytes read).
pub(crate) fn appended(
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
    let end = p.at.end;
    // (a cancelled append is cut back to `end`, the footer it makes a spacer block of, or cuts
    // off where it is written over, written back: the archive as it was)
    let footer_at = p.at.start + p.footer.start as u64;
    let footer = (end - footer_at <= FOOTER_RESTORE).then(|| {
        let mut f = file;
        let mut bytes = vec![0; (end - footer_at) as usize];
        std::io::Seek::seek(&mut f, std::io::SeekFrom::Start(footer_at))
            .and_then(|_| std::io::Read::read_exact(&mut f, &mut bytes))
            .and_then(|()| Ok((std::sync::Arc::new(file.try_clone()?), footer_at, bytes)))
    });
    let restore = footer.and_then(Result::ok);
    // (a footer across a sector of the file written over, from where it starts)
    if !p.kill {
        if a.verbose {
            eprintln!("{}: its footer across a sector of the file: written over", out.display());
        }
        file.set_len(p.at.start + p.footer.start as u64).map_err(|e| err(&exit::seen(e)))?;
    }
    let _undo = stop::undo_by(stop::Undo::Truncate(out.to_owned(), end, restore));
    let mut f = file;
    std::io::Seek::seek(&mut f, std::io::SeekFrom::End(0)).map_err(|e| err(&exit::seen(e)))?;
    let from =
        aqr_compress::Appending { footer: p.footer, metadata: p.metadata, link_groups: p.link_groups, kill: p.kill };
    let placed = std::sync::atomic::AtomicBool::new(true);
    let w = std::io::BufWriter::with_capacity(8 << 20, Held(file));
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
pub(crate) fn stopped_tree(
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
    if !placed.swap(true, std::sync::atomic::Ordering::SeqCst)
        && let Err(e) = place(&part_of(out), out, force)
    {
        eprintln!("aqr: {e}");
        stop::now(1);
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
pub(crate) struct FileOut<'f> {
    pub(crate) w: std::io::BufWriter<Held<'f>>,
    /// Whether it is committed as it is written, once as it ends (an archive appended to with
    /// `--atomic`), and where the archive starts in the file.
    pub(crate) commits: bool,
    pub(crate) once: bool,
    pub(crate) base: u64,
    /// Where it is to be placed (`part`, its output, whether over a file there): none where it is
    /// in place (an archive appended to).
    pub(crate) place: Option<(PathBuf, PathBuf, bool)>,
    pub(crate) placed: &'f std::sync::atomic::AtomicBool,
}

/// A file written to as [`stop::held`] has it: not as the process is stopped and the output undone.
pub(crate) struct Held<'f>(pub(crate) &'f fs::File);

/// The most bytes written at once ([`Held`]): on Windows, a write of 64 MiB or more ran at 1.6-1.85
/// GB/s, of 4-16 MiB at 4.5 GB/s (512 MiB to a new file on NTFS, Ryzen 7 9700X); an archive's
/// batch, written whole, is of hundreds of MB.
const WRITE_AT_ONCE: usize = 8 << 20;

impl Write for Held<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let buf = &buf[..buf.len().min(WRITE_AT_ONCE)];
        stop::held(|| (&*self.0).write(buf))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
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
        sync_file(self.w.get_ref().0)?;
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
        stop::held(|| write_at(self.w.get_ref().0, self.base + at, bytes))
    }
}

/// Makes what was written to `f` durable, in the order written (on macOS, where `fsync` leaves
/// it to the drive, through its cache).
pub(crate) fn sync_file(f: &fs::File) -> std::io::Result<()> {
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
pub(crate) fn write_at(f: &fs::File, at: u64, bytes: &[u8]) -> std::io::Result<()> {
    aqr_common::fsio::write_all_at(f, bytes, at)?;
    // (Windows moves it)
    std::io::Seek::seek(&mut &*f, std::io::SeekFrom::End(0)).map(|_| ())
}

/// An encrypted archive ([`sealer`]) as an [`aqr_compress::Output`]: not committed, its last
/// chunk sealed as it ends.
pub(crate) struct Sealed<'f>(pub(crate) aqr_archive::envelope::Sealer<&'f mut fs::File>);

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
pub(crate) fn write_placing<T>(
    path: &Path,
    force: bool,
    write: impl FnOnce(&mut fs::File, &Path) -> std::io::Result<(T, bool)>,
) -> Result<T, String> {
    // (placing it tells its path already, as it fails at its first commit: told once)
    let err = |p: &Path, e: std::io::Error| match e.to_string() {
        m if m.starts_with(&format!("{}: ", p.display())) => m,
        m => format!("{}: {m}", p.display()),
    };
    path.file_name().ok_or(format!("{}: not a file's name", path.display()))?;
    let part = part_of(path);
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
pub(crate) fn create(path: &Path, force: bool) -> Result<fs::File, String> {
    let err = |e: std::io::Error| format!("{}: {e}", path.display());
    if force && fs::symlink_metadata(path).is_ok_and(|m| !m.is_dir()) {
        fs::remove_file(path).map_err(err)?;
    }
    fs::OpenOptions::new().write(true).create_new(true).open(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => format!("{} exists (-f to overwrite)", path.display()),
        _ => err(e),
    })
}
