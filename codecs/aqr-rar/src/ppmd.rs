//! PPMd variant H with RAR's range coder: the model of RAR 3.x's PPMd blocks
//! (docs/rar3-decoding.md §4), decoding and encoding bit for bit.
//!
//! Ported from lzma-sdk's `Ppmd7.c`, `Ppmd7Dec.c` and `Ppmd7Enc.c` (Igor Pavlov, public domain),
//! themselves based on PPMd var.H (Dmitry Shkarin, public domain). The model works in one buffer
//! of `mem` bytes addressed by 32-bit offsets, as lzma-sdk's does without `PPMD_32BIT`, so it
//! behaves the same natively and in Wasm. The range coder is the original carry-less one of
//! PPMd (lzma-sdk's "Ppmd7a"), not 7z's.
//!
//! Layout in the buffer (little-endian): a state is 6 bytes (symbol, freq, successor u32); a
//! context 12 (num_stats u16, summ_freq u16 or the one state's symbol and freq, stats u32 or the
//! one state's successor, suffix u32); a free node 12 (stamp u16, nu u16, next u32).

const MAX_O: usize = 64;
const UNIT_SIZE: u32 = 12;
const MAX_FREQ: u32 = 124;
const INT_BITS: u32 = 7;
const PERIOD_BITS: u32 = 7;
const BIN_SCALE: u32 = 1 << (INT_BITS + PERIOD_BITS);
const NUM_INDEXES: usize = 4 + 4 + 4 + 26;
const EXP_ESCAPE: [u8; 16] = [25, 14, 9, 7, 5, 5, 4, 4, 4, 3, 3, 3, 2, 2, 2, 2];
const INIT_BIN_ESC: [u16; 8] = [0x3CDD, 0x1F3F, 0x59BF, 0x48F3, 0x64A1, 0x5ABC, 0x6632, 0x6051];
/// The SEE contexts are `see[0..400]`; `see[DUMMY]` is the dummy one of 256-symbol contexts.
const DUMMY: usize = 25 * 16;

const MIN_ORDER: u32 = 2;
const MAX_ORDER: u32 = 64;
const MIN_MEM: u32 = 1 << 11;
const MAX_MEM: u32 = 0xFFFF_FFFF - 12 * 3;

const TOP: u32 = 1 << 24;
const BOT: u32 = 1 << 15;

/// The range decoder of PPMd var.H (§4.3) over one block's bytes.
pub struct RangeDecoder<'a> {
    data: &'a [u8],
    pos: usize,
    low: u32,
    range: u32,
    code: u32,
}

impl<'a> RangeDecoder<'a> {
    /// Starts decoding at `data[0]`, reading the first 4 bytes.
    pub fn new(data: &'a [u8]) -> RangeDecoder<'a> {
        let mut rc = RangeDecoder { data, pos: 0, low: 0, range: 0xFFFF_FFFF, code: 0 };
        for _ in 0..4 {
            rc.code = rc.code << 8 | rc.byte() as u32;
        }
        rc
    }

    #[inline]
    fn byte(&mut self) -> u8 {
        let b = self.data.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        b
    }

    /// The bytes read so far, those past the end of the data (read as 0) included.
    pub fn consumed(&self) -> usize {
        self.pos
    }

    /// Whether bytes past the end of the data were read.
    pub fn overrun(&self) -> bool {
        self.pos > self.data.len()
    }

    /// GetThreshold: None if the range has run out or the value is not below `total`.
    #[inline]
    fn threshold(&mut self, total: u32) -> Option<u32> {
        self.range /= total;
        if self.range == 0 {
            return None;
        }
        let v = self.code.wrapping_sub(self.low) / self.range;
        if v < total { Some(v) } else { None }
    }

    #[inline]
    fn decode(&mut self, start: u32, size: u32) {
        self.low = self.low.wrapping_add(start.wrapping_mul(self.range));
        self.range = self.range.wrapping_mul(size);
        loop {
            if self.low ^ self.low.wrapping_add(self.range) >= TOP {
                if self.range >= BOT {
                    break;
                }
                self.range = self.low.wrapping_neg() & (BOT - 1);
            }
            self.code = self.code << 8 | self.byte() as u32;
            self.range <<= 8;
            self.low <<= 8;
        }
    }
}

/// The range encoder matching [`RangeDecoder`].
pub struct RangeEncoder {
    low: u32,
    range: u32,
    pub out: Vec<u8>,
}

impl Default for RangeEncoder {
    fn default() -> Self {
        RangeEncoder::new()
    }
}

impl RangeEncoder {
    pub fn new() -> RangeEncoder {
        RangeEncoder::with_output(Vec::new())
    }

    /// Appends to `out`.
    pub fn with_output(out: Vec<u8>) -> RangeEncoder {
        RangeEncoder { low: 0, range: 0xFFFF_FFFF, out }
    }

