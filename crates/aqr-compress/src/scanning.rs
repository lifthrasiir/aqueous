//! Data scanned for the files in it ([`scan`]): found, made ahead on the pool ([`ahead`]), or
//! later, apart ([`make_later`]).

use super::*;

/// The files in `d`, if any is found: from the first byte on, a file whose recompressor takes
/// it (up to its [`extent`]) is put in place, and the scan goes on after it; else, so that
/// files in one turned down are still found, within it by its parts where its recompressor
/// tells them ([`within`]), or on from the next byte. Weak signatures are confirmed by the
/// extent: a zlib stream by its checksum, MPEG audio frames without a tag by one following
/// another. Where the data is known to start with a file of `first` ([`Nest::As`]), it is that
/// file, or none where that is turned down and tells nothing of its parts.
pub(crate) fn scan(d: &[u8], first: Option<Format>, opt: &Options, depth: u32, cx: &Cx) -> Option<Made> {
    scan_with(d, first, opt, depth, SCAN_SLACK.get(), cx)
}

/// [`scan`] with `slack` for [`SCAN_SLACK`].
pub(crate) fn scan_with(
    d: &[u8],
    first: Option<Format>,
    opt: &Options,
    depth: u32,
    slack: usize,
    cx: &Cx,
) -> Option<Made> {
    let (found, close) = find(d, first, opt, depth, slack, cx)?;
    // each file with the data before it, and the data after the last and the whole
    // (a slice of all of it anywhere: at the last byte of a block, over the most blocks)
    let around = description_bound(&Node::slice(0, aqr_archive::BLOCK as u64 - 1, d.len() as u64));
    let sizes: Vec<_> = found.iter().map(|(_, n, child)| (*n, bound_of(child).saturating_add(around))).collect();
    let keep = fit(around + 11, &sizes, opt.description);

    // the data between the files cut out of `d` once put in place
    let mut f = Formula::default();
    f.push("raw", Vec::new());
    let found: Vec<Found> = found.into_iter().zip(keep).filter_map(|(c, keep)| keep.then_some(c)).collect();
    let close = closes_held(close, &found);
    let (mut gaps, mut parts, mut at, mut len) = (Vec::new(), Vec::new(), 0, 0);
    // (the bytes from `at` to `p` as they are: a slice of the data between the files)
    let mut gap = |gaps: &mut Vec<Range<usize>>, at: usize, p: usize| {
        gaps.push(at..p);
        len += p - at;
        Node::slice(0, (len - (p - at)) as u64, (p - at) as u64)
    };
    let mut found = found.into_iter().peekable();
    let mut close = close.into_iter().peekable();
    loop {
        let next = found.peek().map(|c| c.0);
        // a file kept beside what was found in it as it was turned down ([`find`]): those, then
        // it
        if let Some(&(p, ..)) = close.peek()
            && next.is_none_or(|q| p <= q)
        {
            let (p, n, made, sizes) = close.next().expect("peeked");
            parts.push(gap(&mut gaps, at, p));
            let mut kept = Vec::new();
            at = p;
            while let Some(&(q, m, _)) = found.peek()
                && q + m <= p + n
            {
                let (q, m, child) = found.next().expect("peeked");
                kept.push(gap(&mut gaps, at, q));
                kept.push(f.embed(child));
                at = q + m;
            }
            kept.push(gap(&mut gaps, at, p + n));
            at = p + n;
            let made = f.embed(made);
            let e = aqr_formula::Either { kept: Node::concat(kept), made, len: n as u64, sizes };
            parts.push(Node::Either(Box::new(e)));
            continue;
        }
        let Some((p, n, child)) = found.next() else { break };
        parts.push(gap(&mut gaps, at, p));
        parts.push(f.embed(child));
        at = p + n;
    }
    if parts.is_empty() {
        return None;
    }
    parts.push(gap(&mut gaps, at, d.len()));
    f.root = Node::concat(parts);
    Some(Made { f, gaps: Some(gaps), close: None })
}

/// A file found by a scan that lost to its data by little ([`find`]): where it starts, its
/// length, its formula and the sizes compared.
type Close = (usize, usize, Formula, (usize, usize));

