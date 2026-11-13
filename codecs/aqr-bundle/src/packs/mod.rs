//! The asset packs of game engines, as aqr-disk's formulas: a pack's directory read into the
//! files it holds and where their contents lie, so that each file's contents are a content of
//! the application (a stream of its kind, by its name) and the rest (headers, directories,
//! padding, what no file points at, what is encrypted or in compressed blocks) the stream
//! `{format}/rest`, the pack the concatenation of the two. There is no module: what the
//! directories hold that follows from the contents (offsets, sizes) costs about as little
//! stored as it would derived. The README ("Packs of game engines") tells of what each reader
//! takes.
//!
//! The formats are named `<engine>-<extension>`, the names of aqr-compress's formats and the
//! prefixes of the streams' classes: [`Pack::GodotPck`], [`Pack::UnityBundle`],
//! [`Pack::UnrealPak`], [`Pack::SourceVpk`].

pub mod godot;
pub mod unity;
pub mod unreal;
pub mod vpk;

use aqr_disk::File;
use aqr_formula::{Formula, Kind};

/// The packs read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pack {
    /// Godot's `.pck` (`GDPC`, versions 1 to 4).
    GodotPck,
    /// Unity's AssetBundle (`UnityFS`): the nodes in blocks stored uncompressed.
    UnityBundle,
    /// Unreal Engine 4 and 5's `.pak` (versions 1 to 11).
    UnrealPak,
    /// Valve's VPK of the Source engine (versions 1 and 2): the files in the directory file.
    SourceVpk,
}

impl Pack {
    pub const ALL: [Pack; 4] = [Pack::GodotPck, Pack::UnityBundle, Pack::UnrealPak, Pack::SourceVpk];

    /// Its name, `<engine>-<extension>`: of aqr-compress's format, and the prefix of the classes
    /// of its formula's streams.
    pub fn name(self) -> &'static str {
        match self {
            Pack::GodotPck => "godot-pck",
            Pack::UnityBundle => "unity-bundle",
            Pack::UnrealPak => "unreal-pak",
            Pack::SourceVpk => "source-vpk",
        }
    }

    /// Whether `d` starts like (or, for a `.pak`, ends like) a pack of it.
    pub fn starts(self, d: &[u8]) -> bool {
        match self {
            Pack::GodotPck => godot::starts(d),
            Pack::UnityBundle => unity::starts(d),
            Pack::UnrealPak => unreal::footer(d).is_some(),
            Pack::SourceVpk => vpk::starts(d),
        }
    }

    /// The files of pack `d` with their extents in it, or None where it is not one (or its
    /// directory is encrypted).
    pub fn files(self, d: &[u8]) -> Option<Vec<File>> {
        match self {
            Pack::GodotPck => godot::files(d),
            Pack::UnityBundle => unity::files(d),
            Pack::UnrealPak => unreal::files(d),
            Pack::SourceVpk => vpk::files(d),
        }
    }
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
    if !pieces.iter().any(|p| p.file.is_some()) {
        return None;
    }
    let opt = aqr_disk::formula::Options { kind, prefix: pack.name(), ..Default::default() };
    Some(aqr_disk::formula::build(d, files, &pieces, &opt))
}

/// A file of one extent, of rank 0.
fn file(name: Vec<u8>, extent: std::ops::Range<usize>) -> File {
    File { name, extents: vec![extent], rank: 0, compressed: None }
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

/// The bytes of a NUL-terminated string at `at`, and the position after its NUL.
fn cstr(d: &[u8], at: usize) -> Option<(&[u8], usize)> {
    let n = d.get(at..)?.iter().position(|&b| b == 0)?;
    Some((&d[at..at + n], at + n + 1))
}
