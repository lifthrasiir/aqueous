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
