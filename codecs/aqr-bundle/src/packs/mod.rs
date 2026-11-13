//! The asset packs of game engines, as aqr-disk's formulas: a pack's directory read into the
//! files it holds and where their contents lie, so that each file's contents are a content of
//! the application (a stream of its kind, by its name) and the rest (headers, directories,
//! padding, what no file points at, what is encrypted or in compressed blocks) the stream
//! `{format}/rest`, the pack the concatenation of the two. There is no module: what the
//! directories hold that follows from the contents (offsets, sizes) costs about as little
//! stored as it would derived. The README ("Packs of game engines") tells of what each reader
//! takes.
//!
//! The formats are named `<engine>-<extension>` (or `<middleware>-`, `<tool>-`), the names of
//! aqr-compress's formats and the prefixes of the streams' classes ([`Pack::name`]).

/// CRI's `@UTF` tables, which its ACBs and CPKs are.
pub mod cri_utf;
mod ue;

use aqr_disk::{Compressed, File};
use aqr_formula::{Formula, Kind};

/// The packs, each `Variant = "name", module;` (its reader `module::starts` and `module::files`):
/// the enum, [`Pack::ALL`], [`Pack::name`], [`Pack::starts`] and [`Pack::files`].
macro_rules! packs {
    ($($(#[$doc:meta])* $v:ident = $name:literal, $m:ident;)*) => {
        $(pub mod $m;)*

        /// The packs read.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Pack {
            $($(#[$doc])* $v,)*
        }

        impl Pack {
            pub const ALL: [Pack; [$(stringify!($v)),*].len()] = [$(Pack::$v),*];

            /// Its name, `<engine>-<extension>`: of aqr-compress's format, and the prefix of the
            /// classes of its formula's streams.
            pub fn name(self) -> &'static str {
                match self {
                    $(Pack::$v => $name,)*
                }
            }

            /// Whether `d` starts like (or, for a format known by its end, ends like) a pack of
            /// it.
            pub fn starts(self, d: &[u8]) -> bool {
                match self {
                    $(Pack::$v => $m::starts(d),)*
                }
            }

            /// The files of pack `d` with their extents in it, or None where it is not one (or
            /// its directory is encrypted, or does not read).
            pub fn files(self, d: &[u8]) -> Option<Vec<File>> {
                match self {
                    $(Pack::$v => $m::files(d),)*
                }
            }
        }
    };
}

packs! {
    /// Godot's `.pck` (`GDPC`, versions 1 to 4).
    GodotPck = "godot-pck", godot;
    /// Unity's AssetBundle (`UnityFS`): the nodes in blocks stored uncompressed.
    UnityBundle = "unity-bundle", unity;
    /// Unreal Engine 4 and 5's `.pak` (versions 1 to 11), known by its footer.
    UnrealPak = "unreal-pak", unreal;
    /// Valve's VPK of the Source engine (versions 1 and 2): the files in the directory file.
    SourceVpk = "source-vpk", vpk;
    /// Unreal Engine 3's packages (`.upk`, `.u`, `.umap`) and texture caches (`.tfc`): exports,
    /// thumbnails, or compressed chunks
    UnrealUpk = "unreal-upk", unreal_upk;
    /// FMOD Studio's banks (`.bank`, RIFF `FEV `): the FSB5 banks of their sound data
    FmodBank = "fmod-bank", fmod_bank;
    /// Audiokinetic Wwise's SoundBanks (`.bnk`): the media of `DIDX`/`DATA`
    WwiseBnk = "wwise-bnk", wwise_bnk;
    /// Audiokinetic Wwise's file packages (`.pck`, `AKPK`): banks and streamed files
    WwisePck = "wwise-pck", wwise_pck;
    /// Microsoft XACT's wave banks (`.xwb`): the waves' play regions
    XactXwb = "xact-xwb", xact_xwb;
    /// CRI's AFS2 archives (`.awb`): HCA and ADX streams
    CriAwb = "cri-awb", cri_awb;
    /// CRI's cue sheets (`.acb`, `@UTF`): the streams of the AFS2 archive they hold
    CriAcb = "cri-acb", cri_acb;
    /// CRI's CPK archives: files of their `TOC` (or `ITOC`), CRILAYLA ones compressed
    CriCpk = "cri-cpk", cri_cpk;
    /// CRI's old AFS archives
    CriAfs = "cri-afs", cri_afs;
    /// Bethesda's BSA archives (Morrowind, versions 103 to 105)
    BethesdaBsa = "bethesda-bsa", bethesda_bsa;
    /// Bethesda's BA2 archives (`BTDX`, `GNRL` and `DX10`)
    BethesdaBa2 = "bethesda-ba2", bethesda_ba2;
    /// EA's BIG archives (`BIGF`, `BIG4`)
    EaBig = "ea-big", ea_big;
    /// Ren'Py's archives (`RPA-3.0`, `RPA-2.0`)
    RenpyRpa = "renpy-rpa", renpy_rpa;
    /// Kirikiri's XP3 archives
    KirikiriXp3 = "kirikiri-xp3", kirikiri_xp3;
    /// Capcom MT Framework's ARC archives
    MtfArc = "mtf-arc", mtf_arc;
    /// Sony's PSARC archives (`PSAR`)
    SonyPsarc = "sony-psarc", sony_psarc;
    /// Unity's serialized files (`.assets`, `globalgamemanagers`, `levelN`, versions 9 to 22):
    /// their objects
    UnityAssets = "unity-assets", unity_assets;
    /// Unity's `.resource` files: FSB5 banks and videos back to back
    UnityResource = "unity-resource", unity_resource;
    /// GameMaker's `data.win` and `audiogroupN.dat` (`FORM`): textures and sounds
    GamemakerWin = "gamemaker-win", gamemaker_win;
    /// Chromium's resource packs (`.pak`, versions 4 and 5; Electron's and NW.js's games)
    ChromiumPak = "chromium-pak", chromium_pak;
    /// id Software's Quake and Quake II packs (`PACK`)
    QuakePak = "quake-pak", quake_pak;
    /// id Software's Doom WADs (`IWAD`, `PWAD`)
    DoomWad = "doom-wad", doom_wad;
    /// Quake's texture WADs (`WAD2`)
    QuakeWad = "quake-wad2", quake_wad;
    /// Half-Life's texture WADs (`WAD3`)
    HlWad = "hl-wad3", hl_wad;
    /// Garry's Mod's addons (`GMAD`)
    GmodGma = "gmod-gma", gmod_gma;
    /// XNA's and MonoGame's compiled content (`.xnb`): a texture's mipmaps, a sound's samples.
    XnaXnb = "xna-xnb", xna_xnb;
}

/// The pack `d` is, if any: [`Pack::starts`] of the first that holds.
pub fn detect(d: &[u8]) -> Option<Pack> {
    Pack::ALL.into_iter().find(|p| p.starts(d))
}

/// The formula of pack `d` of `pack`, the contents of its files of their kinds by `kind`
/// (aqr-disk's [`aqr_disk::formula::Options::kind`]); None where it is not one, or no file of
/// it has contents in it.
pub fn encode_with(pack: Pack, d: &[u8], kind: fn(&[u8]) -> Kind) -> Option<Formula> {
    encode_files(pack, d, &pack.files(d)?, kind)
}

/// [`encode_with`] of `files` of `d`, as another file tells them (a VPK's archive, by its
/// directory file: [`vpk::archive_files`]).
pub fn encode_files(pack: Pack, d: &[u8], files: &[File], kind: fn(&[u8]) -> Kind) -> Option<Formula> {
    let pieces = aqr_disk::pieces(d, files);
    // (none where no file has contents, or one file is all of it: its contents would be the pack
    // again, detected again, without end, as a chunk of Unreal's starts as a file of chunks does)
    if !pieces.iter().any(|p| p.file.is_some()) || matches!(&pieces[..], [p] if p.file.is_some()) {
        return None;
    }
    let opt = aqr_disk::formula::Options { kind, prefix: pack.name(), ..Default::default() };
    Some(aqr_disk::formula::build(d, files, &pieces, &opt))
}

/// A file of one extent, of rank 0.
fn file(name: Vec<u8>, extent: std::ops::Range<usize>) -> File {
    file_with(name, extent, None)
}

/// A file of one extent, of rank 0, as it is stored (`compressed`).
fn file_with(name: Vec<u8>, extent: std::ops::Range<usize>, compressed: Option<Compressed>) -> File {
    File { name, extents: vec![extent], rank: 0, compressed }
}

/// `name` with its `\` made `/`.
fn slashes(name: &mut [u8]) {
    for b in name {
        if *b == b'\\' {
            *b = b'/';
        }
    }
}

/// Little-endian reads of `d` at `at`, None past its end.
fn u16le(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(at..at.checked_add(2)?)?.try_into().ok()?))
}

