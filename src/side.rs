//! Adaptive model of the side information, coded with [`crate::rc`].
//! Encoders differ a lot in how they choose these fields, so the model learns per file.

use crate::frame::{Granule, Header, SideInfo, sfb_widths};
use crate::rc::{BitCoder, P_INIT, Prob, SIntProbs, UIntProbs};

pub struct SideModel {
    placed: Prob,
    mdb: SIntProbs,
    mdb_raw: UIntProbs,
    private: [[Prob; 32]; 6],
    scfsi: [[[Prob; 16]; 16]; 2],
    raw: Prob,
    ws: [[Prob; 2]; 2],
    block_type: [[Prob; 4]; 4],
    mixed: Prob,
    /// By region, previous table of that region.
    table: Vec<[Prob; 32]>,
    /// region0_count by the band where the big values end; region1_count by that and
    /// region0_count.
    r0: Vec<[Prob; 16]>,
    r1: Vec<[Prob; 8]>,
    sbg: [[Prob; 8]; 3],
    gain: [SIntProbs; 2],
    big_values: [SIntProbs; 2],
    sfc1: [[Prob; 16]; 16],
    sfc2: Vec<Prob>,
    preflag: [Prob; 2],
    scale: [Prob; 2],
    c1t: [Prob; 2],
    nquads: [SIntProbs; 2],
    tail: UIntProbs,
    p23: UIntProbs,
    // Previous values per channel.
    prev: [Prev; 2],
}

#[derive(Clone, Copy)]
struct Prev {
    ws: bool,
    block_type: u8,
    table: [u8; 3],
    gain: u8,
    big_values: u16,
    sfc: u8,
    preflag: bool,
    scale: bool,
    c1t: bool,
    nquads: u16,
    scfsi: u8,
}

impl Default for Prev {
    fn default() -> Self {
        Prev {
            ws: false,
            block_type: 0,
            table: [0; 3],
            gain: 0,
            big_values: 0,
            sfc: 0,
            preflag: false,
            scale: false,
            c1t: false,
            nquads: 0,
            scfsi: 0,
        }
    }
}

impl SideModel {
    pub fn new() -> Box<SideModel> {
        Box::new(SideModel {
            placed: P_INIT,
            mdb: SIntProbs::default(),
            mdb_raw: UIntProbs::default(),
            private: [[P_INIT; 32]; 6],
            scfsi: [[[P_INIT; 16]; 16]; 2],
            raw: P_INIT,
            ws: [[P_INIT; 2]; 2],
            block_type: [[P_INIT; 4]; 4],
            mixed: P_INIT,
            table: vec![[P_INIT; 32]; 3 * 32],
            r0: vec![[P_INIT; 16]; 24],
            r1: vec![[P_INIT; 8]; 24 * 16],
            sbg: [[P_INIT; 8]; 3],
            gain: Default::default(),
            big_values: Default::default(),
            sfc1: [[P_INIT; 16]; 16],
            sfc2: vec![P_INIT; 512],
            preflag: [P_INIT; 2],
            scale: [P_INIT; 2],
            c1t: [P_INIT; 2],
            nquads: Default::default(),
            tail: UIntProbs::default(),
            p23: UIntProbs::default(),
            prev: [Prev::default(); 2],
        })
    }

    /// Frame fields: whether the frame's main data is placed, then main_data_begin
    /// (`mdb_resid` relative to the prediction if placed, else `si.main_data_begin` itself),
    /// private bits and scfsi.
    pub fn frame<C: BitCoder>(
        &mut self,
        c: &mut C,
        h: Header,
        si: &mut SideInfo,
        placed: &mut bool,
        mdb_resid: &mut i32,
    ) {
        *placed = c.bit(&mut self.placed, *placed as u32) != 0;
        if *placed {
            *mdb_resid = c.sint(&mut self.mdb, *mdb_resid);
        } else {
            si.main_data_begin = c.uint(&mut self.mdb_raw, si.main_data_begin as u32) as u16 & 0x1ff;
        }
        let nbits = if h.lsf() {
            h.channels()
        } else if h.mono() {
            5
        } else {
            3
        };
        si.private_bits = c.tree(&mut self.private[nbits], nbits as u32, si.private_bits as u32) as u8;
        if !h.lsf() {
            for ch in 0..h.channels() {
                let p = &mut self.prev[ch];
                si.scfsi[ch] = c.tree(&mut self.scfsi[ch][p.scfsi as usize], 4, si.scfsi[ch] as u32) as u8;
                p.scfsi = si.scfsi[ch];
            }
        }
    }

