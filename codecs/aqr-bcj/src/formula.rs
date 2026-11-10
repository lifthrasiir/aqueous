//! aqr-bcj as modules of entry formulas (see aqr-formula): a filter's encoder applied to the data
//! before it, for the formulas of containers whose streams went through filters (`.xz` blocks,
//! 7z folders). `FORMAT.md` specifies the modules.

use aqr_formula::{Module, Node};

use crate::bra::{self, Arch};
use crate::{Result, delta, module, swap};

pub const X86: Module = Module::new("bcj-x86", 2).with_plan(memory_part).with_contents(&[0]);
pub const POWERPC: Module = Module::new("bcj-powerpc", 2).with_plan(memory_part).with_contents(&[0]);
pub const IA64: Module = Module::new("bcj-ia64", 2).with_plan(memory_part).with_contents(&[0]);
pub const ARM: Module = Module::new("bcj-arm", 2).with_plan(memory_part).with_contents(&[0]);
pub const ARM_THUMB: Module = Module::new("bcj-armthumb", 2).with_plan(memory_part).with_contents(&[0]);
pub const SPARC: Module = Module::new("bcj-sparc", 2).with_plan(memory_part).with_contents(&[0]);
pub const ARM64: Module = Module::new("bcj-arm64", 2).with_plan(memory_part).with_contents(&[0]);
pub const RISCV: Module = Module::new("bcj-riscv", 2).with_plan(memory_part).with_contents(&[0]);
/// The memory of the part modules ([`module::bra_part`], [`module::delta_part`], [`module::bcj2_part`],
/// [`module::swap_part`]): their initial memory alone (they allocate nothing).
fn memory_part(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    Some(u.initial())
}

pub const DELTA: Module = Module::new("bcj-delta", 1).with_plan(memory_part).with_contents(&[0]);
pub const BCJ2: Module = Module::new("bcj2", 2).with_plan(memory_part).with_contents(&[0]);
pub const SWAP2: Module = Module::new("bcj-swap2", 1).with_plan(memory_part).with_contents(&[0]);
pub const SWAP4: Module = Module::new("bcj-swap4", 1).with_plan(memory_part).with_contents(&[0]);

/// Every module of this crate.
pub const MODULES: [Module; 12] = [X86, POWERPC, IA64, ARM, ARM_THUMB, SPARC, ARM64, RISCV, DELTA, BCJ2, SWAP2, SWAP4];

/// The module of a branch converter.
pub fn module_of(arch: Arch) -> Module {
    match arch {
        Arch::X86 => X86,
        Arch::PowerPc => POWERPC,
        Arch::Ia64 => IA64,
        Arch::Arm => ARM,
        Arch::ArmThumb => ARM_THUMB,
        Arch::Sparc => SPARC,
        Arch::Arm64 => ARM64,
        Arch::RiscV => RISCV,
    }
}

/// The output of `m`, one of [`MODULES`], applied to `inputs`. None where `m` is not one.
pub fn decode(m: Module, params: &[u8], inputs: &[&[u8]], len: u64) -> Option<Result<Vec<u8>>> {
    if let Some(&arch) = Arch::ALL.iter().find(|&&a| module_of(a) == m) {
        return Some(module::bra(arch, params, inputs, len));
    }
    if m == DELTA {
        Some(module::delta(params, inputs, len))
    } else if m == BCJ2 {
        Some(module::bcj2(params, inputs, len))
    } else if m == SWAP2 || m == SWAP4 {
        Some(module::swap(if m == SWAP2 { 2 } else { 4 }, params, inputs, len))
    } else {
        None
    }
}

/// A filter of one stream, of a chain of an `.xz` block or a 7z folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    /// A branch converter from its start address.
    Bra(Arch, u32),
    /// Delta of a distance (1 to 256).
    Delta(usize),
    /// 7z's Swap2 or Swap4: the bytes of each unit of this width (2 or 4) reversed.
    Swap(usize),
}