fn u32le(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

fn u64le(d: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(d.get(at..at.checked_add(8)?)?.try_into().ok()?))
}

fn u32be(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(d.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

fn u64be(d: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(d.get(at..at.checked_add(8)?)?.try_into().ok()?))
}

/// `start..start + len` as a range of `usize` within `d`, if it fits.
fn within(d: &[u8], start: u64, len: u64) -> Option<std::ops::Range<usize>> {
    let end = start.checked_add(len)?;
    (end <= d.len() as u64).then_some(start as usize..end as usize)
}

/// `b` as UTF-16LE units decoded to UTF-8, lossy (a trailing odd byte dropped).
fn utf16_lossy(b: &[u8]) -> String {
    let units = b.as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c));
    char::decode_utf16(units).map(|c| c.unwrap_or('\u{fffd}')).collect()
}

/// The bytes of a NUL-terminated string at `at`, and the position after its NUL.
fn cstr(d: &[u8], at: usize) -> Option<(&[u8], usize)> {
    let n = d.get(at..)?.iter().position(|&b| b == 0)?;
    Some((&d[at..at + n], at + n + 1))
}

/// The data of the zlib stream at the start of `d` (anything may follow it), if it holds at most
/// `max` bytes; None where the stream is no zlib stream (a header with a dictionary, wrong check
/// bits), is damaged or the data is longer. For the indexes that packs compress.
fn zlib_inflate(d: &[u8], max: usize) -> Option<Vec<u8>> {
    let (a, b) = (*d.first()?, *d.get(1)?);
    if a & 0x0f != 8 || b & 0x20 != 0 || !(a as u32 * 256 + b as u32).is_multiple_of(31) {
        return None;
    }
    let flat = aqr_flate::inflate::parse_segments_max(&d[2..], usize::MAX, max, 0, false).ok()?;
    Some(flat.data.to_vec())
}

