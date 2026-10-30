//! Modules run as WebAssembly in worker processes locked down by the system (with the feature
//! `sandbox`), so that a module escaping Wasmtime finds nothing it could reach: no files, no
//! network, no other processes (`os` for what each system does).
//!
//! A worker is a program of the host's: the host's own, invoked with arguments it recognizes,
//! or another one made for it, which calls [`worker_main`] (before anything else, as nothing
//! else of it may use its standard input and output). [`Sandbox`] is the program and its
//! arguments, and the workers it started, kept for the applications that follow. A worker is
//! never shared between sandboxes: a module escaping in one could change what the next
//! applications of the same sandbox give, never another's.
//!
//! [`Sandbox`] runs every module so ([`Wasm`](crate::Wasm) in a worker), and [`Sandboxed`]
//! those this crate does not know ([`Native`] in a worker). Each gives what the runner it stands
//! for gives, failures included, or else a resource failure where a worker could not be
//! started or ended unexpectedly.

mod os;
mod proto;
mod worker;

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use aqr_formula::Resource;
use aqr_formula::aqr::{self, Failure, Loaded, Runner};
use sha2::{Digest, Sha512_256};

use crate::Native;
use proto::*;

/// The bytes a worker takes beyond the memory limit of an application, as the system limits
/// it ([`os::Child::limit_memory`]): of its code, its stacks and the modules it compiled.
const OVERHEAD: u64 = 1 << 30;

/// The bytes of inputs and outputs from which an application has them in the worker's arena,
/// not in the messages.
const ARENA_FROM: u64 = 1 << 16;

/// The shortest arena made.
const ARENA_MIN: u64 = 1 << 20;

/// Whether this system locks workers down; where it does not, no worker starts (a resource
/// failure of any module run in one).
pub const SUPPORTED: bool = cfg!(any(windows, target_os = "linux", target_os = "macos"));

/// Serves the decoder that started this process as its worker, then exits: the whole of the
/// worker program, or the part of the host's that its arguments for workers lead to.
pub fn worker_main() -> ! {
    std::process::exit(match worker::serve() {
        Ok(()) => 0,
        Err(_) => 1,
    })
}

/// The program of the workers and its arguments, and the workers started, idle.
pub struct Sandbox {
    program: PathBuf,
    args: Vec<OsString>,
    /// The threads of each worker; those of aqr-common's pool where `None`.
    threads: Option<usize>,
    idle: Mutex<Vec<Worker>>,
    /// The Wasm modules a worker loaded, by their hashes: valid, and not loaded again for
    /// [`Runner::load`].
    valid: Mutex<HashSet<[u8; 32]>>,
}

impl Sandbox {
    /// Workers of `program` invoked with `args`, each with as many threads as aqr-common's pool
    /// has.
    pub fn new(program: impl Into<PathBuf>, args: impl IntoIterator<Item = impl Into<OsString>>) -> Sandbox {
        Sandbox {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
            threads: None,
            idle: Mutex::default(),
            valid: Mutex::default(),
        }
    }

    /// Workers of this process's own program, invoked with `args`.
    pub fn current_exe(args: impl IntoIterator<Item = impl Into<OsString>>) -> io::Result<Sandbox> {
        Ok(Sandbox::new(std::env::current_exe()?, args))
    }

    /// Each worker with `threads` threads (at least one), for the tasks of the modules.
    pub fn threads(mut self, threads: usize) -> Sandbox {
        self.threads = Some(threads.max(1));
        self
    }

    /// `f` on an idle worker, or one started; the worker kept unless it failed.
    fn with_worker<T>(&self, f: impl FnOnce(&mut Worker) -> io::Result<aqr::Decoded<T>>) -> aqr::Decoded<T> {
        let idle = self.idle.lock().unwrap().pop();
        let mut w = match idle {
            Some(w) => w,
            None => Worker::start(self).map_err(|e| Failure::Resource(format!("a sandbox worker not started: {e}")))?,
        };
        match f(&mut w) {
            Ok(r) => {
                self.idle.lock().unwrap().push(w);
                r
            }
            Err(e) => Err(Failure::Resource(format!("a sandbox worker ended: {}", w.ended(e)))),
        }
    }

    /// What a worker found it could do, for the tests: [`worker::probe`] of `path` and `port`.
    #[doc(hidden)]
    pub fn probe(&self, path: &str, port: u16) -> aqr::Decoded<Vec<String>> {
        self.with_worker(|w| {
            put_u8(&mut w.to, PROBE)?;
            put_bytes(&mut w.to, path.as_bytes())?;
            put_u64(&mut w.to, port as u64)?;
            w.to.flush()?;
            if let Err(f) = get_result(&mut w.from)? {
                return Ok(Err(f));
            }
            let n = get_count(&mut w.from, 16)?;
            let tried = (0..n).map(|_| get_bytes(&mut w.from, MAX_MESSAGE)).collect::<io::Result<Vec<_>>>()?;
            Ok(Ok(tried.into_iter().map(|t| String::from_utf8_lossy(&t).into_owned()).collect()))
        })
    }
}

