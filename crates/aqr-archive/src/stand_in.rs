//! Stand-ins of Wasm modules ([`stand_in`]) and the custom sections of modules: their names and
//! task slots.

use super::*;

/// A Wasm module of a custom section only, naming `m`: what stands in for its Wasm module where
/// there is none (in tests, and to size formulas apart from modules), and which [`natives`]
/// knows `m` by. No other [`Runner`] runs it.
pub fn stand_in(m: Module) -> Vec<u8> {
    fn custom(w: &mut Vec<u8>, name: &str, payload: &[u8]) {
        let mut s = Vec::new();
        varint::put(&mut s, name.len() as u64);
        s.extend_from_slice(name.as_bytes());
        s.extend_from_slice(payload);
        w.push(0);
        varint::put(w, s.len() as u64);
        w.extend_from_slice(&s);
    }
    let mut w = WASM_HEADER.to_vec();
    custom(&mut w, STAND_IN_SECTION, m.to_string().as_bytes());
    w
}

/// The name of the module that Wasm module `w` is the [`stand_in`] of, if any.
pub(crate) fn stand_in_name(w: &[u8]) -> Option<&str> {
    std::str::from_utf8(custom_section(w, STAND_IN_SECTION)?).ok()
}

/// The task slots of Wasm module `w` (its custom section `aqr_task_slots`, the spec's "Tasks"),
/// those of its classes added up (the units that may run at once), 0 where it has none, or not
/// a valid one (which loading it fails on).
pub(crate) fn task_slots(w: &[u8]) -> u64 {
    match custom_section(w, "aqr_task_slots") {
        Some(b) if !b.is_empty() && b.iter().all(|&n| matches!(n & 0x7f, 1..=64)) => {
            b.iter().map(|&n| u64::from(n & 0x7f)).sum()
        }
        _ => 0,
    }
}

/// The task slots of each class of Wasm module `w` (none where it has none, or not a valid one).
pub fn class_slots(w: &[u8]) -> Vec<u64> {
    match custom_section(w, "aqr_task_slots") {
        Some(b) if !b.is_empty() && b.iter().all(|&n| matches!(n & 0x7f, 1..=64)) => {
            b.iter().map(|&n| u64::from(n & 0x7f)).collect()
        }
        _ => Vec::new(),
    }
}

/// The classes of tasks of Wasm module `w`: the bytes of its custom section `aqr_task_slots`,
/// none where it has none (a module that is not valid fails when it is loaded).
pub fn task_classes(w: &[u8]) -> usize {
    custom_section(w, "aqr_task_slots").map_or(0, <[u8]>::len)
}

/// The content of the first custom section `name` of Wasm module `w`, if any.
pub fn custom_section<'w>(w: &'w [u8], name: &str) -> Option<&'w [u8]> {
    let mut at = WASM_HEADER.len();
    if w.get(..at)? != WASM_HEADER {
        return None;
    }
    while at < w.len() {
        let id = w[at];
        at += 1;
        let size = usize::try_from(varint::get(w, &mut at).ok()?).ok()?;
        let s = w.get(at..at.checked_add(size)?)?;
        at += size;
        if id == 0 {
            let mut p = 0;
            let n = usize::try_from(varint::get(s, &mut p).ok()?).ok()?;
            if s.get(p..p.checked_add(n)?)? == name.as_bytes() {
                return Some(&s[p + n..]);
            }
        }
    }
    None
}
