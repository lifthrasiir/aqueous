//! The code of the module `dh-lods` (FORMAT.md, §8): Distant Horizons' blobs' contents, one after
//! another in the order zstd-frames makes their frames in, the coarser levels' made of the finer
//! ones' (`crate::dh`). Each content is stored, or a blob of a parent made of its four quadrants:
//! each the merge of a child that came before it (its blobs kept as they came, then its quadrant
//! made and kept until the parent takes it), or stored. A part module: the steps and the stored
//! bytes taken in order, the contents put in order; what it holds (blobs, quadrants, a parent's
//! blobs to come) is in a pool of its own, of the size the params tell.

use aqr_api::stream::{Put, Reader};
use aqr_zstd::frames::Pool;

use crate::dh::{self, Section, States, Work};
use crate::{Result, corrupt};

/// What a step is (its tag's low bits, its length the high ones): a content stored; a content
/// stored that is a blob of a child (varints of its row and its blob, and of the last of the row's
/// eight the bytes of its quadrant); a blob of a parent made
/// (varints of its row and its blob; the parent's first, then of each quadrant 0 where it is stored
/// or its child's row plus one, then 1 where the parent is a child too, else 0).
pub const PLAIN: u64 = 0;
pub const OBSERVED: u64 = 1;
pub const DERIVED: u64 = 2;
pub const SHIFT: u32 = 2;

/// Pages of the windows: of the steps and the states, and of the stored bytes and the output.
pub const SMALL_WINDOW: u32 = 1;
pub const WINDOW: u32 = 16;

/// Bytes of the readers' pieces.
pub const SMALL_PIECE: usize = 1 << 12;
pub const BIG_PIECE: usize = 1 << 16;

/// The params (FORMAT.md): varints of the rows, the states and their bytes, the most points of a
/// section and of a column, the most bytes of a quadrant, and the pool's bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Params {
    pub rows: u64,
    pub states: u64,
    pub state_bytes: u64,
    pub points: u64,
    pub column: u64,
    pub quadrant: u64,
    pub pool: u64,
}

impl Params {
    const FIELDS: usize = 7;

    pub fn parse(p: &[u8]) -> Result<Params> {
        let mut at = 0;
        let mut v = [0u64; Self::FIELDS];
        for x in &mut v {
            *x = aqr_common::varint::get(p, &mut at).map_err(|_| corrupt())?;
        }
        let [rows, states, state_bytes, points, column, quadrant, pool] = v;
        let p = Params { rows, states, state_bytes, points, column, quadrant, pool };
        let ok = at == p_len(p)
            && rows <= 1 << 26
            && states <= 1 << 20
            && state_bytes <= 1 << 26
            && points <= 1 << 22
            && (1..=u16::MAX as u64).contains(&column)
            && quadrant <= 1 << 26
            && pool <= 1 << 32;
        if ok { Ok(p) } else { Err(corrupt()) }
    }

    pub fn bytes(&self) -> Vec<u8> {
        let mut v = Vec::new();
        for x in [self.rows, self.states, self.state_bytes, self.points, self.column, self.quadrant, self.pool] {
            aqr_common::varint::put(&mut v, x);
        }
        v
    }

    /// The most bytes of a blob of columns a section of `points` points makes ([`dh::write`]: a
    /// count of 3 bytes at most for each column, and for each point 24 at most).
    pub fn blob_max(&self) -> usize {
        24 * self.points as usize + 3 * dh::COLUMNS + 16
    }

    /// The most bytes of a section's mapping.
    pub fn mapping_max(&self) -> usize {
        4 + 2 * self.states as usize + self.state_bytes as usize
    }
}

fn p_len(p: Params) -> usize {
    p.bytes().len()
}

/// A row of the steps: its blobs kept (of a child, until it has all eight; of a parent, those to
/// come), and its quadrant kept (its block, and whether a worker is still making it).
#[derive(Debug, Clone, Copy, Default)]
struct Row {
    blobs: [(u64, u32); 8],
    /// The blobs it has (a child), or has to put (a parent).
    child: u8,
    parent: u8,
    quadrant: Option<(u64, u32)>,
    pending: bool,
}

/// What makes a child's quadrant of its eight blobs: the states of its own mapping, and buffers.
pub struct Maker {
    states: States,
    w: Work,
    kid: Section,
    quad: Section,
    pub out: Vec<u8>,
}