/// Every module run as WebAssembly in a worker, whether known or not, as [`Wasm`](crate::Wasm)
/// runs it.
impl Runner for Sandbox {
    fn load(&self, wasm: &[u8]) -> aqr::Decoded<Box<dyn Loaded + '_>> {
        let hash: [u8; 32] = Sha512_256::digest(wasm).into();
        let wasm: Arc<[u8]> = wasm.into();
        if !self.valid.lock().unwrap().contains(&hash) {
            self.with_worker(|w| w.load(hash, &wasm))?;
            self.valid.lock().unwrap().insert(hash);
        }
        Ok(Box::new(Remote { sandbox: self, hash, wasm }))
    }
}

/// A module loaded, applied in whichever worker is idle.
struct Remote<'a> {
    sandbox: &'a Sandbox,
    hash: [u8; 32],
    wasm: Arc<[u8]>,
}

impl Loaded for Remote<'_> {
    fn apply(
        &self,
        params: &[u8],
        resources: &[Resource],
        inputs: &[&[u8]],
        sizes: &[u64],
        memory_limit: u64,
    ) -> aqr::Decoded<Vec<Vec<u8>>> {
        self.sandbox.with_worker(|w| {
            if !w.loaded.contains(&self.hash)
                && let Err(f) = w.load(self.hash, &self.wasm)?
            {
                return Ok(Err(f));
            }
            w.apply(self.hash, params, resources, inputs, sizes, memory_limit)
        })
    }
}

/// The modules of this crate natively where an archive has the Wasm module that aqr-modules
/// built, as [`Native`] runs them, and any other module in a worker of `sandbox`.
#[derive(Clone, Copy)]
pub struct Sandboxed<'a> {
    pub native: Native,
    pub sandbox: &'a Sandbox,
}

impl Runner for Sandboxed<'_> {
    fn load(&self, wasm: &[u8]) -> aqr::Decoded<Box<dyn Loaded + '_>> {
        match crate::known(wasm) {
            Some(_) => self.native.load(wasm),
            None => self.sandbox.load(wasm),
        }
    }
}

/// A worker started, its input and output, the modules it loaded and the arena it was given
/// last.
struct Worker {
    child: os::Child,
    to: BufWriter<File>,
    from: BufReader<File>,
    loaded: HashSet<[u8; 32]>,
    arena: Option<os::Arena>,
}

impl Worker {
    fn start(s: &Sandbox) -> io::Result<Worker> {
        let (child, to, from) = os::spawn(&s.program, &s.args)?;
        let mut w = Worker {
            child,
            to: BufWriter::with_capacity(1 << 16, to),
            from: BufReader::with_capacity(1 << 16, from),
            loaded: HashSet::new(),
            arena: None,
        };
        w.to.write_all(HELLO)?;
        put_u64(&mut w.to, s.threads.unwrap_or_else(aqr_common::pool::size).max(1) as u64)?;
        w.to.flush()?;
        let mut ready = [0; 8];
        w.from.read_exact(&mut ready).map_err(|e| w.ended(e))?;
        if ready != *READY {
            return Err(violation("not a worker"));
        }
        Ok(w)
    }

    /// Why the worker ended, as it is found to have.
    fn ended(&mut self, e: io::Error) -> io::Error {
        match self.child.exit_code() {
            Some(c) => io::Error::other(format!("{e} (exit code {c:#x})")),
            None => e,
        }
    }

    fn load(&mut self, hash: [u8; 32], wasm: &[u8]) -> io::Result<aqr::Decoded<()>> {
        self.child.limit_memory(OVERHEAD.saturating_add(4 * wasm.len() as u64))?;
        put_u8(&mut self.to, LOAD)?;
        self.to.write_all(&hash)?;
        put_bytes(&mut self.to, wasm)?;
        self.to.flush()?;
        let r = get_result(&mut self.from)?;
        if r.is_ok() {
            self.loaded.insert(hash);
        }
        Ok(r)
    }

