//! Encoding and decoding with more than one thread. The output is the same bytes as
//! with one thread ([`crate::codec::encode`], [`crate::codec::decode`]): the model pass and the
//! rANS chain stay on the calling thread, in order; around them, other threads do work that
//! does not depend on them.
//!
//! - Encoding, a helper builds the tables while the file is split into frames, then builds
//!   the encoder's inverse maps of the contexts the side information says may be used while
//!   the model pass runs.
//! - Decoding, the side information (adaptive range coder, independent of the rANS
//!   stream) is decoded on its own thread, ahead of the main pass; the tables and the
//!   rANS decoder are built on another while the headers are read; the output is assembled
//!   in parallel pieces.

use crate::bits::{BitReader, BitWriter, WordWriter};
use crate::codec::{
    self, Coded, Dec, Enc, Error, Frames, N_BYTE_STREAMS, S_CRC, S_GAPS, S_HDR, S_JUNK, S_RAWGR, S_SEGS, S_SIDE,
    S_TAILBITS,
};
use crate::frame::{Granule, Header, SideInfo, crc16, scf_layout};
use crate::model::{self, NBX, NCUR, NOC, NPOS};
use crate::rans::{self, Tables};
use crate::rc;
use crate::side::SideModel;
use crate::stream::R_PAD;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, sync_channel};

/// Below this size (input when compressing, coded form when decompressing) one thread does
/// everything: starting threads would cost about what they save.
pub(crate) const PAR_MIN: usize = 64 << 10;

/// Sets the flag when dropped (also when unwinding), so that a helper stops.
struct SetOnDrop<'a>(&'a AtomicBool);

impl Drop for SetOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// [`codec::encode`] with a helper thread.
pub(crate) fn encode(d: &[u8]) -> Coded {
    let stop = AtomicBool::new(false);
    let stop = &stop;
    std::thread::scope(|s| {
        // Channels are created here so that a panic on this thread drops the senders and
        // the helper returns.
        let (ctxs_tx, ctxs_rx) = sync_channel::<Vec<u32>>(1);
        let (ack_tx, ack_rx) = sync_channel::<()>(1);
        let helper = s.spawn(move || {
            let t = codec::tables();
            let ctxs = ctxs_rx.recv().ok()?;
            prewarm(t, &ctxs, stop);
            ack_tx.send(()).ok()
        });
        let _stop = SetOnDrop(stop);
        let sp = codec::split(d);
        let ctxs = likely_contexts(&sp.frames);
        ctxs_tx.send(ctxs).expect("helper thread");
        crate::huff::decoders();
        // Waits for the helper if it is still building the tables.
        let t = codec::tables();
        let mut c = Enc {
            t,
            rans: rans::Encoder::with_capacity(d.len() * 5 / 4 + model::MAX_GRANULE_SYMS),
            raw: WordWriter::with_capacity(d.len() / 4),
        };
        let (b, bit_lens) = codec::model_pass(sp, &mut c);
        stop.store(true, Ordering::Relaxed);
        // The helper no longer builds inverse maps once it acknowledges.
        ack_rx.recv().expect("helper thread");
        let words = c.rans.finish(t);
        let raw_bits = c.raw.len() as u64;
        let raw = c.raw.finish();
        if let Err(e) = helper.join() {
            std::panic::resume_unwind(e);
        }
        Coded { len: d.len() as u64, streams: b, words, raw, raw_bits, bit_lens }
    })
}

/// Builds the encoder's inverse maps of `ctxs` in order, until `stop`. The maps are the
/// same whoever builds them; [`rans::Encoder::finish`] builds the rest.
fn prewarm(t: &Tables, ctxs: &[u32], stop: &AtomicBool) {
    for chunk in ctxs.chunks(16) {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let mut e = rans::Encoder::with_capacity(chunk.len());
        let mut l = e.local_tracked(t);
        for &c in chunk {
            l.put(c as usize, 0);
        }
        e.put_back(l);
        e.finish(t);
    }
}

