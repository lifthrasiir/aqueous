//! Files of no header taken apart by others archived along with them, planned for the contents
//! to archive before their batches are made ([`plan`]), by their names:
//!
//! - Valve's VPKs (aqr-bundle's `packs::vpk`): a directory file (`NAME_dir.vpk`) holds the tree of
//!   the files, and their data is in it or in archives beside it (`NAME_000.vpk`, …). A
//!   directory file is a format of its own, known by its header; each of its archives among the
//!   contents, of the name the tree tells and of the bytes it needs at least, has the files whose
//!   data it holds.
//! - Unity's resource files (`NAME.resS`): the textures of the serialized file `NAME` beside it
//!   (aqr-bundle's `unity_assets::streamed`) stream their blocks from it, which aqr-tex rebuilds
//!   (`unity_assets::encode_resource`).
//!
//! The plan reads the directory files and the serialized files of resource files alone.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use aqr_bundle::packs::{Pack, unity_assets, vpk};
use aqr_disk::File;
use aqr_formula::{Formula, Kind};

use crate::archive::Source;
use crate::{Cx, Format, Options, kind_by_name};

/// What a content is planned to be taken apart into.
pub(crate) enum Planned {
    /// A VPK's archive: the files whose data it holds.
    Vpk(Vec<File>),
    /// A Unity resource file: where the blocks of the textures streamed from it are, their layouts.
    Ress(Vec<(Range<u64>, unity_assets::Pixels)>),
}

/// The contents to take apart, by content.
#[derive(Default)]
pub(crate) struct Plan {
    by_content: HashMap<usize, Arc<Planned>>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.by_content.is_empty()
    }

    /// The plan of the contents of a batch, `(c, d)` content `c` of data `d`, by where their
    /// data is (as the contexts of the files see it).
    pub fn placed<'a>(&self, contents: impl IntoIterator<Item = (usize, &'a [u8])>) -> Placed {
        let by_at = contents
            .into_iter()
            .filter_map(|(c, d)| Some(((d.as_ptr() as usize, d.len()), self.by_content.get(&c)?.clone())))
            .collect();
        Placed { by_at }
    }
}

/// The contents of a batch to take apart, by where their data is and its length.
pub(crate) struct Placed {
    by_at: HashMap<(usize, usize), Arc<Planned>>,
}

impl Placed {
    pub fn is_empty(&self) -> bool {
        self.by_at.is_empty()
    }

    fn get(&self, d: &[u8]) -> Option<&Planned> {
        self.by_at.get(&(d.as_ptr() as usize, d.len())).map(|p| &**p)
    }
}

/// The format of `d` where it is a content planned ([`plan`]).
pub(crate) fn format(d: &[u8], cx: &Cx) -> Option<Format> {
    Some(match cx.siblings.as_ref()?.get(d)? {
        Planned::Vpk(_) => Format::SourceVpk,
        Planned::Ress(_) => Format::UnityRess,
    })
}

/// A name lowercased, of `/` separators.
fn norm(name: &[u8]) -> Vec<u8> {
    name.iter().map(|&b| if b == b'\\' { b'/' } else { b.to_ascii_lowercase() }).collect()
}

