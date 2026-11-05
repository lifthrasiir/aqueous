//! A native emulation of the host, following the spec's "apply a module".
//!
//! Every memory is created whether the module uses it or not, and the memory limit is not enforced.
//! A trap is a panic, which [`apply`] catches as a [`Failure`]; so are the violations of
//! the host functions, which the spec signals as errors to the invocation.
//! `aqr_log` messages go to stderr, and the last one to the [`Failure`].
//!
//! Tasks (`crate::task`) run on threads of their own from their spawn, with every memory but
//! the working one empty to them and the functions of part modules a violation; a task's trap is
//! resumed where it is joined. Channels (`crate::chan`) are queues between them, whose ends
//! are threads; there is no telling a deadlock, but for a trap after 30 s of waiting.

use std::cell::RefCell;
use std::panic::{self, AssertUnwindSafe};
use std::sync::Once;

/// Whole or part module, after the exported entry point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Whole,
    Part,
}

/// An entry point, as defined by [`entry!`](crate::entry).
#[derive(Clone, Copy)]
pub struct Entry {
    pub kind: Kind,
    /// The windows of the memories of its inputs and outputs.
    pub windows: &'static [crate::Window],
    pub arity: usize,
    pub call: fn(&[u64]),
}

/// An invalid archive failure, with what went wrong and the last `aqr_log` message before it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub what: String,
    pub log: Option<Vec<u8>>,
}

