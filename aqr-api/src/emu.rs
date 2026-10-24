//! A native emulation of the host, following the spec's "apply a module".
//!
//! Every memory is created whether the module uses it or not, and the memory limit is not enforced.
//! A trap is a panic, which [`apply`] catches as a [`Failure`]; so are the violations of
//! the host functions, which the spec signals as errors to the invocation.
//! `aqr_log` messages go to stderr, and the last one to the [`Failure`].

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
    Output,
    Input(u32),
    Resource(u32),
}

/// The payload of a panic signalling an error to the invocation.
struct Trap(String);

struct Host {
    kind: Kind,
    params: Vec<u8>,
    resources: Vec<Vec<u8>>,
    inputs: Vec<Vec<u8>>,
    output: Vec<u8>,
    /// The data of the inputs and their visible ranges, for a part module.
    data: Vec<Vec<u8>>,
    visible: Vec<(u64, u64)>,
    committed: Vec<u8>,
    size: u64,
    last_log: Option<Vec<u8>>,
}

thread_local! {
    static HOST: RefCell<Option<Host>> = const { RefCell::new(None) };
}

fn sized_to_fit(data: &[u8]) -> Vec<u8> {
    let mut mem = data.to_vec();
    mem.resize(data.len().div_ceil(65536) * 65536, 0);
    mem
}

/// Applies the module `entry` to `inputs`, `resources` and `params` with the declared output size `size`.
pub fn apply(
    entry: &Entry,
    inputs: &[&[u8]],
    resources: &[&[u8]],
    params: &[u8],
    size: u64,
) -> Result<Vec<u8>, Failure> {
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
    let size_usize = usize::try_from(size).map_err(|_| failure("declared output size too large".into()))?;
    let host = Host {
        kind: entry.kind,
        params: sized_to_fit(params),
        resources: resources.iter().map(|r| sized_to_fit(r)).collect(),
        inputs: inputs.iter().map(|i| if whole { sized_to_fit(i) } else { sized_to_fit(&vec![0; i.len()]) }).collect(),
        output: sized_to_fit(&vec![0; size_usize]),
        data: inputs.iter().map(|i| i.to_vec()).collect(),
        visible: vec![(0, 0); inputs.len()],
        committed: Vec::new(),
        size,
        last_log: None,
    };
    HOST.with_borrow_mut(|h| {
        assert!(h.is_none(), "nested emu::apply");
        *h = Some(host);
    });

    let mut args = vec![params.len() as u64];
    args.extend(resources.iter().map(|r| r.len() as u64));
    args.extend(inputs.iter().map(|i| i.len() as u64));
    args.push(size);
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

    let output = if whole {
        host.output.truncate(size_usize);
        host.output
    } else {
        host.committed
    };
    if output.len() as u64 != size {
        return Err(failure(format!("output of {} bytes, not {size}", output.len())));
    }
    Ok(output)
}

fn trap(msg: String) -> ! {
    panic::panic_any(Trap(msg))
}

fn with<R>(f: impl FnOnce(&mut Host) -> Result<R, String>) -> R {
    let r = HOST.with_borrow_mut(|h| f(h.as_mut().expect("aqr_api used outside emu::apply")));
    r.unwrap_or_else(|msg| trap(msg))
}

impl Host {
    fn mem(&mut self, id: MemId) -> Result<&mut Vec<u8>, String> {
        match id {
            MemId::Params => Ok(&mut self.params),
            MemId::Output => Ok(&mut self.output),
            MemId::Input(k) => self.inputs.get_mut(k as usize).ok_or_else(|| format!("aqr_input{k} imported")),
            MemId::Resource(i) => self.resources.get_mut(i as usize).ok_or_else(|| format!("aqr_resource{i} imported")),
        }
    }

    fn part(&self, func: &str) -> Result<(), String> {
        if self.kind == Kind::Whole { Err(format!("whole module imports {func}")) } else { Ok(()) }
    }
}

fn range(size: usize, at: u64, len: u64) -> Result<std::ops::Range<usize>, String> {
    match at.checked_add(len) {
        Some(end) if end <= size as u64 => Ok(at as usize..end as usize),
        _ => Err(format!("out of bounds memory access at {at}+{len}")),
    }
}

pub(crate) mod sys {
    use super::{MemId, range, with};

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

    pub fn copy_to_output(id: MemId, dst: u64, src: u64, len: u64) {
        with(|h| {
            let mem = h.mem(id)?;
            let bytes = mem[range(mem.len(), src, len)?].to_vec();
            let out = &mut h.output;
            let r = range(out.len(), dst, len)?;
            out[r].copy_from_slice(&bytes);
            Ok(())
        })
    }

    pub fn write_output(dst: u64, src: &[u8]) {
        with(|h| {
            let out = &mut h.output;
            let r = range(out.len(), dst, src.len() as u64)?;
            out[r].copy_from_slice(src);
            Ok(())
        })
    }

    pub fn request(k: u32, hi: u64) {
        with(|h| {
            h.part("aqr_request")?;
            let w = h.mem(MemId::Input(k))?.len() as u64;
            let data = &h.data[k as usize];
            let (lo_k, hi_k) = h.visible[k as usize];
            if !(hi_k..=data.len() as u64).contains(&hi) || hi - lo_k > w {
                return Err(format!("aqr_request({k}, {hi}) outside [{hi_k}, {}] or window {w}", data.len()));
            }
            let mem = &mut h.inputs[k as usize];
            for o in hi_k..hi {
                mem[(o % w) as usize] = data[o as usize];
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

    pub fn commit(hi: u64) {
        with(|h| {
            h.part("aqr_commit")?;
            let w = h.output.len() as u64;
            let len = h.committed.len() as u64;
            if !(len..=h.size).contains(&hi) || hi - len > w {
                return Err(format!("aqr_commit({hi}) outside [{len}, {}] or window {w}", h.size));
            }
            for o in len..hi {
                h.committed.push(h.output[(o % w) as usize]);
            }
            Ok(())
        })
    }

    pub fn heartbeat() {}

    pub fn log(msg: &[u8]) {
        eprintln!("aqr_log: {}", String::from_utf8_lossy(msg));
        with(|h| {
            h.last_log = Some(msg.to_vec());
            Ok(())
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

                fn copy_to_output(dst: u64, src: u64, len: u64) {
                    $crate::emu::sys::copy_to_output($id, dst, src, len)
                }
            }
        };
    }
    pub(crate) use memory;
}