impl Maker {
    pub fn new(p: &Params) -> Maker {
        let (points, column, states, bytes) =
            (p.points as usize, p.column as usize, p.states as usize, p.state_bytes as usize);
        Maker {
            states: States::with_capacity(states, bytes),
            w: Work::with_capacity(points, column, states, bytes),
            kid: Section::with_capacity(dh::COLUMNS, points, column),
            quad: Section::with_capacity(32 * 32, quad_points(p), column),
            out: Vec::with_capacity(p.quadrant as usize),
        }
    }

    /// The bytes it takes, of the params.
    fn memory(p: &Params, block: impl Fn(u64) -> u64 + Copy) -> u64 {
        let (points, column, states) = (p.points as usize, p.column as usize, p.states as usize);
        states_memory(p, block)
            + Work::memory(points, column, states, p.state_bytes as usize, block)
            + grid(dh::COLUMNS as u64, points as u64, block)
            + grid(32 * 32, quad_points(p) as u64, block)
            + block(p.quadrant)
    }

    /// The quadrant's bytes of the child of `blobs` into `out`, of at most `limit` bytes.
    pub fn make(&mut self, blobs: &[&[u8]; 8], limit: u64) -> Result<()> {
        self.states.clear();
        if dh::mapping(blobs[7])?.len() > self.states.at.capacity() {
            return Err(corrupt());
        }
        dh::section(blobs, &mut self.states, &mut self.w, &mut self.kid)?;
        dh::quadrant(&self.kid, &self.states, &mut self.w, &mut self.quad)?;
        self.out.clear();
        if !dh::quadrant_bytes(&self.quad, &self.states, &mut self.w, &mut self.out) || self.out.len() as u64 > limit {
            return Err(corrupt());
        }
        Ok(())
    }
}

/// The workers making children's quadrants, natively: jobs sent of the rows' blobs, their bytes
/// (or failures) by row.
#[cfg(not(target_arch = "wasm32"))]
struct Jobs {
    tx: std::sync::mpsc::Sender<(usize, Vec<u8>, [u32; 8])>,
    done: Done,
}

/// The quadrants the workers made (or their failures), by row, and what tells of them.
#[cfg(not(target_arch = "wasm32"))]
type Done = std::sync::Arc<(std::sync::Mutex<std::collections::HashMap<usize, Result<Vec<u8>>>>, std::sync::Condvar)>;

fn grid(cols: u64, pts: u64, block: impl Fn(u64) -> u64) -> u64 {
    block(4 * cols) + block(2 * cols) + block(16 * pts) + 2 * block(cols)
}

fn states_memory(p: &Params, block: impl Fn(u64) -> u64) -> u64 {
    let states = p.states;
    block(p.state_bytes) + 2 * block(states) + block(8 * states) + block(4 * States::slots(states as usize) as u64)
}

/// What the module holds, of the params.
pub struct Lods {
    pub p: Params,
    rows: Vec<Row>,
    pool: Pool,
    mem: Vec<u8>,
    states: States,
    w: Work,
    kid: Section,
    quads: [Section; 4],
    parent: Section,
    blobs: [Vec<u8>; 8],
    quad: Vec<u8>,
    maker: Maker,
    #[cfg(not(target_arch = "wasm32"))]
    jobs: Option<Jobs>,
}

/// The plan of the module ([`aqr_api::stack`]) beyond its initial memory, of its params: the
/// readers' pieces, the states, the rows, the pool, and the buffers of a parent's making.
pub fn plan(initial: u64, params: &[u8], states_len: u64) -> Option<u64> {
    use aqr_api::stack::{Sim, block};
    let p = Params::parse(params).ok()?;
    let mut m = Sim::new(initial);
    m.take(block(64));
    m.take(block(SMALL_PIECE as u64));
    m.take(block(BIG_PIECE as u64));
    m.take(block(states_len));
    m.take(memory(&p, block));
    Some(m.plan())
}

