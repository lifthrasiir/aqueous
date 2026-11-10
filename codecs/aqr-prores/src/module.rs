//! The code of the module `prores-recons` (see `formula`, and `FORMAT.md`), which needs no
//! aqr-formula, so that aqr-modules builds it into its Wasm module.
//!
//! A part module: its instance reads the side stream, the stripes' pieces and the gaps in
//! order through windows, and writes the output through its window. A gang of tasks, one for
//! each stripe ([`codec::Stripe`]), lives for the whole chunk: for each modelled frame the
//! instance sends each its message (the side piece and the stripe's piece) and takes what it
//! writes, and keeps [`DEPTH`] frames in flight so that none waits for another. What it holds
//! is a few frames' worth, whatever the chunk's length ([`plan`]). Natively the same code runs
//! through the emulation of aqr-api, the tasks on threads.

use aqr_api::chan::{self, Receiver, Sender};
use aqr_api::stack::block;
use aqr_api::stream::{In, Put, Reader, Written};
use aqr_common::msb::PAD;
use aqr_common::varint;

use crate::codec::{self, Info, Limits, MAX_STRIPES, SideState, Stripe};
use crate::{Error, Result};

/// Most blocks of a frame and bytes of a frame that parameters may give (8K 4:4:4 takes about
/// 1.6 M blocks; a frame's size is a u32).
const MAX_BLOCKS: u64 = 1 << 24;
const MAX_FRAME: u64 = u32::MAX as u64;

/// The parameters of an application: the limits of its chunk, as varints.
pub fn params(l: &Limits) -> Vec<u8> {
    let mut p = Vec::new();
    l.to_array().into_iter().for_each(|v| varint::put(&mut p, v));
    p
}

/// The limits that parameters `p` give.
pub fn limits(p: &[u8]) -> Result<Limits> {
    let bad = || Error("bad parameters");
    let mut at = 0;
    let mut a = [0u64; 12];
    for v in &mut a {
        *v = varint::get(p, &mut at).map_err(|_| bad())?;
    }
    let l = Limits::from_array(a);
    let ok = at == p.len()
        && l.blocks <= MAX_BLOCKS
        && l.frame <= MAX_FRAME
        && l.stripe_blocks <= l.blocks
        && (1..=MAX_STRIPES as u64).contains(&l.stripes)
        && (1..=2).contains(&l.fields)
        && l.head <= l.frame
        && l.widths <= 2 * 8192
        && l.stripe_slices <= l.blocks
        && l.stripe_bytes <= l.frame
        && l.side_piece <= MAX_FRAME
        && l.piece <= MAX_FRAME
        && l.result <= MAX_FRAME;
    ok.then_some(l).ok_or_else(bad)
}

/// Pages of the windows of the module ([`decode_part`]): of each input and of the output.
pub const WINDOW: u32 = 16;

/// Frames in flight at most, and the messages a channel holds: the instance sends a frame to
/// every task before it takes the oldest back, so no end waits for one that waits for it.
pub const DEPTH: usize = 3;

/// Bytes in which the instance takes its inputs.
const PIECE: usize = 1 << 16;

/// The memory plan of the module ([`aqr_api::stack`]): the limits of its instance and of its
/// gang of tasks (their memories and the messages their channels hold, which come with it), and
/// what the messages may hold. Of the params and the initial memory (`initial` bytes) only:
/// whatever the lengths of the inputs and the output.
pub fn plan(params: &[u8], initial: u64) -> Option<((u64, u64), u64)> {
    let l = limits(params).ok()?;
    let pages = aqr_api::stack::pages;
    let k = l.stripes;
    // (before the spawn: the ends of the channels to the tasks and from them)
    let top = initial + 2 * block(8 * k);
    let msg = block(l.message() + PAD as u64);
    let out = block(l.result + 1);
    let task_own = msg + out + Stripe::plan(&l);
    let task = pages(top).max(pages(top + task_own));
    let queued = DEPTH as u64 * k * (l.message() + l.result + 1);
    let gang = k * task + queued;
    // (the instance: the tasks' handles, then what it starts with)
    let instance = block(k)
        + 3 * block(PIECE as u64)
        + SideState::plan(l.head)
        + DEPTH as u64 * Info::plan(l.head, 0, l.widths)
        + msg
        + block(l.frame)
        + block(12 * k)
        + k * out;
    Some(((pages(top + instance), gang), queued))
}

