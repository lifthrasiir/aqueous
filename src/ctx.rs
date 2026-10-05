//! Context identifiers of the decision stream.
//!
//! Every binary decision carries a [`Ctx`]. Contexts are allocated in named groups, and every
//! group belongs to one [`Channel`]; decisions about related things (token kinds, match
//! lengths, distances, Huffman headers...) thus end up in separate channels that can be
//! stored and modelled independently. Context ids are stable for a given layout version
//! ([`LAYOUT`]), which is what trained priors are keyed on.

/// Bump whenever groups are added, removed, resized or reinterpreted.
pub const LAYOUT: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Channel {
    /// Predictor configuration, block types and sizes, padding, trailing data.
    Block = 0,
    /// Literal-or-match decisions.
    Token = 1,
    /// Match length corrections.
    Length = 2,
    /// Match distance ranks.
    Distance = 3,
    /// Dynamic Huffman headers.
    Tree = 4,
}

impl Channel {
    pub const ALL: [Channel; 5] = [
        Channel::Block,
        Channel::Token,
        Channel::Length,
        Channel::Distance,
        Channel::Tree,
    ];
    pub const COUNT: usize = Self::ALL.len();

    pub fn name(self) -> &'static str {
        match self {
            Channel::Block => "block",
            Channel::Token => "token",
            Channel::Length => "length",
            Channel::Distance => "distance",
            Channel::Tree => "tree",
        }
    }
}

/// A context id. Ids below [`Channel::COUNT`] are the bypass (equiprobable, unmodelled)
/// contexts of each channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Ctx(pub u16);

impl Ctx {
    pub const fn bypass(ch: Channel) -> Ctx {
        Ctx(ch as u16)
    }

    pub fn is_bypass(self) -> bool {
        (self.0 as usize) < Channel::COUNT
    }

    pub fn channel(self) -> Channel {
        if self.is_bypass() {
            return Channel::ALL[self.0 as usize];
        }
        GROUPS[self.group()].channel
    }

    /// Index into [`GROUPS`]; panics on bypass contexts.
    pub fn group(self) -> usize {
        let i = GROUP_OFFSETS.partition_point(|&o| o <= self.0);
        assert!(i > 0, "bypass context has no group");
        i - 1
    }

    pub fn describe(self) -> String {
        if self.is_bypass() {
            return format!("{}.bypass", self.channel().name());
        }
        let g = self.group();
        format!("{}[{}]", GROUPS[g].name, self.0 - GROUP_OFFSETS[g])
    }
}

pub struct Group {
    pub name: &'static str,
    pub channel: Channel,
    pub size: u16,
}

/// A group handle; `at(i)` gives its `i`-th context.
#[derive(Clone, Copy)]
pub struct Grp {
    pub base: u16,
    pub size: u16,
    pub channel: Channel,
}

impl Grp {
    #[inline]
    pub fn at(self, i: usize) -> Ctx {
        debug_assert!(
            i < self.size as usize,
            "context {i} out of group of {}",
            self.size
        );
        Ctx(self.base + i as u16)
    }

    /// The `k`-th of equal subgroups of `size` contexts.
    pub fn sub(self, k: usize, size: usize) -> Grp {
        debug_assert!((k + 1) * size <= self.size as usize);
        Grp {
            base: self.base + (k * size) as u16,
            size: size as u16,
            channel: self.channel,
        }
    }
}

const fn offsets<const N: usize>(groups: &[Group]) -> [u16; N] {
    let mut out = [0u16; N];
    let mut o = Channel::COUNT as u16;
    let mut i = 0;
    while i < N {
        out[i] = o;
        o += groups[i].size;
        i += 1;
    }
    out
}