impl Filter {
    /// The filter of an `.xz` filter id and properties (the branch converters' start offset,
    /// le32 or none; Delta's distance less one).
    pub fn from_xz(id: u64, props: &[u8]) -> Option<Filter> {
        let arch = match id {
            3 => {
                let &[k] = props else { return None };
                return Some(Filter::Delta(k as usize + 1));
            }
            4 => Arch::X86,
            5 => Arch::PowerPc,
            6 => Arch::Ia64,
            7 => Arch::Arm,
            8 => Arch::ArmThumb,
            9 => Arch::Sparc,
            0xa => Arch::Arm64,
            0xb => Arch::RiscV,
            _ => return None,
        };
        Filter::bra(arch, props)
    }

    /// The filter of a 7z coder id and properties.
    pub fn from_7z(id: &[u8], props: &[u8]) -> Option<Filter> {
        let arch = match id {
            [3] => {
                let &[k] = props else { return None };
                return Some(Filter::Delta(k as usize + 1));
            }
            &[2, 3, w @ (2 | 4)] => return props.is_empty().then_some(Filter::Swap(w as usize)),
            [3, 3, 1, 3] => Arch::X86,
            [3, 3, 2, 5] => Arch::PowerPc,
            [3, 3, 4, 1] => Arch::Ia64,
            [3, 3, 5, 1] => Arch::Arm,
            [3, 3, 7, 1] => Arch::ArmThumb,
            [3, 3, 8, 5] => Arch::Sparc,
            [0xa] => Arch::Arm64,
            [0xb] => Arch::RiscV,
            _ => return None,
        };
        Filter::bra(arch, props)
    }

    fn bra(arch: Arch, props: &[u8]) -> Option<Filter> {
        let pc = match props {
            [] => 0,
            &[a, b, c, d] => u32::from_le_bytes([a, b, c, d]),
            _ => return None,
        };
        (pc & arch.alignment() == 0).then_some(Filter::Bra(arch, pc))
    }

    pub fn encode(self, d: &mut [u8]) {
        match self {
            Filter::Bra(arch, pc) => bra::convert(arch, d, pc, true),
            Filter::Delta(n) => delta::encode(d, n),
            Filter::Swap(w) => swap::convert(d, w),
        }
    }

    pub fn decode(self, d: &mut [u8]) {
        match self {
            Filter::Bra(arch, pc) => bra::convert(arch, d, pc, false),
            Filter::Delta(n) => delta::decode(d, n),
            Filter::Swap(w) => swap::convert(d, w),
        }
    }

    /// The application of its encoder to `data`, of `len` bytes.
    pub fn apply(self, data: Node, len: u64) -> Node {
        match self {
            Filter::Bra(arch, pc) => {
                let params = if pc == 0 { Vec::new() } else { pc.to_le_bytes().to_vec() };
                Node::apply(module_of(arch), params, vec![data], len)
            }
            Filter::Delta(n) => Node::apply(DELTA, vec![(n - 1) as u8], vec![data], len),
            Filter::Swap(w) => Node::apply(if w == 2 { SWAP2 } else { SWAP4 }, Vec::new(), vec![data], len),
        }
    }
}

/// The data before `filters` (the first applied first in encoding) of `filtered`, where encoding
/// it gives back `filtered` (else None).
pub fn unfilter(filters: &[Filter], filtered: &[u8]) -> Option<Vec<u8>> {
    let mut d = filtered.to_vec();
    filters.iter().rev().for_each(|f| f.decode(&mut d));
    let mut e = d.clone();
    filters.iter().for_each(|f| f.encode(&mut e));
    (e == filtered).then_some(d)
}

/// The node of `filters` applied to `data` (of `len` bytes), the first innermost.
pub fn apply(filters: &[Filter], data: Node, len: u64) -> Node {
    filters.iter().fold(data, |n, f| f.apply(n, len))
}