/// Of the files `close` kept beside their data ([`find`]), those whose bytes hold every file
/// found in them (`found`, in order) whole, and no other of them: the others are turned down
/// as they were.
fn closes_held(close: Vec<Close>, found: &[Found]) -> Vec<Close> {
    let mut held: Vec<Close> = Vec::new();
    for c in close {
        let (p, end) = (c.0, c.0 + c.1);
        let crossing = found.iter().any(|&(q, m, _)| (q < p && q + m > p) || (q >= p && q < end && q + m > end));
        if crossing || held.last().is_some_and(|h| h.0 + h.1 > p) {
            continue;
        }
        held.push(c);
    }
    held
}

/// A file found in data: where it starts, its length and its formula.
type Found = (usize, usize, Formula);

/// The files of [`scan_with`], in order; `None` where `d` starts with a file of `first` that is
/// turned down and whose parts are not told ([`regions`]). Within a file turned down whose parts
/// are told, files are looked for by them ([`within`]), else from each byte on.
fn find(
    d: &[u8],
    first: Option<Format>,
    opt: &Options,
    depth: u32,
    slack: usize,
    cx: &Cx,
) -> Option<(Vec<Found>, Vec<Close>)> {
    let (mut found, mut close) = (Vec::new(), Vec::new());
    let (mut p, mut budget) = (0, d.len().saturating_mul(2).saturating_add(slack));
    // Where each format's last file turned down ends: within it, a file of that format again
    // would be part of the same (an archive from each of its local headers, MPEG audio from
    // each of its frames), and is not tried.
    let mut turned_down: Vec<(Format, usize)> = Vec::new();
    let mut made = Ahead::default();
    // (for a file that is all of `d` only)
    let mut sealing = cx.sealing.take();
    while p < d.len() {
        // at the start, a file may also be known by its end (a LevelDB table), or be one that
        // is only ever a whole file (a SQLite database)
        let format = match first {
            Some(f) if p == 0 => Some(f),
            _ if p == 0 => detect(d),
            _ => detect::starts(&d[p..]),
        };
        let format = format.filter(|f| !turned_down.iter().any(|&(g, end)| g == *f && p < end));
        let mut down = None;
        let child = format.filter(|_| budget > 0).and_then(|format| {
            let n = extent(format, &d[p..]).filter(|&n| n > 0)?;
            if format == Format::Bvx && format_on(format, opt) && !made.0.contains_key(&p) {
                ahead(d, p, &turned_down, opt, depth, cx, &mut made);
            }
            // (where it may be, a file that loses by little is kept beside what is found in it
            // as it is turned down, [`scan_with`])
            let piece = &d[p..p + n];
            let id = (piece.as_ptr() as usize, piece.len());
            let defer = cx.defers.get() && !(p == 0 && n == d.len() && sealing.is_some());
            let outer = cx.defer_for.replace(defer.then_some(id));
            let child = match made.0.remove(&p) {
                Some(a) => a.take(piece, cx),
                None if p == 0 && n == d.len() => {
                    *cx.sealing.borrow_mut() = sealing.take();
                    let f = recompress_as(format, d, opt, depth, cx);
                    sealing = cx.sealing.take();
                    f
                }
                None => recompress_as(format, piece, opt, depth, cx),
            };
            cx.defer_for.set(outer);
            let child = match (child, cx.closed.take().filter(|c| c.0 == id)) {
                (Some(f), Some((_, sizes))) => {
                    close.push((p, n, f, sizes));
                    None
                }
                (child, _) => child,
            };
            if child.is_none() {
                budget = budget.saturating_sub(n);
                turned_down.retain(|&(g, _)| g != format);
                turned_down.push((format, p + n));
                down = Some((format, n));
            }
            Some((n, child?))
        });
        match child {
            Some((n, child)) => {
                // (`-Z dump:scan`: each file found, for a measurement; not in builds for
                // distribution)
                #[cfg(not(aqr_dist))]
                if SCAN_DUMP.get() {
                    eprintln!("SCAN\t{}\t{depth}\t{p}\t{n}\t{:?}", d.len(), format);
                }
                found.push((p, n, child));
                p += n;
            }
            None => match down.and_then(|(format, n)| Some((n, regions(format, &d[p..p + n])?))) {
                Some((n, parts)) => {
                    let inner = within(&d[p..p + n], &parts, opt, depth, cx);
                    found.extend(inner.into_iter().map(|(q, m, f)| (p + q, m, f)));
                    p += n;
                }
                // (a file of `first` turned down, telling nothing of its parts: nothing found, but
                // where it was kept beside its data)
                None if p == 0 && first.is_some() => {
                    made.give_back(cx);
                    return (!close.is_empty()).then_some((Vec::new(), close));
                }
                None => p = detect::next_start(d, p + 1),
            },
        }
    }
    made.give_back(cx);
    Some((found, close))
}