macro_rules! groups {
    ($($id:ident = $name:literal, $ch:ident, $n:expr;)*) => {
        #[allow(non_camel_case_types, clippy::upper_case_acronyms)]
        #[derive(Clone, Copy)]
        enum Gid { $($id,)* }
        pub const GROUPS: &[Group] = &[$(Group { name: $name, channel: Channel::$ch, size: $n },)*];
        const NGROUPS: usize = GROUPS.len();
        pub const GROUP_OFFSETS: [u16; NGROUPS] = offsets::<NGROUPS>(GROUPS);
        $(pub const $id: Grp = Grp {
            base: GROUP_OFFSETS[Gid::$id as usize],
            size: $n,
            channel: Channel::$ch,
        };)*
    };
}

/// Number of buckets of [`len_bucket`].
pub const LB: usize = 9;

/// Coarse match-length classes used to condition several decisions.
pub fn len_bucket(len: usize) -> usize {
    match len {
        0..=2 => 0,
        3 => 1,
        4 => 2,
        5 => 3,
        6..=8 => 4,
        9..=16 => 5,
        17..=32 => 6,
        33..=257 => 7,
        _ => 8,
    }
}

groups! {
    // Block channel
    BTYPE = "block.btype", Block, 4 * 2;
    FINAL = "block.final", Block, 3;
    COUNT_EOF = "block.count_eof", Block, 2 * 2;
    COUNT_SAME = "block.count_same", Block, 2;
    COUNT_EG = "block.count_eg", Block, 24;
    STORED_PAD_ZERO = "block.stored_pad_zero", Block, 1;
    STORED_LEN_FULL = "block.stored_len_full", Block, 1;
    STORED_LEN_SAME = "block.stored_len_same", Block, 1;
    TAIL_PAD_ZERO = "block.tail_pad_zero", Block, 1;
    TRAILING_EG = "block.trailing_eg", Block, 24;
    // Token channel: bucket(predicted len) x next-position comparison x previous kind x far
    IS_MATCH = "token.is_match", Token, (LB * 3 * 2 * 2) as u16;
    // Length channel
    LEN_EQ = "length.eq", Length, (LB * 3) as u16;
    LEN_UP = "length.up", Length, LB as u16;
    LEN_MAG_UP = "length.mag_up", Length, 10;
    LEN_MAG_DOWN = "length.mag_down", Length, 10;
    LEN_FRESH = "length.fresh", Length, 10;
    ALT258 = "length.alt258", Length, 1;
    // Distance channel
    RANK_FOUND = "distance.found", Distance, 1;
    RANK_ZERO = "distance.zero", Distance, (2 * LB) as u16;
    RANK_EG = "distance.rank_eg", Distance, 2 * 16;
    // Tree channel
    HLIT_EQ = "tree.hlit_eq", Tree, 1;
    HDIST_EQ = "tree.hdist_eq", Tree, 1;
    HCLEN_EQ = "tree.hclen_eq", Tree, 1;
    LENS_ALL_EQ = "tree.lens_all_eq", Tree, 1;
    LENS_EQ = "tree.lens_eq", Tree, 16 * 2;
    LENS_VAL = "tree.lens_val", Tree, 16 * 16;
    CL_ALL_EQ = "tree.cl_all_eq", Tree, 1;
    CL_EQ = "tree.cl_eq", Tree, 8 * 2;
    CL_VAL = "tree.cl_val", Tree, 8 * 8;
    RLE_CAND = "tree.rle_cand", Tree, 3;
    RLE_TYPE = "tree.rle_type", Tree, 16 * 3;
    RLE_MAXRUN = "tree.rle_maxrun", Tree, 3;
}

/// Total number of context ids, bypass ones included.
pub const NUM_CTX: usize = GROUP_OFFSETS[NGROUPS - 1] as usize + GROUPS[NGROUPS - 1].size as usize;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout() {
        assert_eq!(BTYPE.base as usize, Channel::COUNT);
        assert_eq!(Ctx(BTYPE.base).describe(), "block.btype[0]");
        assert_eq!(RLE_MAXRUN.at(2).0 as usize, NUM_CTX - 1);
        assert_eq!(IS_MATCH.at(5).channel(), Channel::Token);
        assert_eq!(Ctx::bypass(Channel::Tree).channel(), Channel::Tree);
        assert_eq!(IS_MATCH.at(5).describe(), "token.is_match[5]");
    }
}