    #[inline]
    fn encode(&mut self, start: u32, size: u32, total: u32) {
        self.range /= total;
        self.low = self.low.wrapping_add(start.wrapping_mul(self.range));
        self.range = self.range.wrapping_mul(size);
        loop {
            if self.low ^ self.low.wrapping_add(self.range) >= TOP {
                if self.range >= BOT {
                    break;
                }
                self.range = self.low.wrapping_neg() & (BOT - 1);
            }
            self.out.push((self.low >> 24) as u8);
            self.range <<= 8;
            self.low <<= 8;
        }
    }

    /// Writes the 4 bytes of Low (§4.5) and returns the output.
    pub fn flush(mut self) -> Vec<u8> {
        self.out.extend_from_slice(&self.low.to_be_bytes());
        self.out
    }
}

#[derive(Clone, Copy, Default)]
struct See {
    summ: u16,
    shift: u8,
    count: u8,
}

impl See {
    #[inline]
    fn update(&mut self) {
        if (self.shift as u32) < PERIOD_BITS {
            self.count = self.count.wrapping_sub(1);
            if self.count == 0 {
                self.summ = self.summ.wrapping_shl(1);
                self.count = (3u32 << self.shift) as u8;
                self.shift += 1;
            }
        }
    }
}

#[inline]
fn hi_bits_3(sym: u32) -> u32 {
    ((sym + 0xC0) >> (8 - 3)) & (1 << 3)
}

#[inline]
fn hi_bits_4(sym: u32) -> u32 {
    ((sym + 0xC0) >> (8 - 4)) & (1 << 4)
}

/// A PPMd var.H model (lzma-sdk's `CPpmd7`).
pub struct Model {
    mem: Vec<u8>,
    size: u32,
    min_context: u32,
    max_context: u32,
    found_state: u32,
    order_fall: u32,
    init_esc: u32,
    prev_success: u32,
    max_order: u32,
    hi_bits_flag: u32,
    run_length: i32,
    init_rl: i32,
    glue_count: u32,
    text: u32,
    units_start: u32,
    lo_unit: u32,
    hi_unit: u32,
    indx2units: [u8; NUM_INDEXES],
    units2indx: [u8; 128],
    free_list: [u32; NUM_INDEXES],
    ns2indx: [u8; 256],
    ns2bs_indx: [u8; 256],
    see: [See; DUMMY + 1],
    bin_summ: [[u16; 64]; 128],
}

impl Model {
    /// A model over `mem` bytes (Ppmd7_Construct and Ppmd7_Alloc), initialised with order 2;
    /// None if `mem` is out of PPMd's bounds or cannot be allocated.
    pub fn new(mem: u32) -> Option<Model> {
        if !(MIN_MEM..=MAX_MEM).contains(&mem) {
            return None;
        }
        let mut buf = Vec::new();
        buf.try_reserve_exact(mem as usize).ok()?;
        buf.resize(mem as usize, 0);
        let mut m = Model {
            mem: buf,
            size: mem,
            min_context: 0,
            max_context: 0,
            found_state: 0,
            order_fall: 0,
            init_esc: 0,
            prev_success: 0,
            max_order: 0,
            hi_bits_flag: 0,
            run_length: 0,
            init_rl: 0,
            glue_count: 0,
            text: 0,
            units_start: 0,
            lo_unit: 0,
            hi_unit: 0,
            indx2units: [0; NUM_INDEXES],
            units2indx: [0; 128],
            free_list: [0; NUM_INDEXES],
            ns2indx: [0; 256],
            ns2bs_indx: [0; 256],
            see: [See::default(); DUMMY + 1],
            bin_summ: [[0; 64]; 128],
        };
        let mut k = 0;
        for i in 0..NUM_INDEXES {
            let step = if i >= 12 { 4 } else { (i >> 2) + 1 };
            for _ in 0..step {
                m.units2indx[k] = i as u8;
                k += 1;
            }
            m.indx2units[i] = k as u8;
        }
        m.ns2bs_indx[0] = 0;
        m.ns2bs_indx[1] = 2;
        m.ns2bs_indx[2..11].fill(4);
        m.ns2bs_indx[11..].fill(6);
        for i in 0..3 {
            m.ns2indx[i] = i as u8;
        }
        let (mut mm, mut k) = (3u32, 1u32);
        for i in 3..256 {
            m.ns2indx[i] = mm as u8;
            k -= 1;
            if k == 0 {
                mm += 1;
                k = mm - 2;
            }
        }
        m.init(MIN_ORDER);
        Some(m)
    }

    /// The most bytes of memory the model holds: the size that [`Model::set_size`] may set.
    pub fn capacity(&self) -> u32 {
        self.mem.len() as u32
    }

    /// Uses `size` bytes of the memory held, no more than [`Model::capacity`] (the model is to
    /// be started afresh by [`Model::init`] after). False where it does not fit.
    pub fn set_size(&mut self, size: u32) -> bool {
        if size > self.capacity() || size < MIN_MEM {
            return false;
        }
        self.size = size;
        true
    }

    /// A fresh model of `order` (Ppmd7_Init), clamped to 2..=64.
    pub fn init(&mut self, order: u32) {
        self.max_order = order.clamp(MIN_ORDER, MAX_ORDER);
        self.restart_model();
    }

    // ---- Memory access ----

