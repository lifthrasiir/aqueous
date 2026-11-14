//! FSE as libzstd's compressor uses it: the table log it picks, the counts it normalises, the
//! header it writes (RFC 8878, "FSE Table Description") and the tables it encodes by.

use super::bits::Bits;

pub(crate) const MIN_TABLELOG: u32 = 5;
pub(crate) const MAX_TABLELOG: u32 = 12;

#[inline]
pub(crate) fn highbit(v: u32) -> u32 {
    31 - v.leading_zeros()
}

fn min_table_log(total: usize, max_symbol: u32) -> u32 {
    let src = highbit(total as u32) + 1;
    let sym = highbit(max_symbol) + 2;
    src.min(sym)
}

/// The table log for `total` symbols up to `max_symbol`, at most `max_log`; `minus` is 2 for
/// FSE, 1 for Huffman.
pub(crate) fn optimal_table_log(max_log: u32, total: usize, max_symbol: u32, minus: u32) -> u32 {
    let max_bits_src = highbit((total - 1) as u32).wrapping_sub(minus);
    let mut log = if max_log == 0 { 11 } else { max_log };
    let min_bits = min_table_log(total, max_symbol);
    if max_bits_src < log {
        log = max_bits_src;
    }
    if min_bits > log {
        log = min_bits;
    }
    log.clamp(MIN_TABLELOG, MAX_TABLELOG)
}

/// The counts normalised to `1 << log` (-1 for a symbol of low probability, where
/// `low_prob`); None where they cannot be.
pub(crate) fn normalize(count: &[u32], total: usize, log: u32, low_prob: bool) -> Option<Vec<i16>> {
    if log < min_table_log(total, count.len() as u32 - 1) {
        return None;
    }
    const RTB: [u64; 8] = [0, 473195, 504333, 520860, 550000, 700000, 750000, 830000];
    let low: i16 = if low_prob { -1 } else { 1 };
    let scale = 62 - log as u64;
    let step = (1u64 << 62) / total as u64;
    let vstep = 1u64 << (scale - 20);
    let mut still = 1i32 << log;
    let mut norm = vec![0i16; count.len()];
    let mut largest = 0;
    let mut largest_p = 0i16;
    let low_threshold = (total >> log) as u32;
    for (s, &c) in count.iter().enumerate() {
        if c as usize == total {
            return None;
        }
        if c == 0 {
            continue;
        }
        if c <= low_threshold {
            norm[s] = low;
            still -= 1;
        } else {
            let mut p = ((c as u64 * step) >> scale) as i16;
            if p < 8 {
                let rest = vstep * RTB[p as usize];
                p += ((c as u64 * step) - ((p as u64) << scale) > rest) as i16;
            }
            if p > largest_p {
                largest_p = p;
                largest = s;
            }
            norm[s] = p;
            still -= p as i32;
        }
    }
    if -still >= (norm[largest] >> 1) as i32 {
        normalize_m2(&mut norm, log, count, total, low)?;
    } else {
        norm[largest] += still as i16;
    }
    Some(norm)
}

fn normalize_m2(norm: &mut [i16], log: u32, count: &[u32], total: usize, low: i16) -> Option<()> {
    const NOT_YET: i16 = -2;
    let mut total = total as u64;
    let mut distributed = 0u32;
    let low_threshold = (total >> log) as u32;
    let mut low_one = ((total * 3) >> (log + 1)) as u32;
    for (s, &c) in count.iter().enumerate() {
        if c == 0 {
            norm[s] = 0;
        } else if c <= low_threshold {
            norm[s] = low;
            distributed += 1;
            total -= c as u64;
        } else if c <= low_one {
            norm[s] = 1;
            distributed += 1;
            total -= c as u64;
        } else {
            norm[s] = NOT_YET;
        }
    }
    let mut to_distribute = (1u32 << log) - distributed;
    if to_distribute == 0 {
        return Some(());
    }
    if total / to_distribute as u64 > low_one as u64 {
        low_one = ((total * 3) / (to_distribute as u64 * 2)) as u32;
        for (s, &c) in count.iter().enumerate() {
            if norm[s] == NOT_YET && c <= low_one {
                norm[s] = 1;
                distributed += 1;
                total -= c as u64;
            }
        }
        to_distribute = (1u32 << log) - distributed;
    }
    if distributed as usize == count.len() {
        let (mut max_v, mut max_c) = (0, 0);
        for (s, &c) in count.iter().enumerate() {
            if c > max_c {
                max_v = s;
                max_c = c;
            }
        }
        norm[max_v] += to_distribute as i16;
        return Some(());
    }
    if total == 0 {
        let mut s = 0;
        while to_distribute > 0 {
            if norm[s] > 0 {
                to_distribute -= 1;
                norm[s] += 1;
            }
            s = (s + 1) % count.len();
        }
        return Some(());
    }
    let vlog = 62 - log as u64;
    let mid = (1u64 << (vlog - 1)) - 1;
    let rstep = (((1u64 << vlog) * to_distribute as u64) + mid) / total;
    let mut tmp = mid;
    for (s, &c) in count.iter().enumerate() {
        if norm[s] == NOT_YET {
            let end = tmp + c as u64 * rstep;
            let weight = ((end >> vlog) as u32).wrapping_sub((tmp >> vlog) as u32);
            if weight < 1 {
                return None;
            }
            norm[s] = weight as i16;
            tmp = end;
        }
    }
    Some(())
}