    fn apply(
        &mut self,
        hash: [u8; 32],
        params: &[u8],
        resources: &[Resource],
        inputs: &[&[u8]],
        sizes: &[u64],
        memory_limit: u64,
    ) -> io::Result<aqr::Decoded<Vec<Vec<u8>>>> {
        // the bytes sent, held by the worker beside the memories it makes of them
        let sent = params.len() as u64
            + resources.iter().map(|r| r.len() as u64).sum::<u64>()
            + inputs.iter().map(|i| i.len() as u64).sum::<u64>();
        self.child.limit_memory(OVERHEAD.saturating_add(memory_limit).saturating_add(sent))?;
        // the inputs and outputs in the arena where they are many and fit in the memories (else
        // the application fails before any of them is made)
        let lens = inputs.iter().map(|i| i.len() as u64).chain(sizes.iter().copied());
        let placed = match layout(lens) {
            Some((places, len)) if (ARENA_FROM..=memory_limit).contains(&len) => {
                self.arena_for(len)?.map(|new| (places, new))
            }
            _ => None,
        };
        let w = &mut self.to;
        put_u8(w, APPLY)?;
        w.write_all(&hash)?;
        put_bytes(w, params)?;
        put_u64(w, resources.len() as u64)?;
        for r in resources {
            put_bytes(w, r)?;
        }
        match &placed {
            None => {
                put_u8(w, INLINE)?;
                put_u64(w, inputs.len() as u64)?;
                for i in inputs {
                    put_bytes(w, i)?;
                }
            }
            Some((places, new)) => {
                match new {
                    Some(token) => {
                        put_u8(w, NEW_ARENA)?;
                        put_u64(w, self.arena.as_ref().expect("the arena").map.len() as u64)?;
                        put_u64(w, *token)?;
                    }
                    None => put_u8(w, ARENA)?,
                }
                put_u64(w, inputs.len() as u64)?;
                let base = self.arena.as_ref().expect("the arena").map.as_ptr();
                for (i, p) in inputs.iter().zip(places) {
                    put_u64(w, i.len() as u64)?;
                    // SAFETY: the place is within the arena, as `arena_for` made it
                    unsafe { std::ptr::copy_nonoverlapping(i.as_ptr(), base.add(p.start as usize), i.len()) };
                }
            }
        }
        put_u64(w, sizes.len() as u64)?;
        for &s in sizes {
            put_u64(w, s)?;
        }
        put_u64(w, memory_limit)?;
        w.flush()?;
        let r = get_result(&mut self.from)?;
        let outs = match (&placed, r) {
            (_, Err(f)) => Err(f),
            (None, Ok(())) => self.outputs(sizes).map(Ok)?,
            (Some((places, _)), Ok(())) => {
                let base = self.arena.as_ref().expect("the arena").map.as_ptr();
                // read once each, as the worker may write them still
                Ok(places[inputs.len()..]
                    .iter()
                    .map(|p| {
                        let n = (p.end - p.start) as usize;
                        let mut o = Vec::with_capacity(n);
                        // SAFETY: the place is within the arena; the bytes copied are the
                        // vector's, of its capacity
                        unsafe {
                            std::ptr::copy_nonoverlapping(base.add(p.start as usize), o.as_mut_ptr(), n);
                            o.set_len(n);
                        }
                        o
                    })
                    .collect())
            }
        };
        // as the worker does
        if placed.is_some() && self.arena.as_ref().is_some_and(|a| a.map.len() as u64 > KEEP) {
            self.arena = None;
        }
        Ok(outs)
    }

    /// The outputs of the sizes declared, in the answer (as the worker checked them).
    fn outputs(&mut self, sizes: &[u64]) -> io::Result<Vec<Vec<u8>>> {
        let n = get_count(&mut self.from, sizes.len() as u64)?;
        if n != sizes.len() {
            return Err(violation(format!("{n} outputs of {}", sizes.len())));
        }
        let mut outs = Vec::with_capacity(n);
        for &s in sizes {
            let o = get_bytes(&mut self.from, s)?;
            if o.len() as u64 != s {
                return Err(violation(format!("an output of {} bytes of {s}", o.len())));
            }
            outs.push(o);
        }
        Ok(outs)
    }

    /// An arena of `len` bytes at least, the last one or a new one given to the worker (of the
    /// token to send it with); `None` where none can be made, so that the application is in
    /// the messages.
    fn arena_for(&mut self, len: u64) -> io::Result<Option<Option<u64>>> {
        if self.arena.as_ref().is_some_and(|a| a.map.len() as u64 >= len) {
            return Ok(Some(None));
        }
        // the last one unmapped first, so that both are never held
        self.arena = None;
        let size = if len <= KEEP { len.next_power_of_two().max(ARENA_MIN) } else { len };
        let Some(a) = usize::try_from(size).ok().and_then(|s| os::arena(s).ok()) else { return Ok(None) };
        let token = self.child.share(&a)?;
        self.arena = Some(a);
        Ok(Some(Some(token)))
    }
}