/// Contexts the model may use for these frames, according to their side information (a
/// superset, most likely first): count1 tables, escapes and pairs of the Huffman tables
/// used (by number of pairs), scalefactors of the sizes used.
fn likely_contexts(frames: &[(Header, SideInfo, usize, usize)]) -> Vec<u32> {
    let mut pairs = [0u64; 32];
    let mut c1 = [false; 2];
    let mut sizes = [0u64; 6];
    let mut stereo = false;
    for (h, si, _, _) in frames {
        stereo |= h.channels() == 2;
        for gr in 0..h.granules() {
            for ch in 0..h.channels() {
                let g = &si.gr[gr][ch];
                if g.big_values > 288 {
                    continue;
                }
                let reg = crate::huff::regions(*h, g);
                for t in 0..3 {
                    pairs[g.table_select[t] as usize] += reg[t] as u64;
                }
                c1[g.count1table as usize] = true;
                let lay = scf_layout(*h, si, gr, ch);
                for k in 0..4 {
                    if !lay.copy[k] && (1..=5).contains(&lay.size[k]) {
                        sizes[lay.size[k] as usize] += lay.count[k] as u64;
                    }
                }
            }
        }
    }
    pairs[0] = 0;
    let mut v = Vec::new();
    for (k, &used) in c1.iter().enumerate() {
        if used {
            let b = model::QUAD_BASE + k * model::QUAD_PER_TABLE;
            v.extend((b..b + model::QUAD_PER_TABLE).map(|c| c as u32));
        }
    }
    let mut ts: Vec<usize> = (1..32).filter(|&t| pairs[t] > 0).collect();
    ts.sort_by_key(|&t| std::cmp::Reverse(pairs[t]));
    for &t in &ts {
        if t >= 16 {
            let b = model::LIN_BASE + (t - 16) * 16;
            v.extend((b..b + 16).map(|c| c as u32));
        }
    }
    // Channel 0 (and mono) has no other channel class; channel 1 of stereo frames 1..NOC.
    let oc = if stereo { 0..NOC } else { 0..1 };
    for &t in &ts {
        let base = model::PAIR_BASE + t * model::PAIR_PER_TABLE;
        for bucket in 0..NBX {
            for o in oc.clone() {
                let bv = bucket * NOC + o;
                for pos in 0..NPOS {
                    for cur in 0..NCUR {
                        v.push((base + (bv * NPOS + pos) * NCUR + cur) as u32);
                    }
                }
            }
        }
    }
    let mut sz: Vec<usize> = (1..=5).filter(|&k| sizes[k] > 0).collect();
    sz.sort_by_key(|&k| std::cmp::Reverse(sizes[k]));
    for k in sz {
        let b = model::SCF_BASE + (k - 1) * 17 * 17;
        v.extend((b..b + 17 * 17).map(|c| c as u32));
    }
    v
}

/// Side information of one frame, decoded ahead.
#[derive(Clone, Copy, Default)]
struct FrameSide {
    si: SideInfo,
    placed: bool,
    resid: i32,
    /// Per granule (gr * 2 + ch): stored verbatim; count1 quadruples; tail bits.
    raw: [bool; 4],
    nq: [u16; 4],
    tl: [u32; 4],
}

enum SideMsg {
    Frames(Vec<FrameSide>),
    /// After the last frame: whether the side stream was long enough.
    End(bool),
}

/// Frames per message from the side information thread.
const SIDE_BATCH: usize = 32;

/// Decodes the side information of the frames with headers `hs` (as the main pass of
/// [`codec::main_pass`] would, in the same order) and sends it in batches. Stops when the
/// receiver is gone.
fn decode_side(stream: &[u8], hs: &[Header], tx: &std::sync::mpsc::SyncSender<SideMsg>) {
    let mut rc = rc::Decoder::new(stream);
    let mut m = SideModel::new();
    let mut batch = Vec::with_capacity(SIDE_BATCH);
    for &h in hs {
        let mut f = FrameSide::default();
        m.frame(&mut rc, h, &mut f.si, &mut f.placed, &mut f.resid);
        for gr in 0..h.granules() {
            for ch in 0..h.channels() {
                let k = gr * 2 + ch;
                if !f.placed {
                    let mut raw = true;
                    m.granule(&mut rc, h, &mut f.si.gr[gr][ch], ch, false, &mut raw);
                    continue;
                }
                let mut raw = false;
                m.granule(&mut rc, h, &mut f.si.gr[gr][ch], ch, true, &mut raw);
                f.raw[k] = raw;
                if !raw {
                    let (mut nq, mut tl) = (0, 0);
                    m.part3(&mut rc, ch, &mut nq, &mut tl);
                    f.nq[k] = nq as u16;
                    f.tl[k] = tl.min(u32::MAX as usize) as u32;
                }
            }
        }
        batch.push(f);
        if batch.len() == SIDE_BATCH && tx.send(SideMsg::Frames(std::mem::take(&mut batch))).is_err() {
            return;
        }
    }
    if !batch.is_empty() && tx.send(SideMsg::Frames(batch)).is_err() {
        return;
    }
    let _ = tx.send(SideMsg::End(rc.in_bounds()));
}

/// Side information from [`decode_side`] on another thread.
struct ThreadSide {
    rx: Receiver<SideMsg>,
    batch: std::vec::IntoIter<FrameSide>,
    cur: FrameSide,
    nch: usize,
    /// Granules of the current frame so far.
    n: usize,
    /// Index (gr * 2 + ch) of the last granule.
    k: usize,
}