/// The module: the chunk of the side stream, the stripes' pieces and the gaps, decoded to the
/// span of `len` bytes (through the emulation of aqr-api, the tasks on threads).
#[cfg(not(target_family = "wasm"))]
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64) -> Result<Vec<u8>> {
    let out = aqr_api::emu::apply(&ENTRY, inputs, &[], params, &[len]).map_err(|_| Error("corrupt chunk"))?;
    Ok(out.into_iter().next().expect("an output"))
}

/// The entry point of the module for the emulation (aqr-modules' has the same windows).
#[cfg(not(target_family = "wasm"))]
pub const ENTRY: aqr_api::emu::Entry = aqr_api::emu::Entry {
    kind: aqr_api::emu::Kind::Part,
    windows: &[
        aqr_api::Window::Input(0, WINDOW),
        aqr_api::Window::Input(1, WINDOW),
        aqr_api::Window::Input(2, WINDOW),
        aqr_api::Window::Output(0, WINDOW),
    ],
    arity: 5,
    call: |a| decode_part(a[0], [a[1], a[2], a[3]], a[4]),
};

/// The module as a part module (its entry point, of `aqr_api::entry!`): the inputs taken and the
/// output put in order through windows, in pieces ([`WINDOW`]).
pub fn decode_part(params_len: u64, lens: [u64; 3], size: u64) {
    aqr_api::finish(part(params_len, lens, size));
}

fn part(params_len: u64, lens: [u64; 3], size: u64) -> Result<()> {
    use aqr_api::{Memory, Params};
    // (twelve varints of five bytes at most)
    let mut buf = [0; 64];
    let p = buf.get_mut(..params_len as usize).ok_or(Error("bad parameters"))?;
    Params::read(0, p);
    let l = limits(p)?;
    let k = l.stripes as usize;
    let to: Vec<(Sender, Receiver)> = (0..k).map(|_| chan::channel(DEPTH)).collect();
    let from: Vec<(Sender, Receiver)> = (0..k).map(|_| chan::channel(DEPTH)).collect();
    let task = |i: usize, _: &mut Vec<u8>| run_stripe(&l, i, to[i].1, from[i].0);
    // SAFETY: dropped below, before `task` goes
    let tasks = unsafe { aqr_api::task::spawn_gang(0, k, &task) };
    let r = drive(&l, lens, size, &to, &from);
    drop(tasks);
    r
}

/// The task of stripe `s`: messages from the instance, a result sent back for each frame
/// modelled. Ends as its channel closes.
fn run_stripe(l: &Limits, s: usize, rx: Receiver, tx: Sender) {
    let mut msg = Vec::with_capacity(l.message() as usize + PAD);
    let mut out = Vec::with_capacity(l.result as usize + 1);
    let mut stripe = Stripe::new(l, s);
    let mut failed = false;
    while rx.recv_into(&mut msg) {
        if msg[0] == 0 {
            stripe.kept();
            continue;
        }
        let end = msg.len();
        msg.resize(end + PAD, 0);
        if failed || !stripe.frame(l, &msg[1..], end - 1, &mut out) {
            failed = true;
            out.clear();
            out.push(0);
        }
        tx.send(&out);
    }
    tx.close();
}

/// Closes the channels to the tasks as it is dropped (once), so that they end.
struct Close<'a>(&'a [(Sender, Receiver)]);

impl Drop for Close<'_> {
    fn drop(&mut self) {
        self.0.iter().for_each(|c| c.0.close());
    }
}