    #[inline]
    fn u16_at(&self, o: u32) -> u32 {
        let o = o as usize;
        u16::from_le_bytes(self.mem[o..o + 2].try_into().unwrap()) as u32
    }

    #[inline]
    fn set_u16(&mut self, o: u32, v: u32) {
        let o = o as usize;
        self.mem[o..o + 2].copy_from_slice(&(v as u16).to_le_bytes());
    }

    #[inline]
    fn u32_at(&self, o: u32) -> u32 {
        let o = o as usize;
        u32::from_le_bytes(self.mem[o..o + 4].try_into().unwrap())
    }

    #[inline]
    fn set_u32(&mut self, o: u32, v: u32) {
        let o = o as usize;
        self.mem[o..o + 4].copy_from_slice(&v.to_le_bytes());
    }

    // Contexts.
    #[inline]
    fn num_stats(&self, c: u32) -> u32 {
        self.u16_at(c)
    }
    #[inline]
    fn summ_freq(&self, c: u32) -> u32 {
        self.u16_at(c + 2)
    }
    #[inline]
    fn set_summ_freq(&mut self, c: u32, v: u32) {
        self.set_u16(c + 2, v)
    }
    #[inline]
    fn stats(&self, c: u32) -> u32 {
        self.u32_at(c + 4)
    }
    #[inline]
    fn suffix(&self, c: u32) -> u32 {
        self.u32_at(c + 8)
    }

    // States.
    #[inline]
    fn sym(&self, s: u32) -> u32 {
        self.mem[s as usize] as u32
    }
    #[inline]
    fn freq(&self, s: u32) -> u32 {
        self.mem[s as usize + 1] as u32
    }
    #[inline]
    fn set_freq(&mut self, s: u32, v: u32) {
        self.mem[s as usize + 1] = v as u8;
    }
    #[inline]
    fn successor(&self, s: u32) -> u32 {
        self.u32_at(s + 2)
    }
    #[inline]
    fn set_successor(&mut self, s: u32, v: u32) {
        self.set_u32(s + 2, v)
    }
    #[inline]
    fn swap_states(&mut self, a: u32, b: u32) {
        for k in 0..6 {
            self.mem.swap((a + k) as usize, (b + k) as usize);
        }
    }
    #[inline]
    fn read_state(&self, s: u32) -> [u8; 6] {
        let s = s as usize;
        self.mem[s..s + 6].try_into().unwrap()
    }
    #[inline]
    fn write_state(&mut self, s: u32, v: [u8; 6]) {
        let s = s as usize;
        self.mem[s..s + 6].copy_from_slice(&v);
    }

    #[inline]
    fn i2u(&self, indx: usize) -> u32 {
        self.indx2units[indx] as u32
    }
    #[inline]
    fn u2i(&self, nu: u32) -> usize {
        self.units2indx[nu as usize - 1] as usize
    }

    // ---- Sub-allocator ----

    fn insert_node(&mut self, node: u32, indx: usize) {
        self.set_u32(node, self.free_list[indx]);
        self.free_list[indx] = node;
    }

    fn remove_node(&mut self, indx: usize) -> u32 {
        let node = self.free_list[indx];
        self.free_list[indx] = self.u32_at(node);
        node
    }

    fn split_block(&mut self, ptr: u32, old_indx: usize, new_indx: usize) {
        let nu = self.i2u(old_indx) - self.i2u(new_indx);
        let ptr = ptr + self.i2u(new_indx) * UNIT_SIZE;
        let mut i = self.u2i(nu);
        if self.i2u(i) != nu {
            i -= 1;
            let k = self.i2u(i);
            self.insert_node(ptr + k * UNIT_SIZE, (nu - k - 1) as usize);
        }
        self.insert_node(ptr, i);
    }

    fn glue_free_blocks(&mut self) {
        self.glue_count = 255;
        if self.lo_unit != self.hi_unit {
            self.set_u16(self.lo_unit, 1);
        }
        // The list of free blocks, each with stamp 0, its units and the next one.
        let mut n = 0;
        for i in 0..NUM_INDEXES {
            let nu = self.i2u(i);
            let mut next = self.free_list[i];
            self.free_list[i] = 0;
            while next != 0 {
                let tmp = next;
                next = self.u32_at(tmp);
                self.set_u16(tmp, 0);
                self.set_u16(tmp + 2, nu);
                self.set_u32(tmp + 4, n);
                n = tmp;
            }
        }
        let mut head = n;
        // Glue neighbours; `prev` is where the link to the current node is (0: `head`).
        let mut prev = 0u32;
        while n != 0 {
            let node = n;
            let mut nu = self.u16_at(node + 2);
            n = self.u32_at(node + 4);
            if nu == 0 {
                if prev == 0 {
                    head = n;
                } else {
                    self.set_u32(prev, n);
                }
                continue;
            }
            prev = node + 4;
            loop {
                let node2 = node + nu * UNIT_SIZE;
                nu += self.u16_at(node2 + 2);
                if self.u16_at(node2) != 0 || nu >= 0x10000 {
                    break;
                }
                self.set_u16(node + 2, nu);
                self.set_u16(node2 + 2, 0);
            }
        }
        // Fill the free lists, walking in the same direction.
        n = head;
        while n != 0 {
            let mut node = n;
            let mut nu = self.u16_at(node + 2);
            n = self.u32_at(node + 4);
            if nu == 0 {
                continue;
            }
            while nu > 128 {
                self.insert_node(node, NUM_INDEXES - 1);
                nu -= 128;
                node += 128 * UNIT_SIZE;
            }
            let mut i = self.u2i(nu);
            if self.i2u(i) != nu {
                i -= 1;
                let k = self.i2u(i);
                self.insert_node(node + k * UNIT_SIZE, (nu - k - 1) as usize);
            }
            self.insert_node(node, i);
        }
    }