/// LZFSE and LZVN streams made ahead of a scan ([`ahead`]), by where they start: each its
/// formula, if taken, what is put off of its prevalidation, and the bytes of its data taken
/// from the budget.
#[derive(Default)]
struct Ahead(HashMap<usize, Early>);

struct Early {
    f: Option<Formula>,
    deferred: Vec<(usize, verify::Deferred)>,
    /// What it noted ([`Cx::coded_held`]).
    notes: HashSet<(usize, u64)>,
    inflated: u64,
    /// What the encoder it was sized with kept of the streams it compressed (without its context).
    enc: aqr_archive::Kept,
}

impl Early {
    /// The formula of stream `d`, as [`recompress_as`] would give it in `cx`.
    pub(crate) fn take(self, d: &[u8], cx: &Cx) -> Option<Formula> {
        cx.deferred.borrow_mut().extend(self.deferred);
        cx.adopt_notes(self.notes);
        // (so that what holds it is sized without compressing its streams again)
        cx.enc.adopt(self.enc);
        if self.f.is_some() {
            cx.enc.forget(d);
        }
        self.f
    }
}

impl Ahead {
    /// Gives the budget of the streams the scan did not come to back to `cx`.
    pub(crate) fn give_back(self, cx: &Cx) {
        let n: u64 = self.0.values().map(|m| m.inflated).sum();
        cx.inflate.set(cx.inflate.get() + n);
    }
}

aqr_common::zopt! {
    /// At most this many streams are made ahead at once ([`ahead`]): a number of its own, not the
    /// threads', so that the budget they take (and the archive) does not depend on the threads.
    static AHEAD: Opt<usize> =
        Opt::value("bvx:ahead", 64, "most LZFSE and LZVN streams made ahead of the scan at once");
}

/// Makes the LZFSE and LZVN streams of `d` from `p` on at once, into `made`, as the scan of
/// [`find`] would come to them one after another if each is taken: up to [`AHEAD`], up to the
/// next file of another format, or to a stream whose data does not fit in the budget left (in
/// the scan's order). Each is made in a context of its own ([`Apart`]), its data taken from
/// `cx`'s budget beforehand, with no more budget for its contents. An asset catalog holds
/// thousands of them, which the scan would make one after another (Windows App's of 72 MB, its
/// data 1.1 GB, at 1.8 threads).
fn ahead(d: &[u8], p: usize, turned_down: &[(Format, usize)], opt: &Options, depth: u32, cx: &Cx, made: &mut Ahead) {
    let mut streams = Vec::new();
    let mut q = p;
    let most = AHEAD.get();
    while q < d.len() && streams.len() < most {
        let format = match q == p {
            true => Some(Format::Bvx),
            false => detect::starts(&d[q..]),
        };
        match format.filter(|f| !turned_down.iter().any(|&(g, end)| g == *f && q < end)) {
            None => {
                q = detect::next_start(d, q + 1);
                continue;
            }
            Some(Format::Bvx) => {}
            Some(_) => break,
        }
        let Some(n) = extent(Format::Bvx, &d[q..]).filter(|&n| n > 0) else { break };
        if !made.0.contains_key(&q) {
            let Some((_, m)) = aqr_bvx::bvx::extent(&d[q..q + n]) else { break };
            if !cx.inflate(m) {
                break;
            }
            streams.push((q, n, m));
        }
        q += n;
    }
    let apart = cx.apart();
    let out = aqr_common::pool::par_map(streams.len(), opt.threads, |i| {
        let (q, n, m) = streams[i];
        let own = apart.cx(n, opt);
        own.inflate.set(m);
        let f = recompress_as(Format::Bvx, &d[q..q + n], opt, depth, &own);
        (f, own.deferred.into_inner(), own.coded_held.into_inner(), own.enc.into_kept())
    });
    for ((q, _, inflated), (f, deferred, notes, enc)) in streams.into_iter().zip(out) {
        made.0.insert(q, Early { f, deferred, notes, inflated, enc });
    }
}