/// The bytes that [`Lods::new`] takes.
pub fn memory(p: &Params, block: impl Fn(u64) -> u64 + Copy) -> u64 {
    let (points, column, states) = (p.points as usize, p.column as usize, p.states as usize);
    block(core::mem::size_of::<Row>() as u64 * p.rows)
        + block(16 * spans(p))
        + block(p.pool)
        + states_memory(p, block)
        + Work::memory(points, column, states, p.state_bytes as usize, block)
        + grid(dh::COLUMNS as u64, points as u64, block)
        + 4 * grid(32 * 32, quad_points(p) as u64, block)
        + grid(dh::COLUMNS as u64, points as u64, block)
        + 5 * block(p.blob_max() as u64)
        + 2 * block(dh::COLUMNS as u64)
        + block(p.mapping_max() as u64)
        + block(p.quadrant)
        + Maker::memory(p, block)
}

/// Points of a quadrant's grid: those of a quadrant (fewer than its bytes), and room for a merge
/// more ([`dh::quadrant`] asks for it before each).
fn quad_points(p: &Params) -> usize {
    p.quadrant as usize + 8 * p.column as usize
}

/// Free spans of the pool at most: one more than the blocks it holds, a row's eight blobs and its
/// quadrant at most.
fn spans(p: &Params) -> u64 {
    9 * p.rows + 1
}

impl Lods {
    pub fn new(p: Params) -> Lods {
        let (points, column) = (p.points as usize, p.column as usize);
        let qp = quad_points(&p);
        let mut rows = Vec::with_capacity(p.rows as usize);
        rows.resize(p.rows as usize, Row::default());
        let blob = p.blob_max();
        Lods {
            p,
            rows,
            pool: Pool::with_spans(p.pool, spans(&p) as usize),
            mem: alloc_zeroed(p.pool as usize),
            states: States::with_capacity(p.states as usize, p.state_bytes as usize),
            w: Work::with_capacity(points, column, p.states as usize, p.state_bytes as usize),
            kid: Section::with_capacity(dh::COLUMNS, points, column),
            quads: std::array::from_fn(|_| Section::with_capacity(32 * 32, qp, column)),
            parent: Section::with_capacity(dh::COLUMNS, points, column),
            blobs: [
                Vec::with_capacity(blob),
                Vec::with_capacity(blob),
                Vec::with_capacity(blob),
                Vec::with_capacity(blob),
                Vec::with_capacity(blob),
                Vec::with_capacity(dh::COLUMNS),
                Vec::with_capacity(dh::COLUMNS),
                Vec::with_capacity(p.mapping_max()),
            ],
            quad: Vec::with_capacity(p.quadrant as usize),
            maker: Maker::new(&p),
            #[cfg(not(target_arch = "wasm32"))]
            jobs: None,
        }
    }

    /// The opaque states (a mapping's list) taken first.
    pub fn opaque(&mut self, list: &[u8]) -> Result<()> {
        for r in dh::mapping(list)? {
            if self.states.len() as u64 >= self.p.states {
                return Err(corrupt());
            }
            let id = self.states.id(&list[r]);
            self.states.opaque[id as usize] = true;
        }
        Ok(())
    }

    fn take(&mut self, n: usize) -> Result<u64> {
        self.pool.take(n as u64).ok_or_else(corrupt)
    }

    fn held(&self, b: (u64, u32)) -> &[u8] {
        &self.mem[b.0 as usize..b.0 as usize + b.1 as usize]
    }

    /// Row `r`'s blobs all come: its quadrant (of `len` bytes) kept, made here or by a worker, and
    /// its blobs given back.
    fn child_done(&mut self, r: usize, len: u64) -> Result<()> {
        let row = self.rows[r];
        if len > self.p.quadrant {
            return Err(corrupt());
        }
        let at = self.take(len as usize)?;
        let mem = &self.mem;
        let b: [&[u8]; 8] = std::array::from_fn(|k| &mem[row.blobs[k].0 as usize..][..row.blobs[k].1 as usize]);
        #[cfg(not(target_arch = "wasm32"))]
        let sent = match &self.jobs {
            Some(j) => {
                let lens = row.blobs.map(|b| b.1);
                let data: Vec<u8> = b.iter().flat_map(|x| x.iter().copied()).collect();
                j.tx.send((r, data, lens)).map_err(|_| corrupt())?;
                true
            }
            None => false,
        };
        #[cfg(target_arch = "wasm32")]
        let sent = false;
        if !sent {
            self.maker.make(&b, self.p.quadrant)?;
            if self.maker.out.len() as u64 != len {
                return Err(corrupt());
            }
            self.mem[at as usize..][..len as usize].copy_from_slice(&self.maker.out);
        }
        for k in 0..8 {
            self.pool.give(row.blobs[k].0, row.blobs[k].1 as u64);
        }
        let r = &mut self.rows[r];
        r.child = 0;
        r.quadrant = Some((at, len as u32));
        r.pending = sent;
        Ok(())
    }