fn failure(what: String) -> Failure {
    Failure { what, log: None }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemId {
    Params,
    Output(u32),
    Input(u32),
    Resource(u32),
}

/// The payload of a panic signalling an error to the invocation.
struct Trap(String);

/// An input of a part module's application streamed ([`apply_streaming`]): pulled as the
/// module requests it.
pub trait Feed {
    fn len(&self) -> u64;

    /// Its bytes from where they were pulled to before up to `hi`, given to `put` in order.
    /// An error fails the application (as a trap, with its message).
    fn pull(&mut self, hi: u64, put: &mut dyn FnMut(&[u8])) -> Result<(), String>;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// An output of a part module's application streamed ([`apply_streaming`]): taking its bytes
/// as the module commits them.
pub trait Drain {
    fn push(&mut self, d: &[u8]) -> Result<(), String>;
}

/// The data of an input: of its own, or pulled.
enum In {
    Data(Vec<u8>),
    Pull(&'static mut dyn Feed),
}

impl In {
    fn len(&self) -> u64 {
        match self {
            In::Data(d) => d.len() as u64,
            In::Pull(f) => f.len(),
        }
    }
}

/// The bytes of an output committed: kept, or pushed (so many of them).
enum Committed {
    Kept(Vec<u8>),
    Pushed(&'static mut dyn Drain, u64),
}

impl Committed {
    fn len(&self) -> u64 {
        match self {
            Committed::Kept(d) => d.len() as u64,
            Committed::Pushed(_, n) => *n,
        }
    }
}

struct Host {
    kind: Kind,
    params: Vec<u8>,
    resources: Vec<Vec<u8>>,
    inputs: Vec<Vec<u8>>,
    outputs: Vec<Vec<u8>>,
    /// The data of the inputs and their visible ranges, and the outputs so far, for a part
    /// module.
    data: Vec<In>,
    visible: Vec<(u64, u64)>,
    committed: Vec<Committed>,
    sizes: Vec<u64>,
    last_log: Option<Vec<u8>>,
    /// How deep within tasks the code runs, and the empty memory they see.
    task: u32,
    empty: Vec<u8>,
}

thread_local! {
    static HOST: RefCell<Option<Host>> = const { RefCell::new(None) };
}

fn sized_to_fit(data: &[u8]) -> Vec<u8> {
    let mut mem = data.to_vec();
    mem.resize(data.len().div_ceil(65536) * 65536, 0);
    mem
}

/// A memory of zeros sized to fit `len` bytes, within `window` pages where given.
fn zeros(len: usize, window: Option<u64>) -> Vec<u8> {
    let pages = (len.div_ceil(65536) as u64).min(window.unwrap_or(u64::MAX));
    vec![0; pages as usize * 65536]
}

/// Applies the module `entry` to `inputs`, `resources` and `params` with the declared output sizes
/// `sizes`; returns the outputs.
pub fn apply(
    entry: &Entry,
    inputs: &[&[u8]],
    resources: &[&[u8]],
    params: &[u8],
    sizes: &[u64],
) -> Result<Vec<Vec<u8>>, Failure> {
    let data = inputs.iter().map(|i| In::Data(i.to_vec())).collect();
    let committed = sizes.iter().map(|_| Committed::Kept(Vec::new())).collect();
    let host = run(entry, data, committed, resources, params, sizes)?;
    let outputs: Vec<Vec<u8>> = match entry.kind {
        Kind::Whole => host
            .outputs
            .into_iter()
            .zip(sizes)
            .map(|(mut o, &s)| {
                o.truncate(s as usize);
                o
            })
            .collect(),
        Kind::Part => (host.committed.into_iter())
            .map(|c| match c {
                Committed::Kept(d) => d,
                Committed::Pushed(..) => unreachable!("kept"),
            })
            .collect(),
    };
    for (j, (o, &size)) in outputs.iter().zip(sizes).enumerate() {
        if o.len() as u64 != size {
            return Err(failure(format!("output {j} of {} bytes, not {size}", o.len())));
        }
    }
    Ok(outputs)
}

/// [`apply`] of a part module, its inputs pulled from `inputs` as it requests them and its
/// outputs pushed to `outputs` as it commits them, on the calling thread: as the spec's
/// "apply a module" allows a host to.
pub fn apply_streaming(
    entry: &Entry,
    inputs: &mut [&mut dyn Feed],
    resources: &[&[u8]],
    params: &[u8],
    outputs: &mut [&mut dyn Drain],
    sizes: &[u64],
) -> Result<(), Failure> {
    if entry.kind != Kind::Part {
        return Err(failure("a whole module streamed".into()));
    }
    if outputs.len() != sizes.len() {
        return Err(failure("outputs of other sizes than declared".into()));
    }
    // SAFETY: the inputs and outputs outlive the host, which `run` takes back before it returns
    let data = (inputs.iter_mut())
        .map(|f| In::Pull(unsafe { std::mem::transmute::<&mut dyn Feed, &'static mut dyn Feed>(&mut **f) }))
        .collect();
    let committed = (outputs.iter_mut())
        .map(|d| {
            Committed::Pushed(unsafe { std::mem::transmute::<&mut dyn Drain, &'static mut dyn Drain>(&mut **d) }, 0)
        })
        .collect();
    let host = run(entry, data, committed, resources, params, sizes)?;
    for (j, (o, &size)) in host.committed.iter().zip(sizes).enumerate() {
        if o.len() != size {
            return Err(failure(format!("output {j} of {} bytes, not {size}", o.len())));
        }
    }
    Ok(())
}

/// Runs `entry` on the host of inputs `data` and outputs `committed`: the host once it returns.
fn run(
    entry: &Entry,
    data: Vec<In>,
    committed: Vec<Committed>,
    resources: &[&[u8]],
    params: &[u8],
    sizes: &[u64],
) -> Result<Host, Failure> {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let prev = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if info.payload().downcast_ref::<Trap>().is_none() {
                prev(info)
            }
        }));
    });

    let whole = entry.kind == Kind::Whole;
    if sizes.is_empty() {
        return Err(failure("no output".into()));
    }
    let sizes_usize = sizes
        .iter()
        .map(|&s| usize::try_from(s).map_err(|_| failure("declared output size too large".into())))
        .collect::<Result<Vec<_>, _>>()?;
    let lens: Vec<u64> = data.iter().map(In::len).collect();
    let host = Host {
        kind: entry.kind,
        params: sized_to_fit(params),
        resources: resources.iter().map(|r| sized_to_fit(r)).collect(),
        inputs: (data.iter().enumerate())
            .map(|(k, i)| match (whole, i) {
                (true, In::Data(d)) => sized_to_fit(d),
                _ => zeros(i.len() as usize, crate::Window::of(entry.windows, false, k as u32)),
            })
            .collect(),
        outputs: (sizes_usize.iter().enumerate())
            .map(|(j, &s)| zeros(s, crate::Window::of(entry.windows, true, j as u32)))
            .collect(),
        data,
        visible: vec![(0, 0); lens.len()],
        committed,
        sizes: sizes.to_vec(),
        last_log: None,
        task: 0,
        empty: Vec::new(),
    };
    HOST.with_borrow_mut(|h| {
        assert!(h.is_none(), "nested emu::apply");
        *h = Some(host);
    });

    let mut args = vec![params.len() as u64];
    args.extend(resources.iter().map(|r| r.len() as u64));
    args.extend(&lens);
    args.extend(sizes);
    let result = if args.len() != entry.arity {
        Err(failure(format!("entry point takes {} arguments, not {}", entry.arity, args.len())))
    } else {
        panic::catch_unwind(AssertUnwindSafe(|| (entry.call)(&args))).map_err(|payload| {
            if let Some(Trap(msg)) = payload.downcast_ref() {
                failure(msg.clone())
            } else if let Some(msg) = payload.downcast_ref::<&str>() {
                failure(format!("trap: {msg}"))
            } else if let Some(msg) = payload.downcast_ref::<String>() {
                failure(format!("trap: {msg}"))
            } else {
                failure("trap".into())
            }
        })
    };
    let mut host = HOST.with_borrow_mut(|h| h.take()).unwrap();
    result.map_err(|f| Failure { log: host.last_log.take(), ..f })?;
    Ok(host)
}

fn trap(msg: String) -> ! {
    panic::panic_any(Trap(msg))
}

/// The host of a task of an application of `kind`.
#[cfg(feature = "alloc")]
fn task_host(kind: Kind) -> Host {
    Host {
        kind,
        params: Vec::new(),
        resources: Vec::new(),
        inputs: Vec::new(),
        outputs: Vec::new(),
        data: Vec::new(),
        visible: Vec::new(),
        committed: Vec::new(),
        sizes: Vec::new(),
        last_log: None,
        task: 1,
        empty: Vec::new(),
    }
}

/// A channel: its messages queued, and the threads at its ends as they first use them.
#[cfg(feature = "alloc")]
struct Chan {
    cap: usize,
    msgs: std::collections::VecDeque<Vec<u8>>,
    closed: bool,
    sender: Option<std::thread::ThreadId>,
    receiver: Option<std::thread::ThreadId>,
}

/// The channels of the process, and their changes.
#[cfg(feature = "alloc")]
static CHANS: std::sync::Mutex<Vec<Chan>> = std::sync::Mutex::new(Vec::new());
#[cfg(feature = "alloc")]
static CHANGED: std::sync::Condvar = std::sync::Condvar::new();

/// The channel `c`, with `f` (which may wait for a change of the channels and return `None`
/// to wait again).
#[cfg(feature = "alloc")]
fn with_chan<R>(c: u32, mut f: impl FnMut(&mut Chan) -> Result<Option<R>, String>) -> R {
    let mut chans = CHANS.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        let r = match chans.get_mut(c as usize) {
            Some(ch) => f(ch),
            None => Err(format!("channel {c} not made")),
        };
        match r {
            Ok(Some(r)) => {
                CHANGED.notify_all();
                return r;
            }
            Ok(None) => {
                let (g, t) =
                    CHANGED.wait_timeout(chans, std::time::Duration::from_secs(30)).unwrap_or_else(|e| e.into_inner());
                if t.timed_out() {
                    drop(g);
                    trap(format!("channel {c} waited for 30 s: a deadlock?"));
                }
                chans = g;
            }
            Err(msg) => {
                drop(chans);
                trap(msg)
            }
        }
    }
}

/// Takes the end `end` of a channel for the calling thread.
#[cfg(feature = "alloc")]
fn end_of(end: &mut Option<std::thread::ThreadId>, what: &str) -> Result<(), String> {
    let me = std::thread::current().id();
    match *end.get_or_insert(me) == me {
        true => Ok(()),
        false => Err(format!("{what} by two instances")),
    }
}

fn with<R>(f: impl FnOnce(&mut Host) -> Result<R, String>) -> R {
    let r = HOST.with_borrow_mut(|h| f(h.as_mut().expect("aqr_api used outside emu::apply")));
    r.unwrap_or_else(|msg| trap(msg))
}

impl Host {
    fn mem(&mut self, id: MemId) -> Result<&mut Vec<u8>, String> {
        if self.task > 0 {
            self.empty.clear();
            return Ok(&mut self.empty);
        }
        match id {
            MemId::Params => Ok(&mut self.params),
            MemId::Output(j) => self.outputs.get_mut(j as usize).ok_or_else(|| format!("aqr_output{j} imported")),
            MemId::Input(k) => self.inputs.get_mut(k as usize).ok_or_else(|| format!("aqr_input{k} imported")),
            MemId::Resource(i) => self.resources.get_mut(i as usize).ok_or_else(|| format!("aqr_resource{i} imported")),
        }
    }

    fn part(&self, func: &str) -> Result<(), String> {
        if self.kind == Kind::Whole {
            Err(format!("whole module imports {func}"))
        } else if self.task > 0 {
            Err(format!("{func} in a task"))
        } else {
            Ok(())
        }
    }
}

fn range(size: usize, at: u64, len: u64) -> Result<std::ops::Range<usize>, String> {
    match at.checked_add(len) {
        Some(end) if end <= size as u64 => Ok(at as usize..end as usize),
        _ => Err(format!("out of bounds memory access at {at}+{len}")),
    }
}

pub(crate) mod sys {
    use super::{Committed, In, MemId, range, with};

    pub fn size(id: MemId) -> u64 {
        with(|h| Ok(h.mem(id)?.len() as u64))
    }

    pub fn read(id: MemId, src: u64, dst: &mut [u8]) {
        with(|h| {
            let mem = h.mem(id)?;
            dst.copy_from_slice(&mem[range(mem.len(), src, dst.len() as u64)?]);
            Ok(())
        })
    }

    pub fn copy_to_output(j: u32, id: MemId, dst: u64, src: u64, len: u64) {
        with(|h| {
            let mem = h.mem(id)?;
            let bytes = mem[range(mem.len(), src, len)?].to_vec();
            let out = h.mem(MemId::Output(j))?;
            let r = range(out.len(), dst, len)?;
            out[r].copy_from_slice(&bytes);
            Ok(())
        })
    }

    pub fn write_output(j: u32, dst: u64, src: &[u8]) {
        with(|h| {
            let out = h.mem(MemId::Output(j))?;
            let r = range(out.len(), dst, src.len() as u64)?;
            out[r].copy_from_slice(src);
            Ok(())
        })
    }

    pub fn request(k: u32, hi: u64) {
        with(|h| {
            h.part("aqr_request")?;
            let w = h.mem(MemId::Input(k))?.len() as u64;
            let len = h.data[k as usize].len();
            let (lo_k, hi_k) = h.visible[k as usize];
            if !(hi_k..=len).contains(&hi) || hi - lo_k > w {
                return Err(format!("aqr_request({k}, {hi}) outside [{hi_k}, {len}] or window {w}"));
            }
            let mem = &mut h.inputs[k as usize];
            let mut at = hi_k;
            let mut put = |mut d: &[u8]| {
                while !d.is_empty() {
                    let from = (at % w) as usize;
                    let n = d.len().min(w as usize - from);
                    mem[from..from + n].copy_from_slice(&d[..n]);
                    (at, d) = (at + n as u64, &d[n..]);
                }
            };
            match &mut h.data[k as usize] {
                In::Data(d) => put(&d[hi_k as usize..hi as usize]),
                In::Pull(f) => f.pull(hi, &mut put)?,
            }
            h.visible[k as usize].1 = hi;
            Ok(())
        })
    }

    pub fn release(k: u32, lo: u64) {
        with(|h| {
            h.part("aqr_release")?;
            h.mem(MemId::Input(k))?;
            let (lo_k, hi_k) = h.visible[k as usize];
            if !(lo_k..=hi_k).contains(&lo) {
                return Err(format!("aqr_release({k}, {lo}) outside [{lo_k}, {hi_k}]"));
            }
            h.visible[k as usize].0 = lo;
            Ok(())
        })
    }

    pub fn commit(j: u32, hi: u64) {
        with(|h| {
            h.part("aqr_commit")?;
            let w = h.mem(MemId::Output(j))?.len() as u64;
            let (len, size) = (h.committed[j as usize].len(), h.sizes[j as usize]);
            if !(len..=size).contains(&hi) || hi - len > w {
                return Err(format!("aqr_commit({j}, {hi}) outside [{len}, {size}] or window {w}"));
            }
            let mem = &h.outputs[j as usize];
            let mut at = len;
            while at < hi {
                let from = (at % w) as usize;
                let n = (hi - at).min(w - from as u64) as usize;
                match &mut h.committed[j as usize] {
                    Committed::Kept(d) => d.extend_from_slice(&mem[from..from + n]),
                    Committed::Pushed(p, done) => {
                        p.push(&mem[from..from + n])?;
                        *done += n as u64;
                    }
                }
                at += n as u64;
            }
            Ok(())
        })
    }

    pub fn heartbeat() {}

    /// Starts the `n` tasks of `rec`, each on a thread of its own, within the application if
    /// any.
    ///
    /// # Safety
    /// What `rec` refers to outlives the threads (joined as `Tasks` are dropped).
    #[cfg(feature = "alloc")]
    pub unsafe fn spawn(rec: &crate::task::Record, n: usize) -> Vec<Option<std::thread::JoinHandle<Vec<u8>>>> {
        struct Send(crate::task::Record);
        // SAFETY: the closure `rec` refers to is `Sync`, and outlives the threads
        unsafe impl std::marker::Send for Send {}
        let kind = super::HOST.with_borrow(|h| h.as_ref().map(|h| h.kind));
        (0..n)
            .map(|i| {
                let rec = Send(rec.clone());
                Some(std::thread::spawn(move || {
                    let rec = rec;
                    super::HOST.with_borrow_mut(|h| *h = kind.map(super::task_host));
                    // (the channels it sent to closed as it ends, however it does)
                    struct End;
                    impl Drop for End {
                        fn drop(&mut self) {
                            let me = std::thread::current().id();
                            let mut chans = super::CHANS.lock().unwrap_or_else(|e| e.into_inner());
                            chans.iter_mut().filter(|c| c.sender == Some(me)).for_each(|c| c.closed = true);
                            super::CHANGED.notify_all();
                        }
                    }
                    let _end = End;
                    rec.0.call(i)
                }))
            })
            .collect()
    }

    #[cfg(feature = "alloc")]
    pub fn chan(cap: u32) -> u32 {
        let mut chans = super::CHANS.lock().unwrap_or_else(|e| e.into_inner());
        chans.push(super::Chan {
            cap: cap as usize,
            msgs: Default::default(),
            closed: false,
            sender: None,
            receiver: None,
        });
        (chans.len() - 1) as u32
    }

    #[cfg(feature = "alloc")]
    pub fn send(c: u32, msg: &[u8]) {
        let mut msg = Some(msg.to_vec());
        super::with_chan(c, |ch| {
            super::end_of(&mut ch.sender, "sending")?;
            if ch.closed {
                return Err(format!("aqr_send({c}) after closing"));
            }
            if ch.msgs.len() >= ch.cap {
                return Ok(None);
            }
            ch.msgs.push_back(msg.take().unwrap());
            Ok(Some(()))
        })
    }

    #[cfg(feature = "alloc")]
    pub fn recv(c: u32, dst: &mut [u8]) -> i32 {
        let msg = super::with_chan(c, |ch| {
            super::end_of(&mut ch.receiver, "receiving")?;
            Ok(match ch.msgs.front() {
                None if ch.closed => Some(Err(-1)),
                None => None,
                Some(m) if m.len() > dst.len() => Some(Err(m.len() as i32)),
                Some(_) => Some(Ok(ch.msgs.pop_front().unwrap())),
            })
        });
        match msg {
            Ok(m) => {
                dst[..m.len()].copy_from_slice(&m);
                m.len() as i32
            }
            Err(n) => n,
        }
    }

    #[cfg(feature = "alloc")]
    pub fn close(c: u32) {
        super::with_chan(c, |ch| {
            super::end_of(&mut ch.sender, "sending")?;
            ch.closed = true;
            Ok(Some(()))
        })
    }

    /// Also outside [`apply`](super::apply), where the code of a module runs as a library.
    pub fn log(msg: &[u8]) {
        eprintln!("aqr_log: {}", String::from_utf8_lossy(msg));
        super::HOST.with_borrow_mut(|h| {
            if let Some(h) = h {
                h.last_log = Some(msg.to_vec());
            }
        })
    }

    pub fn trap() -> ! {
        super::trap("unreachable".into())
    }

    macro_rules! memory {
        ($ty:ty, $name:literal, $id:expr) => {
            impl $crate::Memory for $ty {
                fn size() -> u64 {
                    $crate::emu::sys::size($id)
                }

                fn read(src: u64, dst: &mut [u8]) {
                    $crate::emu::sys::read($id, src, dst)
                }
            }
        };
    }
    pub(crate) use memory;

    macro_rules! output {
        ($j:literal) => {
            impl $crate::Writable for $crate::Output<$j> {
                fn write(dst: u64, src: &[u8]) {
                    $crate::emu::sys::write_output($j, dst, src)
                }
            }
        };
    }
    pub(crate) use output;

    macro_rules! copy {
        ($j:literal, $ty:ty, $name:literal, $id:expr) => {
            impl $crate::CopyTo<$j> for $ty {
                fn copy_to(dst: u64, src: u64, len: u64) {
                    $crate::emu::sys::copy_to_output($j, $id, dst, src, len)
                }
            }
        };
    }
    pub(crate) use copy;
}