/// Appends a piece of at most `max` bytes (its code and raw bits) from `r` to `msg`, with its
/// lengths.
fn read_piece(r: &mut Reader, msg: &mut Vec<u8>, max: u64) -> Result<()> {
    let bad = || Error("corrupt chunk");
    let a = r.num().map_err(|_| bad())?;
    let b = r.num().map_err(|_| bad())?;
    let n = a.checked_add(b).filter(|&n| n <= max).ok_or_else(bad)?;
    varint::put(msg, a);
    varint::put(msg, b);
    r.each(n, |d| {
        msg.extend_from_slice(d);
        Some(())
    })
    .ok_or_else(bad)
}

/// The instance: items of the side stream in order, the modelled ones sent to the tasks and
/// taken back, assembled and written, up to [`DEPTH`] in flight.
fn drive(l: &Limits, lens: [u64; 3], size: u64, to: &[(Sender, Receiver)], from: &[(Sender, Receiver)]) -> Result<()> {
    let bad = || Error("corrupt chunk");
    let k = l.stripes as usize;
    let (mut i0, mut i1, mut i2) = (In::<0>::new(lens[0]), In::<1>::new(lens[1]), In::<2>::new(lens[2]));
    let (mut side, mut stripes, mut gaps) =
        (Reader::new(&mut i0, PIECE), Reader::new(&mut i1, PIECE), Reader::new(&mut i2, PIECE));
    let mut out = Written::new(size);
    let mut st = SideState::new(l.head as usize);
    let mut ring: [Info; DEPTH] = std::array::from_fn(|_| Info::new(l.head as usize, 0, l.widths as usize));
    let mut gap_of = [0u64; DEPTH];
    let mut msg = Vec::with_capacity(l.message() as usize + PAD);
    let mut frame = Vec::with_capacity(l.frame as usize);
    let mut results: Vec<Vec<u8>> = Vec::with_capacity(k);
    results.extend((0..k).map(|_| Vec::with_capacity(l.result as usize + 1)));
    let _close = Close(to);
    let (mut head, mut inflight) = (0, 0);

    // The oldest frame in flight, taken back and written after its gap.
    macro_rules! finish_one {
        () => {{
            for (r, c) in results.iter_mut().zip(from) {
                if !c.1.recv_into(r) || r.first() != Some(&1) {
                    return Err(Error("a stripe failed"));
                }
            }
            codec::assemble(&ring[head], &results, l, &mut frame)?;
            gaps.each(gap_of[head], |d| out.put(d)).ok_or_else(bad)?;
            out.put(&frame).ok_or_else(bad)?;
            head = (head + 1) % DEPTH;
            inflight -= 1;
        }};
    }

    loop {
        let tag = side.byte().ok_or_else(bad)?;
        let gap = side.num().map_err(|_| bad())?;
        match tag {
            0 => {
                while inflight > 0 {
                    finish_one!();
                }
                gaps.each(gap, |d| out.put(d)).ok_or_else(bad)?;
                break;
            }
            1 => {
                let n = side.num().map_err(|_| bad())?;
                if n > l.frame {
                    return Err(bad());
                }
                while inflight > 0 {
                    finish_one!();
                }
                gaps.each(gap, |d| out.put(d)).ok_or_else(bad)?;
                side.each(n, |d| out.put(d)).ok_or_else(bad)?;
                to.iter().for_each(|c| c.0.send(&[0]));
            }
            2 => {
                if inflight == DEPTH {
                    finish_one!();
                }
                let slot = (head + inflight) % DEPTH;
                msg.clear();
                msg.push(1);
                read_piece(&mut side, &mut msg, l.side_piece)?;
                let prefix = msg.len();
                // (decoded here for the frame's headers; the tasks decode it for their slices)
                let mut at = 0;
                msg.extend_from_slice(&[0; PAD]);
                let piece = codec::take_piece(&msg[1..], &mut at, prefix - 1)?;
                codec::side_dec(piece, &mut st, &mut ring[slot], l, None)?;
                gap_of[slot] = gap;
                for c in to {
                    msg.truncate(prefix);
                    read_piece(&mut stripes, &mut msg, l.piece)?;
                    c.0.send(&msg);
                }
                inflight += 1;
            }
            _ => return Err(bad()),
        }
    }
    if !side.done() || !stripes.done() || !gaps.done() || out.total() != size {
        return Err(bad());
    }
    Ok(())
}