    #[cold]
    fn alloc_units_rare(&mut self, indx: usize) -> u32 {
        if self.glue_count == 0 {
            self.glue_free_blocks();
            if self.free_list[indx] != 0 {
                return self.remove_node(indx);
            }
        }
        let mut i = indx;
        loop {
            i += 1;
            if i == NUM_INDEXES {
                let num_bytes = self.i2u(indx) * UNIT_SIZE;
                let us = self.units_start;
                self.glue_count = self.glue_count.wrapping_sub(1);
                return if us - self.text > num_bytes {
                    self.units_start = us - num_bytes;
                    self.units_start
                } else {
                    0
                };
            }
            if self.free_list[i] != 0 {
                break;
            }
        }
        let block = self.remove_node(i);
        self.split_block(block, i, indx);
        block
    }

    #[inline]
    fn alloc_units(&mut self, indx: usize) -> u32 {
        if self.free_list[indx] != 0 {
            return self.remove_node(indx);
        }
        let num_bytes = self.i2u(indx) * UNIT_SIZE;
        let lo = self.lo_unit;
        if self.hi_unit - lo >= num_bytes {
            self.lo_unit = lo + num_bytes;
            return lo;
        }
        self.alloc_units_rare(indx)
    }

    fn copy_units(&mut self, dest: u32, src: u32, nu: u32) {
        let src = src as usize;
        self.mem.copy_within(src..src + (nu * UNIT_SIZE) as usize, dest as usize);
    }

    // ---- Model ----

    #[cold]
    fn restart_model(&mut self) {
        self.free_list = [0; NUM_INDEXES];
        self.text = 0;
        self.hi_unit = self.text + self.size;
        self.lo_unit = self.hi_unit - self.size / 8 / UNIT_SIZE * 7 * UNIT_SIZE;
        self.units_start = self.lo_unit;
        self.glue_count = 0;

        self.order_fall = self.max_order;
        self.init_rl = -(self.max_order.min(12) as i32) - 1;
        self.run_length = self.init_rl;
        self.prev_success = 0;

        self.hi_unit -= UNIT_SIZE;
        let mc = self.hi_unit;
        let s = self.lo_unit;
        self.lo_unit += 256 / 2 * UNIT_SIZE;
        self.max_context = mc;
        self.min_context = mc;
        self.found_state = s;
        self.set_u16(mc, 256);
        self.set_summ_freq(mc, 257);
        self.set_u32(mc + 4, s);
        self.set_u32(mc + 8, 0);
        for i in 0..256 {
            self.write_state(s + i * 6, [i as u8, 1, 0, 0, 0, 0]);
        }

        for i in 0..128 {
            for (k, &esc) in INIT_BIN_ESC.iter().enumerate() {
                let val = (BIN_SCALE - esc as u32 / (i as u32 + 2)) as u16;
                for m in (0..64).step_by(8) {
                    self.bin_summ[i][k + m] = val;
                }
            }
        }
        for i in 0..25 {
            let summ = ((5 * i as u32 + 10) << (PERIOD_BITS - 4)) as u16;
            for k in 0..16 {
                self.see[i * 16 + k] = See { summ, shift: (PERIOD_BITS - 4) as u8, count: 4 };
            }
        }
        self.see[DUMMY] = See { summ: 0, shift: PERIOD_BITS as u8, count: 64 };
    }

