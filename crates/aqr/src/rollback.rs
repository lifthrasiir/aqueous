//! `--rollback SPEC ARCHIVE`: the archive cut back in place to the state of a journal point
//! (`docs/append-design.md`, "Journaling"; spec, "Appending to an archive": roll back), what was written after
//! the point's segment taken out of the file: not written again as `--compact` writes it, nor
//! kept as `--delete` keeps what it takes away.
//!
//! The archive is opened as `-a` opens one (locked, an append not committed cut off; not one
//! encrypted, of mirror blocks or after other data: `--convert --keep-until` writes the state into
//! another file of those). The footer the commit after the point's segment made a spacer block is
//! written again ([`aqr_archive::rollback`]), its first bytes last, so that the file holds the
//! archive as it was or as it is cut back whatever happens meanwhile; then the file is cut after
//! it. Where the file cannot be cut (Windows does not cut one that another program has mapped,
//! as a folder of it in Explorer may), it is tried again for a while, then left holding what
//! follows as trailing bytes, which readers leave out and the next append cuts off.

use std::time::{Duration, Instant};

use crate::{Args, exit, journal};

/// How long the file is tried to be cut where another program has it mapped.
const MAPPED_WAIT: Duration = Duration::from_secs(5);

/// `--rollback SPEC ARCHIVE` ([module](self)).
pub fn rollback(a: &Args) -> Result<(), String> {
    let at = a.rollback.as_ref().ok_or("--rollback takes a point")?;
    let [path] = &a.inputs[..] else { return Err("--rollback takes one archive".into()) };
    let start = Instant::now();
    crate::begin("rollback", std::slice::from_ref(path), Some(path));
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    let io = |e: std::io::Error| err(&exit::seen(e));
    let prior = crate::append::open(path, a.quiet)?.into_prior();
    let base = prior.at.start;
    let (plan, told) = {
        // SAFETY: the archive is locked against other writers; where it changes all the same, its
        // checksums tell
        let map = unsafe { memmap2::Mmap::map(&prior.file) }.map_err(io)?;
        let c = &map[prior.at.start as usize..prior.at.end as usize];
        let archive = aqr_archive::open(c, &aqr_archive::NoModules).map_err(|e| err(&e))?;
        let points = archive.journal();
        let picked = journal::pick(points, at).map_err(|e| err(&format!("--rollback: {e}")))?;
        let n = points.iter().position(|p| p.metadata == picked.metadata && p.time == picked.time).unwrap_or(0);
        // (the points after the segment's: those of later segments)
        let later = points.iter().filter(|p| p.metadata > picked.metadata).count();
        let segments = archive.segment_metadata().map_err(|e| err(&e))?;
        let dropped = segments.iter().filter(|&&m| m > picked.metadata).count();
        let told = format!("#{} {} {:?}", n + 1, journal::show(picked.time), picked.label);
        if dropped == 0 {
            drop(archive);
            if !a.quiet {
                eprintln!("{}: as of {told} already: nothing after it to take out", path.display());
            }
            crate::end(Some(path), 0, c.len() as u64, 0, start);
            return Ok(());
        }
        let plan = aqr_archive::rollback(c, picked.metadata, base)
            .ok_or_else(|| err(&"no metadata block where the point is"))?;
        let what = match later {
            0 => format!("{dropped} segment{} after it", crate::plural(dropped)),
            n => format!("{dropped} segment{} after it, {n} later point{}", crate::plural(dropped), crate::plural(n)),
        };
        (plan, (told, what, c.len()))
    };
    let (point, what, was) = told;
    let file = &prior.file;
    for (at, bytes) in &plan.writes {
        crate::write_at(file, base + *at as u64, bytes).map_err(io)?;
        crate::sync_file(file).map_err(io)?;
    }
    // (cut back now, as of the point: what follows taken out of the file where it can be)
    let len = base + plan.len as u64;
    let waited = Instant::now();
    let cut = loop {
        match file.set_len(len) {
            Ok(()) => break Ok(()),
            Err(e) if mapped(&e) && waited.elapsed() < MAPPED_WAIT => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => break Err(e),
        }
    };
    let now = match cut {
        Ok(()) => {
            crate::sync_file(file).map_err(io)?;
            len
        }
        Err(e) if mapped(&e) => {
            eprintln!(
                "{}: another program has it open, so what was after the point is left at its end, out of the \
                 archive (the next -a takes it out)",
                path.display()
            );
            file.metadata().map_err(io)?.len()
        }
        Err(e) => return Err(io(e)),
    };
    if !a.quiet {
        let how = if plan.restored { "" } else { ", its footer made again" };
        eprintln!("{}: back to {point} ({what} taken out{how}), {was} -> {} bytes", path.display(), plan.len);
    }
    crate::end(Some(path), 0, was as u64, now - base, start);
    Ok(())
}

/// Whether `e` is that the file cannot be cut as another program has it mapped (Windows'
/// `ERROR_USER_MAPPED_FILE`).
fn mapped(e: &std::io::Error) -> bool {
    cfg!(windows) && e.raw_os_error() == Some(1224)
}
