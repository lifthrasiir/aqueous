//! A worker: the modules loaded and applied as the decoder asks, over its standard input and
//! output, locked down ([`os::lockdown`]) before it reads anything of an archive.

use std::collections::HashMap;
use std::io::{self, BufReader, BufWriter, Read, Write};

use aqr_formula::Resource;
use aqr_formula::aqr::{Failure, Loaded};

use super::os;
use super::proto::*;
use crate::wasm;

/// Serves the decoder until it closes the input.
pub fn serve() -> io::Result<()> {
    let (input, output) = os::channel()?;
    let mut r = BufReader::with_capacity(1 << 16, input);
    let mut w = BufWriter::with_capacity(1 << 16, output);
    let mut hello = [0; 8];
    r.read_exact(&mut hello)?;
    if hello != *HELLO {
        return Err(violation("not a decoder"));
    }
    let threads = get_u64(&mut r)? as usize;
    // what the worker would make or load later, made before it is locked down
    aqr_common::pool::init(threads);
    aqr_common::pool::size();
    wasm::warm_up();
    os::lockdown()?;
    w.write_all(READY)?;
    w.flush()?;
    // on the thread locked down last, the modules on the pool (of the stacks they would have)
    requests(&mut r, &mut w)
}

fn requests(r: &mut impl Read, w: &mut impl Write) -> io::Result<()> {
    let mut loaded: HashMap<[u8; 32], Box<dyn Loaded>> = HashMap::new();
    loop {
        let mut tag = [0];
        if r.read(&mut tag)? == 0 {
            return Ok(());
        }
        match tag[0] {
            LOAD => {
                let hash = get_hash(r)?;
                let bytes = get_bytes(r, u64::MAX)?;
                let l = aqr_common::pool::run(|| wasm::load(&bytes, hash)).map(|l| {
                    loaded.insert(hash, l);
                });
                put_result(w, &l)?;
            }
            APPLY => {
                let hash = get_hash(r)?;
                let params = get_bytes(r, u64::MAX)?;
                let n = get_count(r, u64::MAX)?;
                let resources =
                    (0..n).map(|_| get_bytes(r, u64::MAX).map(Resource::new)).collect::<io::Result<Vec<_>>>()?;
                let n = get_count(r, u64::MAX)?;
                let inputs = (0..n).map(|_| get_bytes(r, u64::MAX)).collect::<io::Result<Vec<_>>>()?;
                let n = get_count(r, u64::MAX)?;
                let sizes = (0..n).map(|_| get_u64(r)).collect::<io::Result<Vec<_>>>()?;
                let limit = get_u64(r)?;
                let inputs: Vec<&[u8]> = inputs.iter().map(|i| &i[..]).collect();
                let out = match loaded.get(&hash) {
                    Some(l) => aqr_common::pool::run(|| l.apply(&params, &resources, &inputs, &sizes, limit)),
                    None => Err(Failure::Resource("a module not loaded in the worker".into())),
                };
                match out {
                    Ok(outs) => {
                        put_result(w, &Ok(()))?;
                        put_u64(w, outs.len() as u64)?;
                        for o in &outs {
                            put_bytes(w, o)?;
                        }
                    }
                    Err(f) => put_result(w, &Err(f))?,
                }
            }
            PROBE => {
                let path = String::from_utf8_lossy(&get_bytes(r, u64::MAX)?).into_owned();
                let port = get_u64(r)? as u16;
                let tried = probe(&path, port);
                put_result(w, &Ok(()))?;
                put_u64(w, tried.len() as u64)?;
                for t in &tried {
                    put_bytes(w, t.as_bytes())?;
                }
            }
            t => return Err(violation(format!("a request of tag {t}"))),
        }
        w.flush()?;
    }
}

/// What a module escaping the engine would try, each `"<what>: ok"` or the error (or panic,
/// as Windows sockets are not started locked down): reading the file `path`, writing next to
/// it, running it and connecting to `port` of the loopback.
fn probe(path: &str, port: u16) -> Vec<String> {
    let said =
        |what: &str, f: &dyn Fn() -> io::Result<()>| match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
            Ok(Ok(())) => format!("{what}: ok"),
            Ok(Err(e)) => format!("{what}: {e}"),
            Err(_) => format!("{what}: panicked"),
        };
    let written = format!("{path}.probe");
    vec![
        said("read", &|| std::fs::read(path).map(drop)),
        said("write", &|| std::fs::write(&written, b"probe")),
        said("spawn", &|| std::process::Command::new(path).spawn().map(|mut c| drop(c.kill()))),
        said("connect", &|| std::net::TcpStream::connect(("127.0.0.1", port)).map(drop)),
    ]
}