    /// Returns the new context, or 0 if memory ran out.
    fn create_successors(&mut self) -> u32 {
        let mut c = self.min_context;
        let mut up_branch = self.successor(self.found_state);
        let mut ps = [0u32; MAX_O];
        let mut num_ps = 0;
        if self.order_fall != 0 {
            ps[num_ps] = self.found_state;
            num_ps += 1;
        }
        let fsym = self.sym(self.found_state);
        while self.suffix(c) != 0 {
            c = self.suffix(c);
            let mut s;
            if self.num_stats(c) != 1 {
                s = self.stats(c);
                while self.sym(s) != fsym {
                    s += 6;
                }
            } else {
                s = c + 2;
            }
            let successor = self.successor(s);
            if successor != up_branch {
                c = successor;
                if num_ps == 0 {
                    return c;
                }
                break;
            }
            ps[num_ps] = s;
            num_ps += 1;
        }

        let new_sym = self.mem[up_branch as usize];
        up_branch += 1;
        let new_freq = if self.num_stats(c) == 1 {
            self.freq(c + 2)
        } else {
            let mut s = self.stats(c);
            while self.sym(s) != new_sym as u32 {
                s += 6;
            }
            let cf = self.freq(s) - 1;
            let s0 = self.summ_freq(c).wrapping_sub(self.num_stats(c)).wrapping_sub(cf);
            1 + if 2 * cf <= s0 { (5 * cf > s0) as u32 } else { (2 * cf + s0 - 1) / (2 * s0) + 1 }
        };

        loop {
            let c1;
            if self.hi_unit != self.lo_unit {
                self.hi_unit -= UNIT_SIZE;
                c1 = self.hi_unit;
            } else if self.free_list[0] != 0 {
                c1 = self.remove_node(0);
            } else {
                c1 = self.alloc_units_rare(0);
                if c1 == 0 {
                    return 0;
                }
            }
            self.set_u16(c1, 1);
            self.mem[c1 as usize + 2] = new_sym;
            self.mem[c1 as usize + 3] = new_freq as u8;
            self.set_successor(c1 + 2, up_branch);
            self.set_u32(c1 + 8, c);
            num_ps -= 1;
            self.set_successor(ps[num_ps], c1);
            c = c1;
            if num_ps == 0 {
                break;
            }
        }
        c
    }

    fn update_model(&mut self) {
        let fs = self.found_state;
        let fsym = self.sym(fs);
        let ffreq = self.freq(fs);
        if ffreq < MAX_FREQ / 4 && self.suffix(self.min_context) != 0 {
            let c = self.suffix(self.min_context);
            if self.num_stats(c) == 1 {
                let s = c + 2;
                if self.freq(s) < 32 {
                    self.set_freq(s, self.freq(s) + 1);
                }
            } else {
                let mut s = self.stats(c);
                if self.sym(s) != fsym {
                    loop {
                        s += 6;
                        if self.sym(s) == fsym {
                            break;
                        }
                    }
                    if self.freq(s) >= self.freq(s - 6) {
                        self.swap_states(s, s - 6);
                        s -= 6;
                    }
                }
                if self.freq(s) < MAX_FREQ - 9 {
                    self.set_freq(s, self.freq(s) + 2);
                    self.set_summ_freq(c, self.summ_freq(c) + 2);
                }
            }
        }

        if self.order_fall == 0 {
            let c = self.create_successors();
            self.min_context = c;
            self.max_context = c;
            if c == 0 {
                self.restart_model();
                return;
            }
            self.set_successor(self.found_state, c);
            return;
        }

        self.mem[self.text as usize] = fsym as u8;
        self.text += 1;
        if self.text >= self.units_start {
            self.restart_model();
            return;
        }
        let mut max_successor = self.text;
        let mut min_successor = self.successor(fs);

        if min_successor != 0 {
            if min_successor <= max_successor {
                let cs = self.create_successors();
                if cs == 0 {
                    self.restart_model();
                    return;
                }
                min_successor = cs;
            }
            self.order_fall -= 1;
            if self.order_fall == 0 {
                max_successor = min_successor;
                self.text -= (self.max_context != self.min_context) as u32;
            }
        } else {
            self.set_successor(fs, max_successor);
            min_successor = self.min_context;
        }

        let mc = self.min_context;
        let mut c = self.max_context;
        self.max_context = min_successor;
        self.min_context = min_successor;
        if c == mc {
            return;
        }

        let ns = self.num_stats(mc);
        let s0 = self.summ_freq(mc).wrapping_sub(ns).wrapping_sub(ffreq - 1);

        loop {
            let ns1 = self.num_stats(c);
            let mut sum;
            if ns1 != 1 {
                if ns1 & 1 == 0 {
                    let old_nu = ns1 >> 1;
                    let i = self.u2i(old_nu);
                    if i != self.u2i(old_nu + 1) {
                        let ptr = self.alloc_units(i + 1);
                        if ptr == 0 {
                            self.restart_model();
                            return;
                        }
                        let old_ptr = self.stats(c);
                        self.copy_units(ptr, old_ptr, old_nu);
                        self.insert_node(old_ptr, i);
                        self.set_u32(c + 4, ptr);
                    }
                }
                sum = self.summ_freq(c);
                sum += (2 * ns1 < ns) as u32 + 2 * ((4 * ns1 <= ns) as u32 & (sum <= 8 * ns1) as u32);
            } else {
                let s = self.alloc_units(0);
                if s == 0 {
                    self.restart_model();
                    return;
                }
                let mut freq = self.mem[c as usize + 3] as u32;
                self.mem[s as usize] = self.mem[c as usize + 2];
                let succ = self.u32_at(c + 4);
                self.set_successor(s, succ);
                self.set_u32(c + 4, s);
                if freq < MAX_FREQ / 4 - 1 {
                    freq <<= 1;
                } else {
                    freq = MAX_FREQ - 4;
                }
                self.set_freq(s, freq);
                sum = freq + self.init_esc + (ns > 3) as u32;
            }

            let s = self.stats(c) + ns1 * 6;
            let mut cf = 2 * (sum + 6) * ffreq;
            let sf = s0.wrapping_add(sum);
            self.mem[s as usize] = fsym as u8;
            self.set_u16(c, ns1 + 1);
            self.set_successor(s, max_successor);
            if cf < sf.wrapping_mul(6) {
                cf = 1 + (cf > sf) as u32 + (cf >= sf.wrapping_mul(4)) as u32;
                sum += 3;
            } else {
                cf = 4
                    + (cf >= sf.wrapping_mul(9)) as u32
                    + (cf >= sf.wrapping_mul(12)) as u32
                    + (cf >= sf.wrapping_mul(15)) as u32;
                sum += cf;
            }
            self.set_summ_freq(c, sum);
            self.set_freq(s, cf);
            c = self.suffix(c);
            if c == mc {
                break;
            }
        }
    }