/// The files in `d`, a file turned down whose parts are `parts` ([`regions`]), in order: a part
/// of a format is that file, taken as one up to its [`extent`] (by its parts again where turned
/// down; what follows its end in the part is its own, and not looked into), the bytes no part
/// tells of and parts of unknown kind are scanned, and the other parts are not looked into
/// ([`Nest::of`]).
fn within(d: &[u8], parts: &[Region], opt: &Options, depth: u32, cx: &Cx) -> Vec<Found> {
    let mut found = Vec::new();
    // (not from the first byte, where `d` itself starts)
    let scan_part = |found: &mut Vec<Found>, r: Range<usize>| {
        let r = r.start.max(1)..r.end;
        if !r.is_empty()
            && let Some((inner, _)) = find(&d[r.clone()], None, opt, depth, SCAN_SLACK.get(), cx)
        {
            found.extend(inner.into_iter().map(|(q, n, f)| (r.start + q, n, f)));
        }
    };
    let mut at = 0;
    for part in parts {
        let r = part.span.clone();
        if r.start < at || r.end > d.len() {
            continue;
        }
        scan_part(&mut found, at..r.start);
        match Nest::Scan.of(part.kind) {
            None => {}
            Some(Nest::As(format)) => {
                let file = &d[r.clone()];
                let format = detect::named(format, file);
                let n = extent(format, file).filter(|&n| n > 0).unwrap_or(file.len());
                match recompress_with(format, &file[..n], part.contents, opt, depth, cx) {
                    Some(f) => found.push((r.start, n, f)),
                    None => {
                        let inner = regions(format, &file[..n]).map(|p| within(&file[..n], &p, opt, depth, cx));
                        found.extend(inner.into_iter().flatten().map(|(q, m, f)| (r.start + q, m, f)));
                    }
                }
            }
            Some(_) => scan_part(&mut found, r.clone()),
        }
        at = r.end;
    }
    scan_part(&mut found, at..d.len());
    found
}

/// What the parts of `d`, a file of `format`, are, where its recompressor tells.
fn regions(format: Format, d: &[u8]) -> Option<Vec<Region>> {
    caught(format.name(), d.len(), || format.spec().regions.and_then(|r| r(d))).flatten()
}

/// Which of `parts` (bytes of data and of description each) to keep, so that `base` and the
/// descriptions of those kept take at most `limit`: those with the fewest bytes per byte of
/// description go first.
fn fit(base: usize, parts: &[(usize, usize)], limit: usize) -> Vec<bool> {
    let mut keep = vec![true; parts.len()];
    let mut total = parts.iter().fold(base as u128, |t, p| t + p.1 as u128);
    let mut order: Vec<usize> = (0..parts.len()).collect();
    // by n / desc, as n * desc' < n' * desc
    order.sort_by_key(|&i| std::cmp::Reverse(parts[i].1 as u128 * u64::MAX as u128 / (parts[i].0 as u128 + 1)));
    for i in order {
        if total <= limit as u128 {
            break;
        }
        keep[i] = false;
        total -= parts[i].1 as u128;
    }
    keep
}

/// The contents of `f` ([`contents`]), a formula of data `depth`
/// formats deep, each its slice and its formula to put in place of the slice ([`place_all`])
/// where it has any application (as many as fit in [`Options::description`]), and the plan of
/// the sets of them (whose members with a parent have no formula); `None` where none has any
/// and there is no set.
pub(crate) fn nested(f: &mut Formula, of: &[u8], opt: &Options, depth: u32, cx: &Cx) -> Option<(Contents, sets::Plan)> {
    f.mark_contents();
    if depth + 1 >= opt.depth {
        return None;
    }
    // (where [`recompress_with`] lets `f`'s contents be sealed: only by [`make_later`])
    let sealing = cx.sealing.take();
    let (prepared, plan) = prepare(f, opt, depth, cx);
    let mut children = Vec::new();
    // (told once: it walks all of `f`, which an image's thousands of files would each do again)
    let apart = bundle::contents_apart(f);
    for n in shared_nodes(f).iter().chain([&f.root]) {
        contents(n, f, apart, opt, depth, cx, &mut children);
    }
    // (sets read their members' bytes from the formula once made: those of a tree's top too)
    let top = depth == 0 && cx.top_sets.is_some();
    make_later(f, of, &|n| !top && !plan.holds(n), opt, depth, cx, sealing, &mut children);
    // (those not taken: the contents' own nested contents make and take their own)
    let mut ready = cx.ready.borrow_mut();
    prepared.iter().for_each(|k| drop(ready.remove(k)));
    drop(ready);
    let mut skip = cx.skip.borrow_mut();
    plan.children().for_each(|k| _ = skip.remove(&k));
    drop(skip);
    let sizes: Vec<_> = children
        .iter()
        .map(|(slice, child)| match child {
            Some(c) => (slice.len() as usize, bound_of(&c.f).saturating_sub(description_bound(slice))),
            None => (0, 0),
        })
        .collect();
    let keep = fit(bound_of(f), &sizes, opt.description);
    if plan.is_empty() && !children.iter().zip(&keep).any(|((_, child), &keep)| keep && child.is_some()) {
        return None;
    }
    let children = children.into_iter().zip(keep).map(|((slice, child), keep)| (slice, child.filter(|_| keep)));
    Some((children.collect(), plan))
}

