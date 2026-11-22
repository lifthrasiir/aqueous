//! `decode`'s body ([`decode`]): an archive opened and decoded, and where it decodes, read as it
//! is read otherwise (by its index, of some paths alone, as it was at each segment, written again
//! by `rewrite`), each of which has to give what the whole decoding does of it.

use std::collections::BTreeMap;

use aqr_archive::{Archive, EntryNode, Level, Payload, Tree, Value};

/// The files of `v` by their paths (its file alone, of an archive of one, at the empty path).
fn files(v: &Value) -> BTreeMap<Vec<Vec<u8>>, Vec<u8>> {
    match v {
        Value::File(d) => BTreeMap::from([(Vec::new(), d.clone())]),
        Value::Tree(t, files) => {
            let mut out = BTreeMap::new();
            for (p, e) in paths(t).into_iter().zip(&t.entries) {
                if let EntryNode::File(c) = e.node {
                    out.insert(p, files[c].clone());
                }
            }
            out
        }
    }
}

/// The path of each entry of `t`.
fn paths(t: &Tree) -> Vec<Vec<Vec<u8>>> {
    let mut out: Vec<Vec<Vec<u8>>> = Vec::with_capacity(t.entries.len());
    for e in &t.entries {
        let mut p = e.parent.map_or_else(Vec::new, |q| out[q].clone());
        p.push(e.name.clone());
        out.push(p);
    }
    out
}

/// What an archive opened another way decodes to has to be what the whole decoding gives.
fn alike(how: &str, other: aqr_archive::Decoded<Archive>, want: &Value) {
    match other.and_then(|a| a.value(1)) {
        Ok(v) => assert!(v == *want, "{how}: decoded otherwise"),
        Err(e) => panic!("{how}: not decoded, though the archive decodes: {e}"),
    }
}

pub fn decode(c: &[u8]) {
    let runner = crate::toys();
    let _ = aqr_archive::footer_of(c);
    let _ = aqr_archive::trim(c);
    let _ = aqr_archive::pe_certificate_table(c);
    if let Some(r) = aqr_archive::locate(c)
        && let Ok(a) = aqr_archive::open(&c[r], &runner)
    {
        let _ = a.value(1);
    }
    let _ = aqr_archive::inspect::inspect(c, false, &|_| None, Some(64));
    let Ok(a) = aqr_archive::open(c, &runner) else { return };
    let _ = a.archival();
    let Ok(want) = a.value(1) else { return };
    let p = Payload::Plain(c);
    if !p.of_tree() {
        return;
    }
    alike("by its index", aqr_archive::open_indexed(p, &runner, 1), &want);
    // as it was after each segment: whole and by its index alike
    for m in a.segment_metadata().unwrap_or_default() {
        let whole = aqr_archive::open_upto(c, &runner, m).and_then(|a| a.value(1));
        match whole {
            Ok(v) => alike(&format!("up to {m} by its index"), aqr_archive::open_indexed_upto(p, &runner, 1, m), &v),
            Err(e) => panic!("up to {m}: not decoded, though the archive decodes: {e}"),
        }
    }
    let Some(t) = a.tree() else { return };
    let all = files(&want);
    // of some paths alone (every other file's, as the entries come): those files as they are
    let ps = paths(t);
    let some: Vec<Vec<Vec<u8>>> = ps
        .iter()
        .zip(&t.entries)
        .filter(|(_, e)| matches!(e.node, EntryNode::File(_)))
        .map(|(p, _)| p.clone())
        .step_by(2)
        .collect();
    // (its files taken alone, by `contents_of`, as `aqr -d` takes those of paths given)
    match aqr_archive::open_indexed_at(p, &runner, 1, &some) {
        Ok(a) => {
            let t = a.tree().expect("a tree").clone();
            let ps = paths(&t);
            let mut wanted = vec![false; a.contents_len()];
            let mut path_of = vec![Vec::new(); a.contents_len()];
            for (path, e) in ps.iter().zip(&t.entries) {
                if let EntryNode::File(c) = e.node
                    && some.contains(path)
                    && let Some(w) = wanted.get_mut(c)
                {
                    *w = true;
                    path_of[c] = path.clone();
                }
            }
            let got = std::sync::Mutex::new(BTreeMap::new());
            let r = a.contents_of(1, Some(&wanted), |c, d| {
                got.lock().unwrap().insert(path_of[c].clone(), d);
                Ok(())
            });
            if let Err(e) = r {
                panic!("of some paths: not decoded, though the archive decodes: {e}");
            }
            for (path, d) in got.into_inner().unwrap() {
                assert!(all.get(&path) == Some(&d), "of some paths: {path:?} decoded otherwise");
            }
        }
        Err(e) => panic!("of some paths: not opened, though the archive decodes: {e}"),
    }
    // written again of its tree without those files: the others as they are
    let mut kept = t.clone();
    let gone: std::collections::HashSet<&Vec<Vec<u8>>> = some.iter().collect();
    let keep: Vec<bool> = ps.iter().map(|p| !gone.contains(p)).collect();
    let mut at = vec![None; kept.entries.len()];
    let mut entries = Vec::new();
    for (i, mut e) in std::mem::take(&mut kept.entries).into_iter().enumerate() {
        if keep[i] && e.parent.is_none_or(|q| at[q].is_some()) {
            e.parent = e.parent.map(|q| at[q].unwrap());
            at[i] = Some(entries.len());
            entries.push(e);
        }
    }
    kept.entries = entries;
    if kept.entries.is_empty() {
        return;
    }
    let mut out = Vec::new();
    if aqr_archive::rewrite(c, &kept, Level::STORED, 1, &mut out).is_ok() {
        match aqr_archive::decode_value(&out, &runner) {
            Ok(v) => {
                let want: BTreeMap<_, _> = all.into_iter().filter(|(p, _)| !gone.contains(p)).collect();
                assert!(files(&v) == want, "written again: decoded otherwise");
            }
            Err(e) => panic!("written again: not decoded, though the archive decodes: {e}"),
        }
    }
}