    /// Row `r`'s quadrant, made by a worker, waited for and put in its block.
    #[cfg(not(target_arch = "wasm32"))]
    fn wait(&mut self, r: usize) -> Result<()> {
        if !self.rows[r].pending {
            return Ok(());
        }
        let Some(j) = &self.jobs else { return Err(corrupt()) };
        let (lock, cv) = &*j.done;
        let mut done = lock.lock().unwrap();
        let bytes = loop {
            if let Some(b) = done.remove(&r) {
                break b?;
            }
            done = cv.wait(done).unwrap();
        };
        drop(done);
        let (at, len) = self.rows[r].quadrant.ok_or_else(corrupt)?;
        if bytes.len() != len as usize {
            return Err(corrupt());
        }
        self.mem[at as usize..][..len as usize].copy_from_slice(&bytes);
        self.rows[r].pending = false;
        Ok(())
    }

    #[cfg(target_arch = "wasm32")]
    fn wait(&mut self, _: usize) -> Result<()> {
        Ok(())
    }

    /// The quadrant in `quads[0]` kept as row `r`'s.
    fn hold_quadrant(&mut self, r: usize) -> Result<()> {
        self.quad.clear();
        if self.rows[r].quadrant.is_some()
            || !dh::quadrant_bytes(&self.quads[0], &self.states, &mut self.w, &mut self.quad)
        {
            return Err(corrupt());
        }
        if self.quad.len() as u64 > self.p.quadrant {
            return Err(corrupt());
        }
        let at = self.take(self.quad.len())?;
        self.mem[at as usize..][..self.quad.len()].copy_from_slice(&self.quad);
        self.rows[r].quadrant = Some((at, self.quad.len() as u32));
        Ok(())
    }

    /// A quadrant's bytes parsed into `quads[q]` (their states taken).
    fn quadrant_into(&mut self, q: usize, b: Option<(u64, u32)>) -> Result<()> {
        let n = self.states.len();
        let bytes = match b {
            Some(b) => &self.mem[b.0 as usize..][..b.1 as usize],
            None => &self.quad[..],
        };
        // (its new states counted first: past the params, corrupt)
        let m = {
            let mut p = 0;
            let len = aqr_common::varint::get(bytes, &mut p).map_err(|_| corrupt())? as usize;
            bytes.get(p..p.checked_add(len).ok_or_else(corrupt)?).ok_or_else(corrupt)?
        };
        let new = dh::mapping(m)?.into_iter().filter(|r| self.states.find(&m[r.clone()]).is_none()).count();
        if (n + new) as u64 > self.p.states {
            return Err(corrupt());
        }
        let (states, w, out) = (&mut self.states, &mut self.w, &mut self.quads[q]);
        dh::quadrant_of(bytes, states, w, out)
    }

    /// Row `r` made, a parent, of its quadrants' `sources` (0 stored, read of `stored`; else the
    /// child's row plus one): its blobs kept, its own quadrant too where `child`.
    fn derive(&mut self, r: usize, sources: [u64; 4], child: bool, stored: &mut Reader) -> Result<()> {
        for (q, &s) in sources.iter().enumerate() {
            match s {
                0 => {
                    let n = stored.num()?;
                    if n > self.p.quadrant {
                        return Err(corrupt());
                    }
                    self.quad.clear();
                    let quad = &mut self.quad;
                    stored
                        .each(n, |d| {
                            quad.extend_from_slice(d);
                            Some(())
                        })
                        .ok_or_else(corrupt)?;
                    self.quadrant_into(q, None)?;
                }
                c => {
                    let c = usize::try_from(c - 1).ok().filter(|&c| c < self.rows.len()).ok_or_else(corrupt)?;
                    self.wait(c)?;
                    let b = self.rows[c].quadrant.take().ok_or_else(corrupt)?;
                    self.quadrant_into(q, Some(b))?;
                    self.pool.give(b.0, b.1 as u64);
                }
            }
        }
        let [q0, q1, q2, q3] = &self.quads;
        dh::parent([[q0, q1], [q2, q3]], &self.states, &mut self.parent)?;
        if !dh::blobs(&self.parent, &self.states, &mut self.w, &mut self.blobs) {
            return Err(corrupt());
        }
        for k in 0..8 {
            let n = self.blobs[k].len();
            let at = self.take(n)?;
            self.mem[at as usize..][..n].copy_from_slice(&self.blobs[k]);
            self.rows[r].blobs[k] = (at, n as u32);
        }
        self.rows[r].parent = 0xFF;
        if child {
            std::mem::swap(&mut self.kid, &mut self.parent);
            dh::quadrant(&self.kid, &self.states, &mut self.w, &mut self.quads[0])?;
            self.hold_quadrant(r)?;
        }
        Ok(())
    }

