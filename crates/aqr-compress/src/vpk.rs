//! Valve's VPKs (aqr-bundle's `packs::vpk`): a directory file (`NAME_dir.vpk`) holds the tree
//! of the files, and their data is in it or in archives of no header beside it
//! (`NAME_000.vpk`, `NAME_001.vpk`, …). A directory file is a format of its own, known by its
//! header (the files whose data it holds taken apart); its archives are taken apart by its tree,
//! planned for the contents to archive before their batches are made ([`plan`]): each archive
//! among them of the name the tree tells and of the bytes it needs at least, the files whose data
//! it holds. The plan reads the directory files alone (a few MB for a game's of GB).

use std::collections::HashMap;
use std::sync::Arc;

use aqr_bundle::packs::{Pack, vpk};
use aqr_disk::File;
use aqr_formula::{Formula, Kind};

use crate::archive::Source;
use crate::{Cx, Options, kind_by_name};

/// The archives to take apart, by content: the files of each.
#[derive(Default)]
pub(crate) struct Plan {
    by_content: HashMap<usize, Arc<Vec<File>>>,
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

/// The archives of a batch to take apart, by where their data is and its length.
pub(crate) struct Placed {
    by_at: HashMap<(usize, usize), Arc<Vec<File>>>,
}

impl Placed {
    pub fn is_empty(&self) -> bool {
        self.by_at.is_empty()
    }

    fn get(&self, d: &[u8]) -> Option<&[File]> {
        self.by_at.get(&(d.as_ptr() as usize, d.len())).map(|f| &f[..])
    }
}

/// Whether `d` is an archive of a VPK planned ([`plan`]).
pub(crate) fn has(d: &[u8], cx: &Cx) -> bool {
    cx.vpks.as_ref().is_some_and(|p| p.get(d).is_some())
}

/// A name lowercased, of `/` separators.
fn norm(name: &[u8]) -> Vec<u8> {
    name.iter().map(|&b| if b == b'\\' { b'/' } else { b.to_ascii_lowercase() }).collect()
}

/// The archives among the contents of `src` to take apart: of each directory file (named
/// `*_dir.vpk`, of the header of one), the contents of the names of its archives beside it,
/// of the bytes the tree needs of each at least.
pub(crate) fn plan<S: Source + ?Sized>(src: &S) -> Plan {
    let mut plan = Plan::default();
    let mut by_name: HashMap<Vec<u8>, usize> = HashMap::new();
    let mut dirs = Vec::new();
    for c in 0..src.len() {
        let name = norm(src.name(c));
        if name.ends_with(b"_dir.vpk") {
            dirs.push(c);
        } else if name.ends_with(b".vpk") {
            by_name.insert(name, c);
        }
    }
    for c in dirs {
        if by_name.is_empty() {
            break;
        }
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
                plan.by_content.insert(ac, Arc::new(files));
            }
        }
    }
    plan
}

/// The formula of a VPK: of an archive planned ([`plan`]), else of a directory file.
pub(crate) fn encode(d: &[u8], _: Kind, _: &Options, _: u32, cx: &Cx) -> Option<Formula> {
    match cx.vpks.as_ref().and_then(|p| p.get(d)) {
        Some(files) => aqr_bundle::packs::encode_files(Pack::SourceVpk, d, files, kind_by_name),
        None => aqr_bundle::packs::encode_with(Pack::SourceVpk, d, kind_by_name),
    }
}
