//! MPEG audio Layer III frame headers and side information (MPEG-1, MPEG-2 and MPEG-2.5).

use crate::bits::{BitReader, BitWriter};

/// A 4-byte frame header.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Header(pub u32);

const BITRATE_V1: [u16; 15] = [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320];
const BITRATE_V2: [u16; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];
const SAMPLE_RATE: [u32; 3] = [44100, 48000, 32000];

impl Header {
    pub fn from_bytes(b: &[u8]) -> Header {
        Header(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    /// 3 = MPEG-1, 2 = MPEG-2, 0 = MPEG-2.5 (1 is reserved).
    pub fn version(self) -> u32 {
        (self.0 >> 19) & 3
    }
    pub fn lsf(self) -> bool {
        self.version() != 3
    }
    pub fn layer(self) -> u32 {
        (self.0 >> 17) & 3
    }
    pub fn protected(self) -> bool {
        (self.0 >> 16) & 1 == 0
    }
    pub fn bitrate_index(self) -> u32 {
        (self.0 >> 12) & 15
    }
    pub fn sr_index(self) -> u32 {
        (self.0 >> 10) & 3
    }
    pub fn padding(self) -> bool {
        (self.0 >> 9) & 1 != 0
    }
    pub fn mode(self) -> u32 {
        (self.0 >> 6) & 3
    }
    pub fn mode_ext(self) -> u32 {
        (self.0 >> 4) & 3
    }
    pub fn mono(self) -> bool {
        self.mode() == 3
    }
    pub fn channels(self) -> usize {
        if self.mono() { 1 } else { 2 }
    }
    pub fn granules(self) -> usize {
        if self.lsf() { 1 } else { 2 }
    }
    pub fn ms_stereo(self) -> bool {
        self.mode() == 1 && self.mode_ext() & 2 != 0
    }
    pub fn i_stereo(self) -> bool {
        self.mode() == 1 && self.mode_ext() & 1 != 0
    }

    /// Sync, a known version, Layer III, a bitrate index other than free format or 15,
    /// and a valid sample rate.
    pub fn valid(self) -> bool {
        self.0 >> 21 == 0x7ff
            && self.version() != 1
            && self.layer() == 1
            && (1..15).contains(&self.bitrate_index())
            && self.sr_index() != 3
    }

    pub fn sample_rate(self) -> u32 {
        SAMPLE_RATE[self.sr_index() as usize] >> (3 - self.version()).min(2)
    }

    /// Row of the scalefactor band tables (0..8, 44.1 kHz MPEG-1 is 5).
    pub fn sfb_row(self) -> usize {
        let v = match self.version() {
            3 => 6,
            2 => 3,
            _ => 0,
        };
        let i = v + self.sr_index() as usize;
        i - (i != 0) as usize
    }

    /// Frame length in bytes, for a valid header.
    pub fn frame_len(self) -> usize {
        let (tab, mul) = if self.lsf() { (&BITRATE_V2, 72000) } else { (&BITRATE_V1, 144000) };
        (mul * tab[self.bitrate_index() as usize] as u32 / self.sample_rate()) as usize + self.padding() as usize
    }

    pub fn side_info_len(self) -> usize {
        match (self.lsf(), self.mono()) {
            (false, true) => 17,
            (false, false) => 32,
            (true, true) => 9,
            (true, false) => 17,
        }
    }

    /// Bytes before the main data: header, CRC and side information.
    pub fn prefix_len(self) -> usize {
        4 + 2 * self.protected() as usize + self.side_info_len()
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Granule {
    pub part2_3_length: u16,
    pub big_values: u16,
    pub global_gain: u8,
    pub scalefac_compress: u16,
    pub window_switching: bool,
    pub block_type: u8,
    pub mixed: bool,
    pub table_select: [u8; 3],
    pub subblock_gain: [u8; 3],
    pub region0_count: u8,
    pub region1_count: u8,
    /// Coded only in MPEG-1; derived from scalefac_compress otherwise.
    pub preflag: bool,
    pub scalefac_scale: bool,
    pub count1table: bool,
}

impl Granule {
    pub fn short(&self) -> bool {
        self.window_switching && self.block_type == 2
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct SideInfo {
    pub main_data_begin: u16,
    pub private_bits: u8,
    /// MPEG-1 only; low 4 bits per channel.
    pub scfsi: [u8; 2],
    pub gr: [[Granule; 2]; 2],
}

impl SideInfo {
    /// Parses the side information; `r` must be positioned at its start.
    pub fn read(h: Header, r: &mut BitReader) -> SideInfo {
        let mut si = SideInfo::default();
        let nch = h.channels();
        let lsf = h.lsf();
        if !lsf {
            si.main_data_begin = r.get(9) as u16;
            si.private_bits = r.get(if nch == 1 { 5 } else { 3 }) as u8;
            for ch in 0..nch {
                si.scfsi[ch] = r.get(4) as u8;
            }
        } else {
            si.main_data_begin = r.get(8) as u16;
            si.private_bits = r.get(nch as u32) as u8;
        }
        for gr in 0..h.granules() {
            for ch in 0..nch {
                let g = &mut si.gr[gr][ch];
                g.part2_3_length = r.get(12) as u16;
                g.big_values = r.get(9) as u16;
                g.global_gain = r.get(8) as u8;
                g.scalefac_compress = r.get(if lsf { 9 } else { 4 }) as u16;
                g.window_switching = r.get(1) != 0;
                if g.window_switching {
                    g.block_type = r.get(2) as u8;
                    g.mixed = r.get(1) != 0;
                    g.table_select[0] = r.get(5) as u8;
                    g.table_select[1] = r.get(5) as u8;
                    for s in 0..3 {
                        g.subblock_gain[s] = r.get(3) as u8;
                    }
                } else {
                    for t in 0..3 {
                        g.table_select[t] = r.get(5) as u8;
                    }
                    g.region0_count = r.get(4) as u8;
                    g.region1_count = r.get(3) as u8;
                }
                g.preflag = if lsf { g.scalefac_compress >= 500 } else { r.get(1) != 0 };
                g.scalefac_scale = r.get(1) != 0;
                g.count1table = r.get(1) != 0;
            }
        }
        si
    }

    pub fn write(&self, h: Header, w: &mut BitWriter) {
        let nch = h.channels();
        let lsf = h.lsf();
        if !lsf {
            w.put(self.main_data_begin as u32, 9);
            w.put(self.private_bits as u32, if nch == 1 { 5 } else { 3 });
            for ch in 0..nch {
                w.put(self.scfsi[ch] as u32, 4);
            }
        } else {
            w.put(self.main_data_begin as u32, 8);
            w.put(self.private_bits as u32, nch as u32);
        }
        for gr in 0..h.granules() {
            for ch in 0..nch {
                let g = &self.gr[gr][ch];
                w.put(g.part2_3_length as u32, 12);
                w.put(g.big_values as u32, 9);
                w.put(g.global_gain as u32, 8);
                w.put(g.scalefac_compress as u32, if lsf { 9 } else { 4 });
                w.put(g.window_switching as u32, 1);
                if g.window_switching {
                    w.put(g.block_type as u32, 2);
                    w.put(g.mixed as u32, 1);
                    w.put(g.table_select[0] as u32, 5);
                    w.put(g.table_select[1] as u32, 5);
                    for s in 0..3 {
                        w.put(g.subblock_gain[s] as u32, 3);
                    }
                } else {
                    for t in 0..3 {
                        w.put(g.table_select[t] as u32, 5);
                    }
                    w.put(g.region0_count as u32, 4);
                    w.put(g.region1_count as u32, 3);
                }
                if !lsf {
                    w.put(g.preflag as u32, 1);
                }
                w.put(g.scalefac_scale as u32, 1);
                w.put(g.count1table as u32, 1);
            }
        }
    }
}

/// CRC-16 (polynomial 0x8005, initial 0xffff) of header bytes 2..4 and the side information,
/// as stored after the header of a protected frame.
pub fn crc16(header: &[u8], side_info: &[u8]) -> u16 {
    let mut crc: u16 = 0xffff;
    for &b in header[2..4].iter().chain(side_info) {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x8005 } else { crc << 1 };
        }
    }
    crc
}

/// Scalefactor band widths for long blocks, per [`Header::sfb_row`]; the last band runs to 576.
pub static SFB_LONG: [[u8; 22]; 8] = [
    [6, 6, 6, 6, 6, 6, 8, 10, 12, 14, 16, 20, 24, 28, 32, 38, 46, 52, 60, 68, 58, 54],
    [12, 12, 12, 12, 12, 12, 16, 20, 24, 28, 32, 40, 48, 56, 64, 76, 90, 2, 2, 2, 2, 2],
    [6, 6, 6, 6, 6, 6, 8, 10, 12, 14, 16, 20, 24, 28, 32, 38, 46, 52, 60, 68, 58, 54],
    [6, 6, 6, 6, 6, 6, 8, 10, 12, 14, 16, 18, 22, 26, 32, 38, 46, 54, 62, 70, 76, 36],
    [6, 6, 6, 6, 6, 6, 8, 10, 12, 14, 16, 20, 24, 28, 32, 38, 46, 52, 60, 68, 58, 54],
    [4, 4, 4, 4, 4, 4, 6, 6, 8, 8, 10, 12, 16, 20, 24, 28, 34, 42, 50, 54, 76, 158],
    [4, 4, 4, 4, 4, 4, 6, 6, 6, 8, 10, 12, 16, 18, 22, 28, 34, 40, 46, 54, 54, 192],
    [4, 4, 4, 4, 4, 4, 6, 6, 8, 10, 12, 16, 20, 24, 30, 38, 46, 56, 68, 84, 102, 26],
];

/// Scalefactor band widths for short blocks, in coding order (each band three times, once
/// per window).
pub static SFB_SHORT: [[u8; 39]; 8] = [
    [4, 4, 4, 4, 4, 4, 4, 4, 4, 6, 6, 6, 8, 8, 8, 10, 10, 10, 12, 12, 12, 14, 14, 14, 18, 18, 18, 24, 24, 24, 30, 30, 30, 40, 40, 40, 18, 18, 18],
    [8, 8, 8, 8, 8, 8, 8, 8, 8, 12, 12, 12, 16, 16, 16, 20, 20, 20, 24, 24, 24, 28, 28, 28, 36, 36, 36, 2, 2, 2, 2, 2, 2, 2, 2, 2, 26, 26, 26],
    [4, 4, 4, 4, 4, 4, 4, 4, 4, 6, 6, 6, 6, 6, 6, 8, 8, 8, 10, 10, 10, 14, 14, 14, 18, 18, 18, 26, 26, 26, 32, 32, 32, 42, 42, 42, 18, 18, 18],
    [4, 4, 4, 4, 4, 4, 4, 4, 4, 6, 6, 6, 8, 8, 8, 10, 10, 10, 12, 12, 12, 14, 14, 14, 18, 18, 18, 24, 24, 24, 32, 32, 32, 44, 44, 44, 12, 12, 12],
    [4, 4, 4, 4, 4, 4, 4, 4, 4, 6, 6, 6, 8, 8, 8, 10, 10, 10, 12, 12, 12, 14, 14, 14, 18, 18, 18, 24, 24, 24, 30, 30, 30, 40, 40, 40, 18, 18, 18],
    [4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 6, 6, 6, 8, 8, 8, 10, 10, 10, 12, 12, 12, 14, 14, 14, 18, 18, 18, 22, 22, 22, 30, 30, 30, 56, 56, 56],
    [4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 6, 6, 6, 6, 6, 6, 10, 10, 10, 12, 12, 12, 14, 14, 14, 16, 16, 16, 20, 20, 20, 26, 26, 26, 66, 66, 66],
    [4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 6, 6, 6, 8, 8, 8, 12, 12, 12, 16, 16, 16, 20, 20, 20, 26, 26, 26, 34, 34, 34, 42, 42, 42, 12, 12, 12],
];

/// Mixed blocks: long bands up to line 36 (MPEG-1: 8 bands; LSF: 6), then short bands
/// from band 3, in coding order.
pub static SFB_MIXED: [[u8; 39]; 8] = [
    [6, 6, 6, 6, 6, 6, 6, 6, 6, 8, 8, 8, 10, 10, 10, 12, 12, 12, 14, 14, 14, 18, 18, 18, 24, 24, 24, 30, 30, 30, 40, 40, 40, 18, 18, 18, 0, 0, 0],
    [12, 12, 12, 4, 4, 4, 8, 8, 8, 12, 12, 12, 16, 16, 16, 20, 20, 20, 24, 24, 24, 28, 28, 28, 36, 36, 36, 2, 2, 2, 2, 2, 2, 2, 2, 2, 26, 26, 26],
    [6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 8, 8, 8, 10, 10, 10, 14, 14, 14, 18, 18, 18, 26, 26, 26, 32, 32, 32, 42, 42, 42, 18, 18, 18, 0, 0, 0],
    [6, 6, 6, 6, 6, 6, 6, 6, 6, 8, 8, 8, 10, 10, 10, 12, 12, 12, 14, 14, 14, 18, 18, 18, 24, 24, 24, 32, 32, 32, 44, 44, 44, 12, 12, 12, 0, 0, 0],
    [6, 6, 6, 6, 6, 6, 6, 6, 6, 8, 8, 8, 10, 10, 10, 12, 12, 12, 14, 14, 14, 18, 18, 18, 24, 24, 24, 30, 30, 30, 40, 40, 40, 18, 18, 18, 0, 0, 0],
    [4, 4, 4, 4, 4, 4, 6, 6, 4, 4, 4, 6, 6, 6, 8, 8, 8, 10, 10, 10, 12, 12, 12, 14, 14, 14, 18, 18, 18, 22, 22, 22, 30, 30, 30, 56, 56, 56, 0],
    [4, 4, 4, 4, 4, 4, 6, 6, 4, 4, 4, 6, 6, 6, 6, 6, 6, 10, 10, 10, 12, 12, 12, 14, 14, 14, 16, 16, 16, 20, 20, 20, 26, 26, 26, 66, 66, 66, 0],
    [4, 4, 4, 4, 4, 4, 6, 6, 4, 4, 4, 6, 6, 6, 8, 8, 8, 12, 12, 12, 16, 16, 16, 20, 20, 20, 26, 26, 26, 34, 34, 34, 42, 42, 42, 12, 12, 12, 0],
];

/// Band widths (in coding order) of a granule, terminated by 0 or by the end of the slice.
pub fn sfb_widths(h: Header, g: &Granule) -> &'static [u8] {
    let row = h.sfb_row();
    if g.short() {
        if g.mixed { &SFB_MIXED[row] } else { &SFB_SHORT[row] }
    } else {
        &SFB_LONG[row]
    }
}

/// Scalefactor layout of a granule: up to four groups of `count[i]` scalefactors of
/// `size[i]` bits each. `copy[i]` marks groups shared with granule 0 (MPEG-1 scfsi).
#[derive(Clone, Copy, Default, Debug)]
pub struct ScfLayout {
    pub size: [u8; 4],
    pub count: [u8; 4],
    pub copy: [bool; 4],
}

impl ScfLayout {
    pub fn total(&self) -> usize {
        self.count.iter().map(|&c| c as usize).sum()
    }
    pub fn bits(&self) -> u32 {
        (0..4).filter(|&i| !self.copy[i]).map(|i| self.size[i] as u32 * self.count[i] as u32).sum()
    }
}

/// Groups of the scalefactor partitions: [long, mixed, short] × (MPEG-1, LSF 3 + LSF i-stereo 3).
static SCF_PARTITIONS: [[u8; 28]; 3] = [
    [6, 5, 5, 5, 6, 5, 5, 5, 6, 5, 7, 3, 11, 10, 0, 0, 7, 7, 7, 0, 6, 6, 6, 3, 8, 8, 5, 0],
    [8, 9, 6, 12, 6, 9, 9, 9, 6, 9, 12, 6, 15, 18, 0, 0, 6, 15, 12, 0, 6, 12, 9, 6, 6, 18, 9, 0],
    [9, 9, 6, 12, 9, 9, 9, 9, 9, 9, 12, 6, 18, 18, 0, 0, 12, 12, 12, 0, 12, 9, 9, 6, 15, 12, 9, 0],
];

pub fn scf_layout(h: Header, si: &SideInfo, gr: usize, ch: usize) -> ScfLayout {
    let g = &si.gr[gr][ch];
    let kind = if g.short() { if g.mixed { 1 } else { 2 } } else { 0 };
    let part = &SCF_PARTITIONS[kind];
    let mut l = ScfLayout::default();
    if !h.lsf() {
        const SCFC: [u8; 16] = [0, 1, 2, 3, 12, 5, 6, 7, 9, 10, 11, 13, 14, 15, 18, 19];
        let p = SCFC[g.scalefac_compress as usize];
        l.size = [p >> 2, p >> 2, p & 3, p & 3];
        l.count.copy_from_slice(&part[0..4]);
        if gr == 1 && kind == 0 {
            for i in 0..4 {
                l.copy[i] = si.scfsi[ch] & (8 >> i) != 0;
            }
        }
    } else {
        const MOD: [u8; 24] = [5, 5, 4, 4, 5, 5, 4, 1, 4, 3, 1, 1, 5, 6, 6, 1, 4, 4, 4, 1, 4, 3, 1, 1];
        let ist = (h.i_stereo() && ch == 1) as usize;
        let mut sfc = (g.scalefac_compress >> ist) as i32;
        let mut k = ist * 12;
        while sfc >= 0 {
            let mut modprod = 1i32;
            for i in (0..4).rev() {
                l.size[i] = (sfc / modprod % MOD[k + i] as i32) as u8;
                modprod *= MOD[k + i] as i32;
            }
            sfc -= modprod;
            k += 4;
        }
        l.count.copy_from_slice(&part[k..k + 4]);
    }
    l
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_len() {
        // 128 kbps, 44.1 kHz, MPEG-1, no padding: 417 bytes.
        let h = Header(0xfffb9000);
        assert!(h.valid());
        assert_eq!(h.sample_rate(), 44100);
        assert_eq!(h.frame_len(), 417);
        assert_eq!(h.sfb_row(), 5);
        for row in 0..8 {
            assert_eq!(SFB_LONG[row].iter().map(|&w| w as u32).sum::<u32>(), 576);
            assert_eq!(SFB_SHORT[row].iter().map(|&w| w as u32).sum::<u32>(), 576);
            assert_eq!(SFB_MIXED[row].iter().map(|&w| w as u32).sum::<u32>(), 576);
        }
    }
}

/// Pre-emphasis table (MPEG-1 long blocks with preflag).
pub static PRETAB: [u8; 22] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 3, 3, 3, 2, 0];

/// All scalefactors of a granule in coding order (`sfb_widths` order), with the groups
/// shared through scfsi taken from `gr0` (granule 0 of the same channel).
pub fn full_scalefactors(lay: &ScfLayout, coded: &[u8], gr0: &[u8; 39]) -> [u8; 39] {
    let mut out = [0u8; 39];
    let (mut i, mut c) = (0usize, 0usize);
    for k in 0..4 {
        let n = lay.count[k] as usize;
        if lay.copy[k] {
            out[i..i + n].copy_from_slice(&gr0[i..i + n]);
        } else {
            out[i..i + n].copy_from_slice(&coded[c..c + n]);
            c += n;
        }
        i += n;
    }
    out
}

/// Quantizer step exponent of each line, in quarter powers of two (larger = coarser):
/// global_gain - 2 or 4 × (scalefactor + pre-emphasis) - 8 × subblock gain.
pub fn step_exponents(h: Header, g: &Granule, scf: &[u8; 39], out: &mut [i16; 576]) {
    let widths = sfb_widths(h, g);
    let mult = if g.scalefac_scale { 4 } else { 2 };
    let nlong = if !g.short() { 22 } else if g.mixed { if h.lsf() { 6 } else { 8 } } else { 0 };
    let mut line = 0usize;
    for (i, &w) in widths.iter().enumerate() {
        if w == 0 || line >= 576 {
            break;
        }
        let mut sf = scf[i.min(38)] as i32;
        let mut a = g.global_gain as i32;
        if i < nlong {
            if g.preflag && !g.short() {
                sf += PRETAB[i] as i32;
            }
        } else {
            a -= 8 * g.subblock_gain[(i - nlong) % 3] as i32;
        }
        a -= mult * sf;
        let end = (line + w as usize).min(576);
        out[line..end].fill(a as i16);
        line = end;
    }
}