    /// Whether everything held was given back.
    fn done(&self) -> bool {
        self.rows.iter().all(|r| r.child == 0 && r.parent == 0 && r.quadrant.is_none())
    }
}

fn alloc_zeroed(n: usize) -> Vec<u8> {
    vec![0; n]
}

/// The contents (`size` bytes in all) made of the steps, the stored bytes and the opaque states
/// (`states`, whole), put to `out`: natively the children's quadrants made on up to `threads`
/// threads more.
#[allow(clippy::too_many_arguments)]
pub fn build(
    p: Params,
    steps: &mut Reader,
    stored: &mut Reader,
    states: &[u8],
    out: &mut dyn Put,
    size: u64,
    threads: usize,
) -> Result<()> {
    let mut l = Lods::new(p);
    l.opaque(states)?;
    #[cfg(not(target_arch = "wasm32"))]
    if threads > 1 {
        use std::sync::{Arc, Condvar, Mutex, mpsc};
        let (tx, rx) = mpsc::channel::<(usize, Vec<u8>, [u32; 8])>();
        let rx = Arc::new(Mutex::new(rx));
        let done = Arc::new((Mutex::new(std::collections::HashMap::new()), Condvar::new()));
        l.jobs = Some(Jobs { tx, done: done.clone() });
        return std::thread::scope(|s| {
            for _ in 0..threads {
                let (rx, done) = (rx.clone(), done.clone());
                s.spawn(move || {
                    let mut m = Maker::new(&p);
                    loop {
                        let job = rx.lock().unwrap().recv();
                        let Ok((r, data, lens)) = job else { return };
                        let mut at = 0;
                        let b: [&[u8]; 8] = std::array::from_fn(|k| {
                            let x = &data[at..at + lens[k] as usize];
                            at += lens[k] as usize;
                            x
                        });
                        let res = m.make(&b, p.quadrant).map(|_| m.out.clone());
                        let (lock, cv) = &*done;
                        lock.lock().unwrap().insert(r, res);
                        cv.notify_all();
                    }
                });
            }
            let r = steps_of(&mut l, steps, stored, out, size);
            l.jobs = None;
            r
        });
    }
    let _ = threads;
    let r = steps_of(&mut l, steps, stored, out, size);
    // (in WebAssembly not given back: its buffers would be freed in the order they were made,
    // each a hole of the stack until the last, more than it takes; the instance ends with it)
    #[cfg(target_arch = "wasm32")]
    core::mem::forget(l);
    r
}