/// The formula of a pack of its own (not aqr-disk's, where its contents are applications as well):
/// the pack as pieces in order, each a content (a stream of its kind), an application, or bytes of
/// the rest, the stream `{prefix}/rest`, stream 1, as aqr-disk's formula has it (stream 0 `raw`).
pub struct Pieces<'a> {
    d: &'a [u8],
    pub f: Formula,
    prefix: &'static str,
    kind: fn(&[u8]) -> Kind,
    rest: Vec<u8>,
    parts: Vec<aqr_formula::Node>,
    at: usize,
}

impl<'a> Pieces<'a> {
    pub fn new(d: &'a [u8], prefix: &'static str, kind: fn(&[u8]) -> Kind) -> Pieces<'a> {
        let mut f = Formula::default();
        f.push_content("raw", Kind::Unknown, Vec::new());
        f.push(aqr_disk::formula::rest(prefix), Vec::new());
        Pieces { d, f, prefix, kind, rest: Vec::new(), parts: Vec::new(), at: 0 }
    }

    /// Where the pieces are up to.
    pub fn at(&self) -> usize {
        self.at
    }

    /// The pack's bytes.
    pub fn data(&self) -> &'a [u8] {
        self.d
    }

    /// The bytes up to `end` the rest's.
    pub fn rest_to(&mut self, end: usize) {
        if end > self.at {
            let b = &self.d[self.at..end];
            self.parts.push(aqr_formula::Node::slice(1, self.rest.len() as u64, b.len() as u64));
            self.rest.extend_from_slice(b);
            self.at = end;
        }
    }

    /// The bytes of `r` (from where the pieces are) a content of the kind of `name`.
    pub fn content(&mut self, name: &[u8], r: std::ops::Range<usize>) {
        self.rest_to(r.start);
        let k = (self.kind)(name);
        let class = match k {
            Kind::Unknown => "raw".to_string(),
            Kind::Format(n) => format!("{}/{n}", self.prefix),
            k => format!("{}/{k}", self.prefix),
        };
        let node = self.f.push_content(class, k, self.d[r.clone()].to_vec());
        self.parts.push(node);
        self.at = r.end;
    }

    /// `node` (of the formula, of `r`'s bytes) in place of the bytes of `r`.
    pub fn node(&mut self, node: aqr_formula::Node, r: std::ops::Range<usize>) {
        self.rest_to(r.start);
        self.parts.push(node);
        self.at = r.end;
    }

    /// The formula, the bytes left the rest's.
    pub fn finish(mut self) -> Formula {
        self.rest_to(self.d.len());
        self.f.streams[1].data = self.rest.into();
        self.f.root = aqr_formula::Node::concat(self.parts);
        debug_assert_eq!(self.f.root.len(), self.d.len() as u64);
        self.f
    }
}