/// The table description of `norm` (symbols up to `norm.len() - 1`), appended to `out`.
pub(crate) fn write_ncount(out: &mut Vec<u8>, norm: &[i16], log: u32) -> usize {
    let start = out.len();
    let table_size = 1i32 << log;
    let mut bits: u32 = 0;
    let mut nbits: i32 = 0;
    bits += (log - MIN_TABLELOG) << nbits;
    nbits += 4;
    let mut remaining = table_size + 1;
    let mut threshold = table_size;
    let mut nb = log as i32 + 1;
    let alphabet = norm.len();
    let mut symbol = 0;
    let mut previous0 = false;
    let flush = |out: &mut Vec<u8>, bits: &mut u32, nbits: &mut i32| {
        out.push(*bits as u8);
        out.push((*bits >> 8) as u8);
        *bits >>= 16;
        *nbits -= 16;
    };
    while symbol < alphabet && remaining > 1 {
        if previous0 {
            let mut start = symbol;
            while symbol < alphabet && norm[symbol] == 0 {
                symbol += 1;
            }
            if symbol == alphabet {
                break;
            }
            while symbol >= start + 24 {
                start += 24;
                bits += 0xFFFF << nbits;
                out.push(bits as u8);
                out.push((bits >> 8) as u8);
                bits >>= 16;
            }
            while symbol >= start + 3 {
                start += 3;
                bits += 3 << nbits;
                nbits += 2;
            }
            bits += ((symbol - start) as u32) << nbits;
            nbits += 2;
            if nbits > 16 {
                flush(out, &mut bits, &mut nbits);
            }
        }
        let mut count = norm[symbol] as i32;
        symbol += 1;
        let max = (2 * threshold - 1) - remaining;
        remaining -= count.abs();
        count += 1;
        if count >= threshold {
            count += max;
        }
        bits += (count as u32) << nbits;
        nbits += nb;
        nbits -= (count < max) as i32;
        previous0 = count == 1;
        while remaining < threshold {
            nb -= 1;
            threshold >>= 1;
        }
        if nbits > 16 {
            flush(out, &mut bits, &mut nbits);
        }
    }
    out.push(bits as u8);
    out.push((bits >> 8) as u8);
    out.truncate(out.len() - 2 + ((nbits + 7) / 8) as usize);
    out.len() - start
}

/// The encoding table of a normalised distribution (or of one symbol, log 0).
pub(crate) struct CTable {
    pub log: u32,
    states: Vec<u16>,
    /// (deltaNbBits, deltaFindState) by symbol.
    tt: Vec<(u32, i32)>,
}