/// Makes the contents of [`Cx::later`] (the files of an archive) at once, on up to
/// [`Options::threads`] threads, each in a context of its own (with the formula made of it
/// beforehand, if any, and the same prevalidator), into `children`; what is put off of their
/// prevalidation goes to `cx`'s. A content that is all of its stream, and its only slice, takes
/// the stream's bytes out of `f`, and is cut out of them as soon as it is made: they are let go
/// of then rather than once all are made (the data of a WIM file's resources, a stream each),
/// where nothing else holds them (only those of slices that `take`s). Where it takes no place
/// they are put back. Their formulas are sealed by `sealing` where `f` makes its files apart.
#[allow(clippy::too_many_arguments)]
fn make_later(
    f: &mut Formula,
    of: &[u8],
    take: &dyn Fn(&Node) -> bool,
    opt: &Options,
    depth: u32,
    cx: &Cx,
    sealing: Option<seal::Sealing>,
    children: &mut Contents,
) {
    let later = std::mem::take(&mut *cx.later.borrow_mut());
    if later.is_empty() {
        return;
    }
    // contents of data not made yet (aqr-wim's), made as each is come to, and the application of
    // each then evaluated with it (by its stream: the application, and where its output is in
    // `of`)
    let lazy: Vec<Option<usize>> = later
        .iter()
        .map(|(_, n, _)| match *n {
            Node::Slice { stream, .. } if f.streams.get(stream).is_some_and(|s| s.data.is_lazy()) => Some(stream),
            _ => None,
        })
        .collect();
    let apps = applications_of_lazy(f);
    let slices = slice_counts(f);
    let whole = |f: &Formula, n: &Node| match *n {
        Node::Slice { stream, start: 0, len } if take(n) => {
            (slices[stream] == 1 && len > 0 && len == f.streams[stream].data.len() as u64).then_some(stream)
        }
        _ => None,
    };
    let owned: Vec<Option<usize>> = later.iter().map(|(_, n, _)| whole(f, n)).collect();
    let members: Vec<bool> = later.iter().map(|(_, n, _)| !take(n)).collect();
    // (those not taken made now, as others are)
    for (l, o) in lazy.iter().zip(&owned) {
        if let (Some(s), None) = (l, o) {
            f.streams[*s].data = f.streams[*s].data.made();
        }
    }
    let lazy: Vec<Option<usize>> = lazy.into_iter().zip(&owned).map(|(l, o)| l.filter(|_| o.is_some())).collect();
    let pre = cx.pre.clone().filter(|_| lazy.iter().flatten().any(|s| apps.contains_key(s)));
    cx.parts_checked.set(cx.parts_checked.get() || pre.is_some());
    // the contents' formulas sealed as they are made, in their order, where this formula's
    // contents are: each in its slot, those of a stream taken that take no place as they are;
    // those that would pass the bound of the description not (kept as they are), the bound
    // taken in their order (the sizes as [`nested`] fits them)
    let sealing = sealing.filter(|_| bundle::files_apart(f));
    let slots = sealing.as_ref().map(|s| s.order.slots(s.slot, later.len()));
    let budget = Arc::new(std::sync::Mutex::new(bound_of(f)));
    let sealed: Arc<Vec<std::sync::Mutex<Option<Made>>>> =
        Arc::new((0..later.len()).map(|_| std::sync::Mutex::new(None)).collect());
    let data = |n: &Node| {
        let &Node::Slice { stream, start, len } = n else { unreachable!("a slice") };
        &f.streams[stream].data[start as usize..][..len as usize]
    };
    // (the data that is made already)
    let peek = |n: &Node| match n {
        &Node::Slice { stream, .. } if f.streams[stream].data.is_lazy() => None,
        n => Some(data(n)),
    };
    // the formulas made beforehand, by where their data is
    let mut by_at: HashMap<usize, Vec<_>> = HashMap::new();
    for (k, v) in std::mem::take(&mut *cx.ready.borrow_mut()) {
        by_at.entry(k.0).or_default().push((k, v));
    }
    let ready: Vec<std::sync::Mutex<Vec<_>>> = later
        .iter()
        .map(|(_, n, _)| {
            std::sync::Mutex::new(peek(n).and_then(|d| by_at.remove(&(d.as_ptr() as usize))).unwrap_or_default())
        })
        .collect();
    cx.ready.borrow_mut().extend(by_at.into_values().flatten());
    let images: Vec<&[u8]> = later.iter().filter_map(|(_, n, _)| peek(n)).filter(|d| exe::remappable(d)).collect();
    let remaps = (images.len() > 1).then(|| Arc::new(exe::plan(&images, opt.threads, opt.block / 2)));
    let apart = Apart { remaps, ..cx.apart() };
    let files = cx.files.as_deref();
    // the costliest first, so that a long one is not left to the end, alone (but in their
    // order where sealed: each waits for those before it)
    let mut order: Vec<usize> = (0..later.len()).collect();
    if sealing.is_none() {
        order.sort_by_cached_key(|&i| {
            std::cmp::Reverse(peek(&later[i].1).map_or(later[i].1.len() * formats::Spec::DEFAULT.cost, cost))
        });
    }
    // (the bytes of the streams taken, shared so as to be moved without a copy)
    let taken: Vec<std::sync::Mutex<Option<Bytes>>> = owned
        .iter()
        .map(|&s| {
            std::sync::Mutex::new(s.map(|s| {
                let mut b = std::mem::take(&mut f.streams[s].data);
                b.share();
                b
            }))
        })
        .collect();
    let fr = &*f;
    let data = |n: &Node| {
        let &Node::Slice { stream, start, len } = n else { unreachable!("a slice") };
        &fr.streams[stream].data[start as usize..][..len as usize]
    };
    let make = |i: usize| {
        let (_, n, nest) = &later[i];
        let mine = taken[i].lock().unwrap().take().map(|b| b.made());
        let d = mine.as_deref().unwrap_or_else(|| data(n));
        // (the application of data made now evaluated with it)
        if let (Some(pre), Some((app, at))) = (&pre, lazy[i].and_then(|s| apps.get(&s))) {
            let s = lazy[i].expect("a stream");
            let part = fr.part(app, &|x| match x == s {
                true => mine.clone().expect("taken"),
                false => fr.streams[x].data.clone(),
            });
            let out = &of[*at..*at + app.len() as usize];
            pre.submit(Arc::new(part), out);
            // (its output hashed: of a WIM file mapped, its chunks, read through again)
            aqr_formula::store::let_go(out);
        }
        let mut own = apart.cx(d.len(), opt);
        *own.sealing.get_mut() =
            sealing.as_ref().zip(slots.as_ref()).map(|(s, slots)| seal::Sealing { slot: slots[i], ..s.clone() });
        own.ready.borrow_mut().extend(std::mem::take(&mut *ready[i].lock().unwrap()));
        files.inspect(|f| f.tell(d.as_ptr(), false));
        let _item = aqr_common::trace::item(files.and_then(|f| f.content(d.as_ptr())));
        // (a formula that loses by little kept beside its data, but where it is sealed as it is made)
        let unsealed = own.sealing.borrow().is_none();
        let mut child = match unsealed {
            true => made_deferring(d, opt, depth + 1, *nest, &own),
            false => made(d, opt, depth + 1, *nest, &own),
        }
        .filter(|c| takes_place(&c.f));
        files.inspect(|f| f.tell(d.as_ptr(), true));
        if let Some((s, slots)) = sealing.as_ref().zip(slots.as_ref()) {
            // (not a member of a set planned, which the sets are made of)
            let member = members[i];
            // (where the data is of a file mapped, an image's file of the input: read through once
            // sealed, but in verifying)
            let input = (d.as_ptr() as usize, d.len());
            // (what those within it that are not sealed kept beside their data, [`made_deferring`]:
            // nothing chooses once it is sealed, so the data kept, as it was compared)
            let child = child.map(|c| choose::kept(c.cut(d)));
            let size = child.as_ref().map(|c| bound_of(c).saturating_sub(description_bound(n)));
            // (the bytes taken let go of now where they have a formula, made again of it in the
            // rare case it is turned down for the bound; but kept where formulas are sealed in
            // it, an image's files, whose streams are no longer at hand to make it)
            let class = owned[i].map(|s| (fr.streams[s].class.clone(), fr.streams[s].kind));
            let sunk = child.as_ref().is_some_and(|c| c.streams.iter().any(|s| s.data.sunk_in().is_some()));
            let raw = mine.filter(|_| child.is_none() || sunk);
            let (budget, sealed, threads, limit) = (budget.clone(), sealed.clone(), opt.threads, opt.description);
            s.order.submit(
                slots[i],
                Box::new(move |sink| {
                    let mut made = child.filter(|_| !member);
                    let mut b = budget.lock().unwrap();
                    let mut raw = raw;
                    match size.filter(|_| made.is_some()) {
                        Some(n) if *b + n <= limit => *b += n,
                        // (of the bytes taken only: those of a slice stay in their stream)
                        _ => {
                            if let (None, Some(m), Some(_)) = (&raw, made.take(), &class) {
                                raw = Some(m.eval(&Native { threads }).expect("the formula of a content").into());
                            }
                        }
                    }
                    drop(b);
                    // (or the bytes taken as they are, which nothing else holds)
                    let made = made.or_else(|| {
                        raw.zip(class).map(|(d, (class, kind))| {
                            let mut f = Formula::default();
                            f.root = f.push_as(class, kind, d);
                            choose::kept(f)
                        })
                    });
                    if let Some(mut m) = made {
                        memory::declare(&mut m, &Native { threads }, &memory::Limits::default());
                        sink.seal(&mut m).expect("sealed");
                        *sealed[i].lock().unwrap() = Some(Made::from(m.into_inner()));
                        aqr_formula::store::let_go_at(input.0, input.1);
                    }
                }),
            );
            return (None, None, own.deferred.into_inner(), own.coded_held.into_inner(), None);
        }
        // (cut out of the bytes taken, which are let go of; put back where it takes no place, or
        // is kept beside them: they are its data as it is, of their class, [`place_slices`])
        let back = match mine {
            Some(mine) if child.as_ref().is_some_and(|c| c.close.is_none()) => {
                child = child.map(|c| Made::from(c.cut(&mine)));
                None
            }
            mine => mine,
        };
        child.iter_mut().for_each(|c| aqr_formula::store::spill_streams(&mut c.f));
        (child, back, own.deferred.into_inner(), own.coded_held.into_inner(), Some(own.enc.into_kept()))
    };
    let made = match sealing.as_ref().zip(slots.as_ref()) {
        // in their order, each let in once the content [`SEAL_AHEAD`] times the threads before it
        // is sealed (the contents made and waiting for their turn held: a long one before them
        // holding it), on workers as they are free (no thread waiting holds one, so that what
        // runs, a long content's own, can take them)
        Some((s, slots)) => aqr_common::pool::dispatch(
            later.len(),
            opt.threads,
            |i| {
                if let Some(back) = i.checked_sub(SEAL_AHEAD.get() * opt.threads.max(1)) {
                    s.order.wait_done(slots[back]);
                }
            },
            |i, ()| make(i),
        ),
        None => aqr_common::pool::par_map(later.len(), opt.threads, |j| make(order[j])),
    };
    if let Some(s) = &sealing {
        s.order.wait(s.slot);
    }
    let mut made: Vec<_> = order.into_iter().zip(made).collect();
    made.sort_unstable_by_key(|&(i, _)| i);
    for (((at, ..), (i, (child, back, deferred, notes, enc))), s) in later.into_iter().zip(made).zip(owned) {
        if let (Some(s), Some(back)) = (s, back) {
            f.streams[s].data = back;
        }
        children[at].1 = child.or_else(|| sealed[i].lock().unwrap().take());
        cx.deferred.borrow_mut().extend(deferred);
        // (in the contents' order, all of them made: none saw another's)
        cx.adopt_notes(notes);
        // (so that what holds it is sized and written without compressing its streams again)
        enc.into_iter().for_each(|e| cx.enc.adopt(e));
    }
}