impl ThreadSide {
    fn new(rx: Receiver<SideMsg>) -> ThreadSide {
        ThreadSide { rx, batch: Vec::new().into_iter(), cur: FrameSide::default(), nch: 1, n: 0, k: 0 }
    }

    fn next(&mut self) -> FrameSide {
        loop {
            if let Some(f) = self.batch.next() {
                return f;
            }
            match self.rx.recv() {
                Ok(SideMsg::Frames(v)) => self.batch = v.into_iter(),
                _ => panic!("side information thread"),
            }
        }
    }
}

impl codec::SideSrc for ThreadSide {
    fn frame(&mut self, h: Header, si: &mut SideInfo, placed: &mut bool, resid: &mut i32) {
        self.cur = self.next();
        *si = self.cur.si;
        *placed = self.cur.placed;
        *resid = self.cur.resid;
        self.nch = h.channels();
        self.n = 0;
    }
    #[inline(always)]
    fn granule(&mut self, _h: Header, _g: &mut Granule, _ch: usize, placed: bool, raw: &mut bool) {
        // The fields came with the frame; calls come in (gr, ch) order.
        self.k = self.n / self.nch * 2 + self.n % self.nch;
        self.n += 1;
        *raw = !placed || self.cur.raw[self.k];
    }
    #[inline(always)]
    fn part3(&mut self, _ch: usize, nq: &mut usize, tl: &mut usize) {
        *nq = self.cur.nq[self.k] as usize;
        *tl = self.cur.tl[self.k] as usize;
    }
    fn in_bounds(&mut self) -> bool {
        loop {
            match self.rx.recv() {
                Ok(SideMsg::End(b)) => return b,
                Ok(SideMsg::Frames(_)) => {}
                Err(_) => panic!("side information thread"),
            }
        }
    }
}

/// The rANS decoder over `words` (u32 LE) and the raw bits, padded.
fn make_decoder<'a>(t: &'a Tables, words: &[u8], raw: &[u8]) -> (rans::Decoder<'a>, Vec<u8>) {
    let w: Vec<u32> = words.chunks_exact(4).map(|w| u32::from_le_bytes(w.try_into().unwrap())).collect();
    let dec = rans::Decoder::new(t, &w);
    let mut rb = Vec::with_capacity(raw.len() + R_PAD);
    rb.extend_from_slice(raw);
    rb.resize(raw.len() + R_PAD, 0);
    (dec, rb)
}

/// [`codec::decode`] of the coded form with byte streams `streams`, rANS words `words` (u32
/// LE) and raw bits `raw`, with `threads` threads.
pub(crate) fn decode(
    t: &Tables,
    file_len: u64,
    streams: &[&[u8]; N_BYTE_STREAMS],
    words: &[u8],
    raw: &[u8],
    threads: usize,
) -> Result<Vec<u8>, Error> {
    std::thread::scope(|s| {
        let (hs_tx, hs_rx) = sync_channel::<Vec<Header>>(1);
        let (side_tx, side_rx) = sync_channel::<SideMsg>(8);
        let side_src = streams[S_SIDE];
        // With two threads, the side information thread builds the tables first.
        let helper = threads >= 3;
        let side_t = s.spawn(move || {
            if !helper {
                t.prepare_dec(threads);
            }
            if let Ok(hs) = hs_rx.recv() {
                decode_side(side_src, &hs, &side_tx);
            }
        });
        let dec_t = helper.then(|| {
            s.spawn(move || {
                t.prepare_dec(threads);
                make_decoder(t, words, raw)
            })
        });
        let res = (move || -> Result<Vec<u8>, Error> {
            let mut fr = codec::parse_frames(streams[S_SEGS], streams[S_HDR], file_len)?;
            hs_tx.send(fr.frames.iter().map(|f| f.0).collect()).expect("side information thread");
            drop(hs_tx);
            let (rans, raw_buf) = match dec_t {
                Some(t) => t.join().unwrap_or_else(|e| std::panic::resume_unwind(e)),
                None => make_decoder(t, words, raw),
            };
            let mut dc = Dec { rans, raw: crate::huff::BitBuf::new(&BitReader::new(&raw_buf, 0)) };
            let mut side = ThreadSide::new(side_rx);
            let r_out =
                codec::main_pass(&mut fr, streams[S_TAILBITS], streams[S_RAWGR], streams[S_GAPS], &mut side, &mut dc)?;
            drop(side);
            let out = assemble(&fr, streams[S_JUNK], streams[S_CRC], &r_out, threads)?;
            if !dc.rans.in_bounds() {
                return Err(Error("rans stream exhausted"));
            }
            Ok(out)
        })();
        side_t.join().unwrap_or_else(|e| std::panic::resume_unwind(e));
        res
    })
}