    fn rescale(&mut self) {
        let mc = self.min_context;
        let stats = self.stats(mc);
        let mut s = self.found_state;
        if s != stats {
            let tmp = self.read_state(s);
            self.mem.copy_within(stats as usize..s as usize, stats as usize + 6);
            self.write_state(stats, tmp);
            s = stats;
        }
        let mut sum_freq = self.freq(s);
        let mut esc_freq = self.summ_freq(mc).wrapping_sub(sum_freq);
        let adder = (self.order_fall != 0) as u32;
        sum_freq = (sum_freq + 4 + adder) >> 1;
        let mut i = self.num_stats(mc) - 1;
        self.set_freq(s, sum_freq);
        loop {
            s += 6;
            let mut freq = self.freq(s);
            esc_freq = esc_freq.wrapping_sub(freq);
            freq = (freq + adder) >> 1;
            sum_freq += freq;
            self.set_freq(s, freq);
            if freq > self.freq(s - 6) {
                let tmp = self.read_state(s);
                let mut s1 = s;
                loop {
                    s1 -= 6;
                    if s1 == stats || freq <= self.freq(s1 - 6) {
                        break;
                    }
                }
                self.mem.copy_within(s1 as usize..s as usize, s1 as usize + 6);
                self.write_state(s1, tmp);
            }
            i -= 1;
            if i == 0 {
                break;
            }
        }

        if self.freq(s) == 0 {
            let mut i = 0;
            loop {
                i += 1;
                s -= 6;
                if self.freq(s) != 0 {
                    break;
                }
            }
            esc_freq = esc_freq.wrapping_add(i);
            let num_stats = self.num_stats(mc);
            let num_stats_new = num_stats - i;
            self.set_u16(mc, num_stats_new);
            let n0 = (num_stats + 1) >> 1;
            if num_stats_new == 1 {
                let mut freq = self.freq(stats);
                loop {
                    esc_freq >>= 1;
                    freq = (freq + 1) >> 1;
                    if esc_freq <= 1 {
                        break;
                    }
                }
                let mut st = self.read_state(stats);
                st[1] = freq as u8;
                self.write_state(mc + 2, st);
                self.found_state = mc + 2;
                let i0 = self.u2i(n0);
                self.insert_node(stats, i0);
                return;
            }
            let n1 = (num_stats_new + 1) >> 1;
            if n0 != n1 {
                let i0 = self.u2i(n0);
                let i1 = self.u2i(n1);
                if i0 != i1 {
                    if self.free_list[i1] != 0 {
                        let ptr = self.remove_node(i1);
                        self.set_u32(mc + 4, ptr);
                        self.copy_units(ptr, stats, n1);
                        self.insert_node(stats, i0);
                    } else {
                        self.split_block(stats, i0, i1);
                    }
                }
            }
        }
        self.set_summ_freq(mc, sum_freq + esc_freq - (esc_freq >> 1));
        self.found_state = self.stats(mc);
    }

    /// Ppmd7_MakeEscFreq: the SEE context's index and the escape frequency.
    #[inline]
    fn make_esc_freq(&mut self, num_masked: u32) -> (usize, u32) {
        let mc = self.min_context;
        let num_stats = self.num_stats(mc);
        if num_stats != 256 {
            let non_masked = num_stats - num_masked;
            let idx = self.ns2indx[non_masked as usize - 1] as usize * 16
                + (non_masked < self.num_stats(self.suffix(mc)).wrapping_sub(num_stats)) as usize
                + 2 * (self.summ_freq(mc) < 11 * num_stats) as usize
                + 4 * (num_masked > non_masked) as usize
                + self.hi_bits_flag as usize;
            let see = &mut self.see[idx];
            let summ = see.summ as u32;
            let r = summ >> see.shift;
            see.summ = (summ - r) as u16;
            (idx, r + (r == 0) as u32)
        } else {
            (DUMMY, 1)
        }
    }

    #[inline]
    fn next_context(&mut self) {
        let c = self.successor(self.found_state);
        if self.order_fall == 0 && c > self.text {
            self.min_context = c;
            self.max_context = c;
        } else {
            self.update_model();
        }
    }

