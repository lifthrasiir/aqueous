//! Archives of other formats (ZIP, 7z, tar, ...) extracted or tested through aqr-unpack.

use super::*;

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

    fn step(&self, s: aqr_archive::Step) {
        if let Some(v) = self.view {
            v.tell(s);
        }
    }
}

/// Extracts (or tests) `input`, an archive of format `format`, to `to`: as an Aqueous archive
/// is ([`extract::Extractor`]); an error at the end where entries were left out. Whether it
/// was (not skipped, as a frontend answers).
pub(crate) fn decompress_foreign(a: &Args, input: &Path, to: &To, format: aqr_unpack::Format) -> Result<bool, String> {
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", stdio::shown(input));
    let c = map_archive(input, &stdio::shown(input))?;
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
    let Some((place, force)) = to.destination(a, true, opened.is_tree(), &ask)? else { return Ok(false) };
    let told = to.told(a, &place);
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
        let step = |s: aqr_archive::Step| shown.into_iter().for_each(|v| v.tell(s));
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
            let n = write_whole(&to.named, force, |w| {
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
            })?;
            meta::Applier::new(false, a.quiet)
                .marking(input, a.mark.unwrap_or_default())
                .mark(&to.named, meta::Kind::File);
            n
        };
        (1, n, None, 0, (!a.test).then(|| to.named.clone()))
    } else {
        let ap = meta::Applier::new(a.preserve, a.quiet).marking(input, a.mark.unwrap_or_default());
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
            hints: Vec::new(),
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