/// Output pieces: junk (offset and length in the junk stream) or a frame (index, CRC).
#[derive(Clone, Copy)]
enum Item {
    Junk(usize, usize),
    /// Frame index, and its CRC: [`CRC_NONE`], [`CRC_COMPUTE`] or the offset of the stored
    /// one in the CRC stream.
    Frame(usize, usize),
}

const CRC_NONE: usize = usize::MAX;
const CRC_COMPUTE: usize = usize::MAX - 1;

/// [`codec::assemble`] in parallel pieces (same output, same errors).
fn assemble(fr: &Frames, junk_s: &[u8], crc_s: &[u8], r_out: &[u8], threads: usize) -> Result<Vec<u8>, Error> {
    let parts = threads.min(fr.frames.len() / 1024 + 1);
    if parts <= 1 {
        return codec::assemble(fr, junk_s, crc_s, r_out);
    }
    // The layout, with the errors of the serial assembly in its order.
    let mut items: Vec<(usize, Item)> = Vec::with_capacity(fr.frames.len() + fr.junk.len());
    let mut crc = codec::Rd { d: crc_s, p: 0 };
    let (mut off, mut fi, mut jp) = (0usize, 0usize, 0usize);
    for (k, &j) in fr.junk.iter().enumerate() {
        junk_s.get(jp..jp + j).ok_or(Error("junk"))?;
        items.push((off, Item::Junk(jp, j)));
        off += j;
        jp += j;
        let Some(&(n, _)) = fr.runs.get(k) else { break };
        for _ in 0..n {
            let c = if !fr.frames[fi].0.protected() {
                CRC_NONE
            } else if crc.u8()? != 0 {
                CRC_COMPUTE
            } else {
                let p = crc.p;
                crc.bytes(2)?;
                p
            };
            items.push((off, Item::Frame(fi, c)));
            off += fr.flens[fi];
            fi += 1;
        }
    }
    let total = off;
    let mut out = Vec::<u8>::with_capacity(total);
    std::thread::scope(|s| {
        let mut rest = &mut out.spare_capacity_mut()[..total];
        let mut handles = Vec::with_capacity(parts - 1);
        let mut first = None;
        let mut i0 = 0;
        let items = &items;
        for p in 0..parts {
            // Items up to about p + 1 parts of the output.
            let target = total / parts * (p + 1);
            let i1 = if p + 1 == parts { items.len() } else { i0 + items[i0..].partition_point(|it| it.0 < target) };
            let end = if i1 == items.len() { total } else { items[i1].0 };
            let start = if i0 == items.len() { total } else { items[i0].0 };
            let (dst, r) = std::mem::take(&mut rest).split_at_mut(end - start);
            rest = r;
            let its = &items[i0..i1];
            if p == 0 {
                first = Some((its, dst));
            } else {
                handles.push(s.spawn(move || write_items(its, dst, fr, junk_s, crc_s, r_out)));
            }
            i0 = i1;
        }
        let (its, dst) = first.unwrap();
        write_items(its, dst, fr, junk_s, crc_s, r_out);
        for h in handles {
            h.join().unwrap_or_else(|e| std::panic::resume_unwind(e));
        }
    });
    // SAFETY: the pieces cover 0..total and each wrote all of its bytes (`write_items`).
    unsafe { out.set_len(total) };
    Ok(out)
}

/// Writes `items` (their bytes are exactly `dst`).
fn write_items(
    items: &[(usize, Item)],
    dst: &mut [MaybeUninit<u8>],
    fr: &Frames,
    junk_s: &[u8],
    crc_s: &[u8],
    r_out: &[u8],
) {
    let mut p = 0usize;
    let mut put = |b: &[u8]| {
        assert!(b.len() <= dst.len() - p);
        // SAFETY: in bounds (above); u8 and MaybeUninit<u8> have the same layout.
        unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), dst.as_mut_ptr().add(p) as *mut u8, b.len()) };
        p += b.len();
    };
    for &(_, it) in items {
        match it {
            Item::Junk(jp, j) => put(&junk_s[jp..jp + j]),
            Item::Frame(fi, c) => {
                let (h, si, pay, len) = &fr.frames[fi];
                let hb = h.0.to_be_bytes();
                put(&hb);
                let mut sw = BitWriter::new();
                si.write(*h, &mut sw);
                let side = sw.finish();
                assert_eq!(side.len(), h.side_info_len());
                match c {
                    CRC_NONE => {}
                    CRC_COMPUTE => put(&crc16(&hb, &side).to_be_bytes()),
                    o => put(&crc_s[o..o + 2]),
                }
                put(&side);
                put(&r_out[*pay..pay + len]);
            }
        }
    }
    assert_eq!(p, dst.len());
}