    fn update1(&mut self) {
        let mut s = self.found_state;
        let freq = self.freq(s) + 4;
        let mc = self.min_context;
        self.set_summ_freq(mc, self.summ_freq(mc) + 4);
        self.set_freq(s, freq);
        if freq > self.freq(s - 6) {
            self.swap_states(s, s - 6);
            s -= 6;
            self.found_state = s;
            if freq > MAX_FREQ {
                self.rescale();
            }
        }
        self.next_context();
    }

    fn update1_0(&mut self) {
        let s = self.found_state;
        let mc = self.min_context;
        let freq = self.freq(s);
        let summ_freq = self.summ_freq(mc);
        self.prev_success = (2 * freq > summ_freq) as u32;
        self.run_length += self.prev_success as i32;
        self.set_summ_freq(mc, summ_freq + 4);
        self.set_freq(s, freq + 4);
        if freq + 4 > MAX_FREQ {
            self.rescale();
        }
        self.next_context();
    }

    fn update2(&mut self) {
        let s = self.found_state;
        let freq = self.freq(s) + 4;
        self.run_length = self.init_rl;
        let mc = self.min_context;
        self.set_summ_freq(mc, self.summ_freq(mc) + 4);
        self.set_freq(s, freq);
        if freq > MAX_FREQ {
            self.rescale();
        }
        self.update_model();
    }

    /// The binary context's BinSumm entry (Ppmd7_GetBinSumm), setting HiBitsFlag.
    #[inline]
    fn bin_summ_index(&mut self) -> (usize, usize) {
        let mc = self.min_context;
        let one = mc + 2;
        self.hi_bits_flag = hi_bits_3(self.sym(self.found_state));
        let i = self.freq(one) as usize - 1;
        let j = self.prev_success
            + ((self.run_length >> 26) & 0x20) as u32
            + self.ns2bs_indx[self.num_stats(self.suffix(mc)) as usize - 1] as u32
            + hi_bits_4(self.sym(one))
            + self.hi_bits_flag;
        (i, j as usize)
    }

    /// After the binary context's symbol (UpdateBin and NextContext).
    #[inline]
    fn update_bin(&mut self, s: u32) {
        let freq = self.freq(s);
        self.found_state = s;
        self.prev_success = 1;
        self.run_length += 1;
        self.set_freq(s, freq + (freq < 128) as u32);
        self.next_context();
    }

    /// Decodes a symbol; None where the data is not a valid stream (an escape from the
    /// order −1 context, a value out of range).
    pub fn decode(&mut self, rc: &mut RangeDecoder) -> Option<u8> {
        let mut mask;
        let mc = self.min_context;
        let ns = self.num_stats(mc);
        if ns != 1 {
            let s = self.stats(mc);
            let summ_freq = self.summ_freq(mc);
            let count = rc.threshold(summ_freq)?;
            let st = &self.mem[s as usize..(s + 6 * ns) as usize];
            let mut hi = st[1] as u32;
            if count < hi {
                rc.decode(0, hi);
                self.found_state = s;
                let sym = st[0];
                self.update1_0();
                return Some(sym);
            }
            self.prev_success = 0;
            let mut hit = None;
            for (k, e) in st.as_chunks::<6>().0.iter().enumerate().skip(1) {
                let f = e[1] as u32;
                if count < hi + f {
                    hit = Some((k as u32, f, e[0]));
                    break;
                }
                hi += f;
            }
            if let Some((k, f, sym)) = hit {
                rc.decode(hi, f);
                self.found_state = s + 6 * k;
                self.update1();
                return Some(sym);
            }
            rc.decode(hi, summ_freq - hi);
            mask = [0xFFu8; 256];
            for e in st.as_chunks::<6>().0.iter() {
                mask[e[0] as usize] = 0;
            }
            self.hi_bits_flag = hi_bits_3(self.sym(self.found_state));
        } else {
            let (i, j) = self.bin_summ_index();
            let pr = self.bin_summ[i][j] as u32;
            let v = rc.threshold(BIN_SCALE)?;
            let pr1 = pr - ((pr + (1 << (PERIOD_BITS - 2))) >> PERIOD_BITS);
            let s = mc + 2;
            if v < pr {
                self.bin_summ[i][j] = (pr1 + (1 << INT_BITS)) as u16;
                rc.decode(0, pr);
                let sym = self.sym(s) as u8;
                self.update_bin(s);
                return Some(sym);
            }
            self.bin_summ[i][j] = pr1 as u16;
            self.init_esc = EXP_ESCAPE[(pr1 >> 10) as usize] as u32;
            rc.decode(pr, BIN_SCALE - pr);
            mask = [0xFFu8; 256];
            mask[self.sym(s) as usize] = 0;
            self.prev_success = 0;
        }

        loop {
            let mut mc = self.min_context;
            let num_masked = self.num_stats(mc);
            loop {
                self.order_fall += 1;
                mc = self.suffix(mc);
                if mc == 0 {
                    return None;
                }
                if self.num_stats(mc) != num_masked {
                    break;
                }
            }
            self.min_context = mc;
            let stats = self.stats(mc);
            let ns = self.num_stats(mc);
            let st = &self.mem[stats as usize..(stats + 6 * ns) as usize];
            let mut hi_cnt = 0;
            for e in st.as_chunks::<6>().0.iter() {
                hi_cnt += e[1] as u32 & mask[e[0] as usize] as u32;
            }
            let (see, esc) = self.make_esc_freq(num_masked);
            let freq_sum = esc + hi_cnt;
            let count = rc.threshold(freq_sum)?;
            let st = &self.mem[stats as usize..(stats + 6 * ns) as usize];
            if count < hi_cnt {
                let mut lo = 0;
                let mut k = 0;
                for e in st.as_chunks::<6>().0.iter() {
                    let f = e[1] as u32 & mask[e[0] as usize] as u32;
                    if count < lo + f {
                        rc.decode(lo, f);
                        break;
                    }
                    lo += f;
                    k += 1;
                }
                let s = stats + 6 * k;
                self.see[see].update();
                self.found_state = s;
                let sym = self.sym(s) as u8;
                self.update2();
                return Some(sym);
            }
            rc.decode(hi_cnt, freq_sum - hi_cnt);
            for e in st.as_chunks::<6>().0.iter() {
                mask[e[0] as usize] = 0;
            }
            let see = &mut self.see[see];
            see.summ = see.summ.wrapping_add(freq_sum as u16);
        }
    }