/// The contents of `src` to take apart: of each VPK directory file (named `*_dir.vpk`, of the
/// header of one), the archives beside it; of each serialized file beside a resource file of its
/// name and `.resS`, the resource file.
pub(crate) fn plan<S: Source + ?Sized>(src: &S) -> Plan {
    let mut plan = Plan::default();
    let mut by_name: HashMap<Vec<u8>, usize> = HashMap::new();
    let (mut dirs, mut ress) = (Vec::new(), Vec::new());
    for c in 0..src.len() {
        let name = norm(src.name(c));
        if name.ends_with(b"_dir.vpk") {
            dirs.push(c);
        } else if name.ends_with(b".ress") {
            ress.push(c);
        }
        by_name.insert(name, c);
    }
    for c in dirs {
        let Ok(d) = src.read(c) else { continue };
        let Some((entries, _)) = vpk::tree(&d) else { continue };
        let name = norm(src.name(c));
        let stem = &name[..name.len() - b"dir.vpk".len()];
        let mut archives: Vec<u16> = entries.iter().map(|e| e.archive).filter(|&a| a != vpk::HERE).collect();
        archives.sort_unstable();
        archives.dedup();
        for a in archives {
            let Some(&ac) = by_name.get(&[stem, format!("{a:03}.vpk").as_bytes()].concat()) else { continue };
            let len = src.size(ac) as usize;
            if vpk::archive_len(&entries, a).is_none_or(|n| n > len) {
                continue;
            }
            let files = vpk::archive_files(&entries, a, len);
            if !files.is_empty() {
                plan.by_content.insert(ac, Arc::new(Planned::Vpk(files)));
            }
        }
    }
    for c in ress {
        let name = norm(src.name(c));
        let Some(&assets) = by_name.get(&name[..name.len() - b".ress".len()]) else { continue };
        let Ok(d) = src.read(assets) else { continue };
        let len = src.size(c);
        let textures: Vec<_> =
            unity_assets::streamed(&d, src.name(c)).into_iter().filter(|(r, _)| r.end <= len).collect();
        if !textures.is_empty() {
            plan.by_content.insert(c, Arc::new(Planned::Ress(textures)));
        }
    }
    plan
}