impl CTable {
    pub fn new(norm: &[i16], log: u32) -> Self {
        let size = 1usize << log;
        let mask = size - 1;
        let step = (size >> 1) + (size >> 3) + 3;
        let n = norm.len();
        let mut cumul = vec![0u32; n + 1];
        let mut symbols = vec![0u8; size];
        let mut high = size - 1;
        for u in 1..=n {
            if norm[u - 1] == -1 {
                cumul[u] = cumul[u - 1] + 1;
                symbols[high] = (u - 1) as u8;
                high = high.wrapping_sub(1);
            } else {
                cumul[u] = cumul[u - 1] + norm[u - 1] as u32;
            }
        }
        cumul[n] = size as u32 + 1;
        let mut pos = 0;
        for (s, &f) in norm.iter().enumerate() {
            for _ in 0..f.max(0) {
                symbols[pos] = s as u8;
                pos = (pos + step) & mask;
                while pos > high {
                    pos = (pos + step) & mask;
                }
            }
        }
        debug_assert_eq!(pos, 0);
        let mut states = vec![0u16; size];
        for (u, &s) in symbols.iter().enumerate() {
            states[cumul[s as usize] as usize] = (size + u) as u16;
            cumul[s as usize] += 1;
        }
        let mut tt = vec![(0u32, 0i32); n];
        let mut total = 0u32;
        for (s, &f) in norm.iter().enumerate() {
            tt[s] = match f {
                0 => (((log + 1) << 16).wrapping_sub(1 << log), 0),
                -1 | 1 => {
                    let t = ((log << 16).wrapping_sub(1 << log), total as i32 - 1);
                    total += 1;
                    t
                }
                _ => {
                    let max_bits_out = log - highbit(f as u32 - 1);
                    let min_state_plus = (f as u32) << max_bits_out;
                    let t = ((max_bits_out << 16).wrapping_sub(min_state_plus), total as i32 - f as i32);
                    total += f as u32;
                    t
                }
            };
        }
        CTable { log, states, tt }
    }

    /// The table of one symbol: no bits for it.
    pub fn rle(symbol: u8) -> Self {
        let mut tt = vec![(0u32, 0i32); symbol as usize + 1];
        tt[symbol as usize] = (0, 0);
        CTable { log: 0, states: vec![0, 0], tt }
    }

    /// The state starting at `symbol` (the last one encoded, the first decoded).
    #[inline]
    pub fn init(&self, symbol: u8) -> u32 {
        let (dnb, dfs) = self.tt[symbol as usize];
        let nb = (dnb.wrapping_add(1 << 15)) >> 16;
        let v = (nb << 16).wrapping_sub(dnb);
        self.states[((v >> nb) as i32 + dfs) as usize] as u32
    }

    #[inline]
    pub fn encode(&self, bits: &mut Bits, state: &mut u32, symbol: u8) {
        let (dnb, dfs) = self.tt[symbol as usize];
        let nb = (state.wrapping_add(dnb)) >> 16;
        bits.add(*state, nb);
        *state = self.states[((*state >> nb) as i32 + dfs) as usize] as u32;
    }

    #[inline]
    pub fn flush(&self, bits: &mut Bits, state: u32) {
        bits.add(state, self.log);
    }

    /// Whether every symbol up to `max` with a count has a state (zstd's
    /// `HUF_validateCTable` counterpart is in huf).
    #[allow(dead_code)]
    pub fn symbols(&self) -> usize {
        self.tt.len()
    }
}

/// Counts of `src` (symbols below `count.len()`): the largest symbol present and the largest
/// count.
pub(crate) fn histogram(count: &mut [u32], src: &[u8]) -> (u32, u32) {
    count.fill(0);
    for &b in src {
        count[b as usize] += 1;
    }
    let mut max = count.len() - 1;
    while max > 0 && count[max] == 0 {
        max -= 1;
    }
    (max as u32, *count.iter().max().unwrap_or(&0))
}

/// FSE compression of `src` by `ct` (two states), as libzstd's `FSE_compress_usingCTable`:
/// 0 where it is too short.
pub(crate) fn compress_using(out: &mut Vec<u8>, src: &[u8], ct: &CTable) -> usize {
    if src.len() <= 2 {
        return 0;
    }
    let mut bits = Bits::new(out);
    let mut i = src.len();
    let (mut s1, mut s2);
    if src.len() & 1 == 1 {
        i -= 1;
        s1 = ct.init(src[i]);
        i -= 1;
        s2 = ct.init(src[i]);
        i -= 1;
        ct.encode(&mut bits, &mut s1, src[i]);
    } else {
        i -= 1;
        s2 = ct.init(src[i]);
        i -= 1;
        s1 = ct.init(src[i]);
    }
    let n = src.len() - 2;
    if n & 2 != 0 {
        i -= 1;
        ct.encode(&mut bits, &mut s2, src[i]);
        i -= 1;
        ct.encode(&mut bits, &mut s1, src[i]);
    }
    while i > 0 {
        i -= 1;
        ct.encode(&mut bits, &mut s2, src[i]);
        i -= 1;
        ct.encode(&mut bits, &mut s1, src[i]);
        i -= 1;
        ct.encode(&mut bits, &mut s2, src[i]);
        i -= 1;
        ct.encode(&mut bits, &mut s1, src[i]);
    }
    ct.flush(&mut bits, s2);
    ct.flush(&mut bits, s1);
    bits.close()
}