    /// Encodes `sym`, the exact inverse of [`Model::decode`].
    pub fn encode(&mut self, rc: &mut RangeEncoder, sym: u8) {
        let symbol = sym as u32;
        let mut mask;
        let mc = self.min_context;
        let ns = self.num_stats(mc);
        if ns != 1 {
            let s = self.stats(mc);
            let summ_freq = self.summ_freq(mc);
            if self.sym(s) == symbol {
                rc.encode(0, self.freq(s), summ_freq);
                self.found_state = s;
                self.update1_0();
                return;
            }
            self.prev_success = 0;
            let st = &self.mem[s as usize..(s + 6 * ns) as usize];
            let mut sum = st[1] as u32;
            let mut hit = None;
            for (k, e) in st.as_chunks::<6>().0.iter().enumerate().skip(1) {
                if e[0] as u32 == symbol {
                    hit = Some((k as u32, e[1] as u32));
                    break;
                }
                sum += e[1] as u32;
            }
            if let Some((k, f)) = hit {
                rc.encode(sum, f, summ_freq);
                self.found_state = s + 6 * k;
                self.update1();
                return;
            }
            rc.encode(sum, summ_freq - sum, summ_freq);
            mask = [0xFFu8; 256];
            for e in st.as_chunks::<6>().0.iter() {
                mask[e[0] as usize] = 0;
            }
            self.hi_bits_flag = hi_bits_3(self.sym(self.found_state));
        } else {
            let (i, j) = self.bin_summ_index();
            let pr = self.bin_summ[i][j] as u32;
            let pr1 = pr - ((pr + (1 << (PERIOD_BITS - 2))) >> PERIOD_BITS);
            let s = mc + 2;
            if self.sym(s) == symbol {
                self.bin_summ[i][j] = (pr1 + (1 << INT_BITS)) as u16;
                rc.encode(0, pr, BIN_SCALE);
                self.update_bin(s);
                return;
            }
            self.bin_summ[i][j] = pr1 as u16;
            self.init_esc = EXP_ESCAPE[(pr1 >> 10) as usize] as u32;
            rc.encode(pr, BIN_SCALE - pr, BIN_SCALE);
            mask = [0xFFu8; 256];
            mask[self.sym(s) as usize] = 0;
            self.prev_success = 0;
        }

        loop {
            let mut mc = self.min_context;
            let num_masked = self.num_stats(mc);
            loop {
                self.order_fall += 1;
                mc = self.suffix(mc);
                if mc == 0 {
                    // Unreachable: the order 0 context has every symbol.
                    return;
                }
                if self.num_stats(mc) != num_masked {
                    break;
                }
            }
            self.min_context = mc;
            let stats = self.stats(mc);
            let ns = self.num_stats(mc);
            let (see, esc) = self.make_esc_freq(num_masked);
            let st = &self.mem[stats as usize..(stats + 6 * ns) as usize];
            let mut lo = 0;
            let mut found = None;
            let mut total = 0;
            for (k, e) in st.as_chunks::<6>().0.iter().enumerate() {
                if e[0] as u32 == symbol {
                    found = Some((k as u32, e[1] as u32));
                    lo = total;
                }
                total += e[1] as u32 & mask[e[0] as usize] as u32;
            }
            if let Some((k, f)) = found {
                rc.encode(lo, f, total + esc);
                self.see[see].update();
                self.found_state = stats + 6 * k;
                self.update2();
                return;
            }
            let freq_sum = total + esc;
            rc.encode(total, esc, freq_sum);
            for e in st.as_chunks::<6>().0.iter() {
                mask[e[0] as usize] = 0;
            }
            let see = &mut self.see[see];
            see.summ = see.summ.wrapping_add(freq_sum as u16);
        }
    }
}