/// The formula of a content planned ([`plan`]); else of a VPK's directory file.
pub(crate) fn encode(d: &[u8], _: Kind, opt: &Options, _: u32, cx: &Cx) -> Option<Formula> {
    match cx.siblings.as_ref().and_then(|p| p.get(d)) {
        Some(Planned::Vpk(files)) => aqr_bundle::packs::encode_files(Pack::SourceVpk, d, files, kind_by_name),
        Some(Planned::Ress(textures)) => unity_assets::encode_resource(d, textures, opt.threads),
        None => aqr_bundle::packs::encode_with(Pack::SourceVpk, d, kind_by_name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::Input;

    /// A VPK directory file (version 1) whose files' data is in archive 0, each of `files`
    /// (name with an extension, bytes) one after another; with the archive's bytes.
    fn vpk_dir(files: &[(&str, &[u8])]) -> (Vec<u8>, Vec<u8>) {
        let (mut tree, mut archive) = (Vec::new(), Vec::<u8>::new());
        for (name, b) in files {
            let (path, ext) = name.rsplit_once('.').unwrap();
            for s in [ext, " ", path] {
                tree.extend(s.as_bytes());
                tree.push(0);
            }
            tree.extend(0xDEADBEEFu32.to_le_bytes());
            tree.extend(0u16.to_le_bytes());
            tree.extend(0u16.to_le_bytes());
            tree.extend((archive.len() as u32).to_le_bytes());
            tree.extend((b.len() as u32).to_le_bytes());
            tree.extend([0xff, 0xff]);
            archive.extend(*b);
            tree.extend([0, 0]);
        }
        tree.push(0);
        let mut d = vec![0x34, 0x12, 0xAA, 0x55, 1, 0, 0, 0];
        d.extend((tree.len() as u32).to_le_bytes());
        d.extend(tree);
        (d, archive)
    }

    /// The contents `plan` takes apart, of files of (name, data).
    fn planned(files: &[(&str, &[u8])]) -> Vec<usize> {
        let inputs: Vec<Input> = files.iter().map(|&(name, data)| Input { name: name.as_bytes(), data }).collect();
        let mut v: Vec<usize> = plan(&inputs[..]).by_content.keys().copied().collect();
        v.sort_unstable();
        v
    }

    #[test]
    fn vpk_archives_by_name() {
        let (dir, archive) = vpk_dir(&[("a.txt", b"hello world"), ("b.bin", &[7; 40])]);
        assert_eq!(planned(&[("pak01_dir.vpk", &dir), ("pak01_000.vpk", &archive)]), [1]);
        // (names lowercased, a backslash a separator)
        assert_eq!(planned(&[("X\\PAK01_DIR.VPK", &dir), ("x/Pak01_000.VPK", &archive)]), [1]);
        // without its archive, or the archive of another name
        assert!(planned(&[("pak01_dir.vpk", &dir)]).is_empty());
        assert!(planned(&[("pak01_dir.vpk", &dir), ("pak02_000.vpk", &archive)]).is_empty());
        assert!(planned(&[("pak01_dir.vpk", &dir), ("pak01_001.vpk", &archive)]).is_empty());
        assert!(planned(&[("pak01_dir.vpk", &dir), ("other/pak01_000.vpk", &archive)]).is_empty());
        // of the bytes the tree needs at least, no fewer
        assert!(planned(&[("pak01_dir.vpk", &dir), ("pak01_000.vpk", &archive[..archive.len() - 1])]).is_empty());
        assert!(planned(&[("pak01_dir.vpk", &dir), ("pak01_000.vpk", b"")]).is_empty());
        // a directory file that is no VPK, or cut short
        assert!(planned(&[("pak01_dir.vpk", b"not a vpk at all"), ("pak01_000.vpk", &archive)]).is_empty());
        assert!(planned(&[("pak01_dir.vpk", b""), ("pak01_000.vpk", &archive)]).is_empty());
        for cut in 0..dir.len() {
            let _ = planned(&[("pak01_dir.vpk", &dir[..cut]), ("pak01_000.vpk", &archive)]);
        }
        assert!(planned(&[("pak01_dir.vpk", &dir[..dir.len() - 3]), ("pak01_000.vpk", &archive)]).is_empty());
        // a directory file of the name `_dir.vpk` alone
        assert_eq!(planned(&[("_dir.vpk", &dir), ("_000.vpk", &archive)]), [1]);
    }

    #[test]
    fn vpk_dirs_claiming_one_archive() {
        let (dir, archive) = vpk_dir(&[("a.txt", b"hello world")]);
        let (dir2, _) = vpk_dir(&[("c.txt", b"hello world, all of it, and more")]);
        // (the same name once lowercased: the archive planned once, no panic)
        let inputs = [
            Input { name: b"pak01_dir.vpk", data: &dir },
            Input { name: b"pak01_000.vpk", data: &archive },
            Input { name: b"PAK01_DIR.VPK", data: &dir2 },
        ];
        assert_eq!(plan(&inputs[..]).by_content.len(), 1);
        // directories of other names plan their own archives
        let inputs = [
            Input { name: b"a_dir.vpk", data: &dir },
            Input { name: b"a_000.vpk", data: &archive },
            Input { name: b"b_dir.vpk", data: &dir },
            Input { name: b"b_000.vpk", data: &archive },
        ];
        assert_eq!(plan(&inputs[..]).by_content.len(), 2);
    }

    /// A serialized file (version 22, little endian, type trees) of Texture2D objects, each
    /// streamed from a path at an offset: BC1 blocks of 64 x 32 and 7 mipmaps (returned: their
    /// size).
    fn serialized(streams: &[(u32, &str)]) -> (Vec<u8>, u32) {
        let size: u32 = (0..7u32).map(|m| ((64 >> m).max(4) / 4) * ((32 >> m).max(4) / 4) * 8).sum();
        let objects: Vec<Vec<u8>> = streams
            .iter()
            .map(|&(offset, path)| {
                let mut o = Vec::new();
                let str4 = |o: &mut Vec<u8>, s: &[u8]| {
                    o.extend((s.len() as u32).to_le_bytes());
                    o.extend(s);
                    o.resize(o.len().next_multiple_of(4), 0);
                };
                str4(&mut o, b"tex");
                o.extend(4u32.to_le_bytes());
                o.push(0);
                o.resize(o.len().next_multiple_of(4), 0);
                for v in [64, 32, size, 10, 7] {
                    o.extend(u32::to_le_bytes(v));
                }
                o.extend([0, 0, 0, 0]);
                for v in [0u32, 1, 2, 1, 1, 0, 0, 0, 0, 0, 1] {
                    o.extend(v.to_le_bytes());
                }
                o.extend(0u32.to_le_bytes());
                o.extend(offset.to_le_bytes());
                o.extend(size.to_le_bytes());
                str4(&mut o, path.as_bytes());
                o
            })
            .collect();
        let mut b = vec![0u8; 48];
        let w = |b: &mut Vec<u8>, v: u32| b.extend(v.to_le_bytes());
        b.extend(b"2019.4.1f1\0");
        w(&mut b, 19);
        b.push(1);
        w(&mut b, 1);
        w(&mut b, 28);
        b.push(0);
        b.extend(0xffffu16.to_le_bytes());
        b.extend([0xa5; 16]);
        let strings = b"Base\0m_Name\0int\0";
        w(&mut b, 3);
        w(&mut b, strings.len() as u32);
        b.extend(vec![0x11; 3 * 32]);
        b.extend(strings);
        w(&mut b, 2);
        w(&mut b, 7);
        w(&mut b, 8);
        w(&mut b, objects.len() as u32);
        let mut start = 0u64;
        for (i, o) in objects.iter().enumerate() {
            while !b.len().is_multiple_of(4) {
                b.push(0);
            }
            b.extend((i as u64 + 1).to_le_bytes());
            b.extend(start.to_le_bytes());
            w(&mut b, o.len() as u32);
            w(&mut b, 0);
            start = (start + o.len() as u64).next_multiple_of(8);
        }
        w(&mut b, 0);
        w(&mut b, 0);
        let meta = b.len() - 48;
        let data_at = b.len().next_multiple_of(16).max(4096);
        b.resize(data_at, 0);
        let mut start = 0usize;
        for o in &objects {
            b.resize(data_at + start, 0);
            b.extend(o);
            start = (start + o.len()).next_multiple_of(8);
        }
        let len = b.len();
        b[8..12].copy_from_slice(&22u32.to_be_bytes());
        b[16] = 0;
        b[20..24].copy_from_slice(&(meta as u32).to_be_bytes());
        b[24..32].copy_from_slice(&(len as u64).to_be_bytes());
        b[32..40].copy_from_slice(&(data_at as u64).to_be_bytes());
        (b, size)
    }

    #[test]
    fn ress_beside_its_serialized_file() {
        let (assets, size) = serialized(&[(1000, "archive:/x/level0.resS")]);
        let t = unity_assets::streamed(&assets, b"level0.resS");
        assert_eq!(t.len(), 1, "the fixture streams a texture");
        let full = vec![9u8; 1000 + size as usize];
        assert_eq!(planned(&[("level0", &assets), ("level0.resS", &full)]), [1]);
        assert_eq!(planned(&[("level0.resS", &full), ("LEVEL0", &assets)]), [0]);
        // (exactly the end of the ranges; one byte less turns the texture down)
        assert!(planned(&[("level0", &assets), ("level0.resS", &full[..full.len() - 1])]).is_empty());
        assert!(planned(&[("level0", &assets), ("level0.resS", b"")]).is_empty());
        // without the serialized file, or one of another name, or one that is none
        assert!(planned(&[("level0.resS", &full)]).is_empty());
        assert!(planned(&[("level1", &assets), ("level0.resS", &full)]).is_empty());
        assert!(planned(&[("level0", b"not serialized"), ("level0.resS", &full)]).is_empty());
        assert!(planned(&[("level0", b""), ("level0.resS", &full)]).is_empty());
        // a resource file of another name than the textures stream from
        assert!(planned(&[("level0", &assets), ("other.resS", &full)]).is_empty());
        for cut in (0..assets.len()).step_by(7) {
            let _ = planned(&[("level0", &assets[..cut]), ("level0.resS", &full)]);
        }
        // (a bare `.resS`, beside a content of an empty name)
        let _ = planned(&[("", &assets), (".resS", &full)]);
    }
}