    /// Granule fields. `raw` (coded only if `placed`) marks a granule stored verbatim;
    /// part2_3_length is coded only for those and in frames that are not placed.
    pub fn granule<C: BitCoder>(
        &mut self,
        c: &mut C,
        h: Header,
        g: &mut Granule,
        ch: usize,
        placed: bool,
        raw: &mut bool,
    ) {
        if placed {
            *raw = c.bit(&mut self.raw, *raw as u32) != 0;
        } else {
            *raw = true;
        }
        let p = self.prev[ch];
        g.window_switching = c.bit(&mut self.ws[ch][p.ws as usize], g.window_switching as u32) != 0;
        let nt = if g.window_switching {
            g.block_type = c.tree(&mut self.block_type[p.block_type as usize], 2, g.block_type as u32) as u8;
            g.mixed = c.bit(&mut self.mixed, g.mixed as u32) != 0;
            2
        } else {
            g.block_type = 0;
            g.mixed = false;
            3
        };
        g.big_values =
            (p.big_values as i32 + c.sint(&mut self.big_values[ch], g.big_values as i32 - p.big_values as i32)) as u16
                & 0x1ff;
        for t in 0..nt {
            g.table_select[t] =
                c.tree(&mut self.table[t * 32 + p.table[t] as usize], 5, g.table_select[t] as u32) as u8;
        }
        if g.window_switching {
            g.table_select[2] = 0;
            g.region0_count = 0;
            g.region1_count = 0;
            for s in 0..3 {
                g.subblock_gain[s] = c.tree(&mut self.sbg[s], 3, g.subblock_gain[s] as u32) as u8;
            }
        } else {
            g.subblock_gain = [0; 3];
            // The long band in which the big values end.
            let mut band = 0;
            let mut line = 0usize;
            for &w in sfb_widths(h, g) {
                if line >= 2 * g.big_values as usize {
                    break;
                }
                line += w as usize;
                band += 1;
            }
            let band = band.min(23);
            g.region0_count = c.tree(&mut self.r0[band], 4, g.region0_count as u32) as u8;
            g.region1_count =
                c.tree(&mut self.r1[band * 16 + g.region0_count as usize], 3, g.region1_count as u32) as u8;
        }
        g.global_gain = (p.gain as i32 + c.sint(&mut self.gain[ch], g.global_gain as i32 - p.gain as i32)) as u8;
        if h.lsf() {
            g.scalefac_compress = c.tree(&mut self.sfc2, 9, g.scalefac_compress as u32) as u16;
            g.preflag = g.scalefac_compress >= 500;
        } else {
            g.scalefac_compress = c.tree(&mut self.sfc1[p.sfc as usize], 4, g.scalefac_compress as u32) as u16;
            g.preflag = c.bit(&mut self.preflag[p.preflag as usize], g.preflag as u32) != 0;
        }
        g.scalefac_scale = c.bit(&mut self.scale[p.scale as usize], g.scalefac_scale as u32) != 0;
        g.count1table = c.bit(&mut self.c1t[p.c1t as usize], g.count1table as u32) != 0;
        if *raw {
            g.part2_3_length = c.uint(&mut self.p23, g.part2_3_length as u32) as u16 & 0xfff;
        }
        let q = &mut self.prev[ch];
        q.ws = g.window_switching;
        q.block_type = g.block_type;
        q.table = g.table_select;
        q.gain = g.global_gain;
        q.big_values = g.big_values;
        q.sfc = (g.scalefac_compress & 15) as u8;
        q.preflag = g.preflag;
        q.scale = g.scalefac_scale;
        q.c1t = g.count1table;
    }

    /// Count1 quadruples and tail bits of a decoded granule.
    pub fn part3<C: BitCoder>(&mut self, c: &mut C, ch: usize, nquads: &mut usize, tail: &mut usize) {
        let p = &mut self.prev[ch];
        *nquads =
            (p.nquads as i32 + c.sint(&mut self.nquads[ch], *nquads as i32 - p.nquads as i32)).clamp(0, 144) as usize;
        p.nquads = *nquads as u16;
        *tail = c.uint(&mut self.tail, *tail as u32) as usize;
    }
}