/// The steps taken, of `l`.
fn steps_of(l: &mut Lods, steps: &mut Reader, stored: &mut Reader, out: &mut dyn Put, size: u64) -> Result<()> {
    let mut done = 0u64;
    let row_of = |l: &Lods, v: u64| usize::try_from(v).ok().filter(|&r| r < l.rows.len()).ok_or_else(corrupt);
    while !steps.done() {
        let tag = steps.num()?;
        let n = tag >> SHIFT;
        match tag & ((1 << SHIFT) - 1) {
            PLAIN => {
                stored.each(n, |d| out.put(d)).ok_or_else(corrupt)?;
                done += n;
            }
            OBSERVED => {
                let r = row_of(l, steps.num()?)?;
                let k = steps.num()? as usize;
                if k >= 8 || l.rows[r].child & 1 << k != 0 || l.rows[r].quadrant.is_some() {
                    return Err(corrupt());
                }
                let n = usize::try_from(n).map_err(|_| corrupt())?;
                let at = l.take(n)?;
                let mem = &mut l.mem[at as usize..][..n];
                let mut i = 0;
                stored
                    .each(n as u64, |d| {
                        mem[i..i + d.len()].copy_from_slice(d);
                        i += d.len();
                        out.put(d)
                    })
                    .ok_or_else(corrupt)?;
                done += n as u64;
                l.rows[r].blobs[k] = (at, n as u32);
                l.rows[r].child |= 1 << k;
                if l.rows[r].child == 0xFF {
                    let len = steps.num()?;
                    l.child_done(r, len)?;
                }
            }
            DERIVED => {
                let r = row_of(l, steps.num()?)?;
                let k = steps.num()? as usize;
                if k >= 8 || n != 0 {
                    return Err(corrupt());
                }
                if l.rows[r].parent == 0 {
                    if l.rows[r].child != 0 {
                        return Err(corrupt());
                    }
                    let mut sources = [0u64; 4];
                    for s in &mut sources {
                        *s = steps.num()?;
                    }
                    let child = match steps.num()? {
                        0 => false,
                        1 => true,
                        _ => return Err(corrupt()),
                    };
                    l.derive(r, sources, child, stored)?;
                }
                if l.rows[r].parent & 1 << k == 0 {
                    return Err(corrupt());
                }
                let b = l.rows[r].blobs[k];
                out.put(l.held(b)).ok_or_else(corrupt)?;
                done += b.1 as u64;
                l.pool.give(b.0, b.1 as u64);
                l.rows[r].parent &= !(1 << k);
            }
            _ => return Err(corrupt()),
        }
    }
    let all = l.done() && stored.done() && done == size;
    if all { Ok(()) } else { Err(corrupt()) }
}

/// The threads to make quadrants on, besides: natively, as many as there are; in WebAssembly none.
fn threads() -> usize {
    #[cfg(target_arch = "wasm32")]
    return 1;
    #[cfg(not(target_arch = "wasm32"))]
    return std::thread::available_parallelism().map_or(1, |n| n.get());
}

/// The module natively: the contents of `len` bytes made of `inputs` (the steps, the stored
/// bytes, the opaque states).
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, threads: usize) -> Result<Vec<u8>> {
    use aqr_api::stream::Slice;
    let &[steps, stored, states] = inputs else { return Err(corrupt()) };
    let p = Params::parse(params)?;
    let (mut s, mut d) = (Slice(steps), Slice(stored));
    let mut s = Reader::new(&mut s, SMALL_PIECE);
    let mut d = Reader::new(&mut d, BIG_PIECE);
    let mut out = Vec::with_capacity(usize::try_from(len).map_err(|_| corrupt())?);
    build(p, &mut s, &mut d, states, &mut out, len, threads)?;
    Ok(out)
}

/// The module as a part module (its entry point, of `aqr_api::entry!`).
pub fn decode_part(params_len: u64, steps_len: u64, stored_len: u64, states_len: u64, size: u64) {
    aqr_api::finish(part(params_len, [steps_len, stored_len, states_len], size));
}

fn part(params_len: u64, lens: [u64; 3], size: u64) -> Result<()> {
    use aqr_api::Memory;
    use aqr_api::stream::{In, Pull, Written};
    if params_len > 64 || lens[2] > 1 << 26 {
        return Err(corrupt());
    }
    let mut params = vec![0; params_len as usize];
    aqr_api::Params::read(0, &mut params);
    let p = Params::parse(&params)?;
    let (mut s, mut d, mut o) = (In::<0>::new(lens[0]), In::<1>::new(lens[1]), In::<2>::new(lens[2]));
    let mut s = Reader::new(&mut s as &mut dyn Pull, SMALL_PIECE);
    let mut d = Reader::new(&mut d, BIG_PIECE);
    let mut states = vec![0; lens[2] as usize];
    let mut at = 0;
    while at < states.len() {
        match o.pull(&mut states[at..]) {
            0 => return Err(corrupt()),
            k => at += k,
        }
    }
    let mut out = Written::new(size);
    build(p, &mut s, &mut d, &states, &mut out, size, threads())?;
    if out.total() == size { Ok(()) } else { Err(corrupt()) }
}
