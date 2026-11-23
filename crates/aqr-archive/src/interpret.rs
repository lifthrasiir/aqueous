//! The archive value of an archive (spec, "Archive model"), as the conformance suite writes it
//! (`conformance/case.cddl`, `archive-value`): CBOR made by [`archive_value`], shown in EDN by
//! [`edn`].
//!
//! The archive is interpreted as [`super::open`] interprets it, its failures the archive's; what
//! the decoder's tree doesn't keep is taken from the segments [`inspect`]ed, by the path of each
//! node: the entry extras as they are encoded (or null), the fork names as the items they are,
//! the data of unknown nodes. Every map is in its order in the archive value: the children of a
//! directory as the decoder's tree has them, an extra's members as they are encoded.

use std::collections::HashMap;

use super::cbor::{head, int};
use super::inspect::{self, Node, Origin, State};
use super::tree::{self, FORKS, JOURNAL, SEGMENT_STATUS, count, ended, head_of, skip};
use super::{Decoded, EntryNode, Runner, Tree, Value, invalid};

/// The archive value of archive `c` (the payload of an encrypted archive where `sealed`), its
/// modules run by `runner` on up to `threads` threads, as CBOR: a map of `root` and, where it
/// is not empty, `extra`; each node a map of what it has (`children`, `content`, `target`,
/// `kind` and `device`, `kind` and `data`; `link_group`, `extra` and `forks` where not null or
/// empty). Link groups are numbers, apart for those apart.
pub fn archive_value(c: &[u8], sealed: bool, runner: &dyn Runner, threads: usize) -> Decoded<Vec<u8>> {
    let a = match sealed {
        true => super::open_payload(c, runner)?,
        false => super::open(c, runner)?,
    };
    let value = a.value(threads)?;
    drop(a);
    let mut out = Vec::new();
    match value {
        Value::File(d) => {
            head(&mut out, 5, 1);
            text(&mut out, "root");
            head(&mut out, 5, 1);
            text(&mut out, "content");
            bytes(&mut out, &d);
        }
        Value::Tree(t, contents) => {
            let i = inspect::inspect(c, sealed, &|_| None, None)?;
            let extra = archive_extra(&i)?;
            head(&mut out, 5, if extra.is_empty() { 1 } else { 2 });
            text(&mut out, "root");
            Nodes::new(&i, &t, &contents).node(&mut out, None, &mut Vec::new())?;
            if !extra.is_empty() {
                text(&mut out, "extra");
                head(&mut out, 5, extra.len() as u64);
                for (k, v) in extra {
                    out.extend_from_slice(k);
                    out.extend_from_slice(v);
                }
            }
        }
    }
    Ok(out)
}

/// The archive-wide members of the segments, each set in turn (spec, "Interpreting an
/// archive"): a key's later value in place of its earlier one.
fn archive_extra<'i>(i: &'i inspect::Inspection) -> Decoded<Vec<(&'i [u8], &'i [u8])>> {
    let mut extra: Vec<(&[u8], &[u8])> = Vec::new();
    for m in i.segments.iter().flat_map(|g| &g.archive_extra) {
        if matches!(m.key, Some(SEGMENT_STATUS | JOURNAL)) {
            continue;
        }
        let key = tree::whole_item(&m.key_cbor)?;
        let mut found = None;
        for (k, (kb, _)) in extra.iter().enumerate() {
            if tree::whole_item(kb)? == key {
                found = Some(k);
                break;
            }
        }
        match found {
            Some(k) => extra[k].1 = &m.value_cbor,
            None => extra.push((&m.key_cbor, &m.value_cbor)),
        }
    }
    Ok(extra)
}

/// The decoder's tree with what the segments inspected have of each node.
struct Nodes<'a, 'c> {
    i: &'a inspect::Inspection<'c>,
    tree: &'a Tree,
    contents: &'a [Vec<u8>],
    /// By entry (by index in the tree, or the root), its entries.
    children: HashMap<Option<usize>, Vec<usize>>,
    /// By path (names as they are encoded), the entry of a segment that is the node, and the
    /// one whose entry extra it has (none: null).
    of_path: HashMap<Vec<Vec<u8>>, (Origin, Option<Origin>)>,
}

impl<'a, 'c> Nodes<'a, 'c> {
    fn new(i: &'a inspect::Inspection<'c>, tree: &'a Tree, contents: &'a [Vec<u8>]) -> Self {
        let mut children: HashMap<Option<usize>, Vec<usize>> = HashMap::new();
        for (k, e) in tree.entries.iter().enumerate() {
            children.entry(e.parent).or_default().push(k);
        }
        let mut of_path = HashMap::new();
        for (s, (g, states)) in i.segments.iter().zip(i.merged()).enumerate() {
            for (k, (e, state)) in g.entries.iter().zip(states).enumerate() {
                if let State::Current { extra } = state {
                    of_path.insert(e.names.clone(), (Origin { segment: s, entry: k }, extra));
                }
            }
        }
        Nodes { i, tree, contents, children, of_path }
    }

    fn entry(&self, o: Origin) -> &'a inspect::Entry {
        &self.i.segments[o.segment].entries[o.entry]
    }

    /// The node of entry `k` (or the root) at `path`.
    fn node(&self, out: &mut Vec<u8>, k: Option<usize>, path: &mut Vec<Vec<u8>>) -> Decoded<()> {
        let Some(&(node, extra_of)) = self.of_path.get(path) else {
            return invalid("an entry of the tree not among those of the segments inspected");
        };
        let (node_of, x) = match k {
            None => (&EntryNode::Dir, &self.tree.root),
            Some(k) => (&self.tree.entries[k].node, &self.tree.entries[k].extra),
        };
        // the entry extra (but its forks) and the fork names, as they are encoded
        let (mut members, mut fork_names) = (None, Vec::new());
        if let Some(o) = extra_of {
            let all = self.entry(o).members.as_deref().unwrap_or_default();
            let mut kept = Vec::new();
            for m in all {
                match m.key {
                    Some(FORKS) => fork_names = map_keys(&m.value_cbor)?,
                    _ => kept.push(m),
                }
            }
            members = Some(kept);
        }
        if fork_names.len() != x.forks.len() {
            return invalid("forks other than the segment inspected has");
        }
        let link_group = match node_of {
            EntryNode::File(_) => x.link_group,
            _ => None,
        };
        let n = 1
            + matches!(node_of, EntryNode::Device { .. } | EntryNode::Special(_)) as u64
            + link_group.is_some() as u64
            + members.is_some() as u64
            + !x.forks.is_empty() as u64;
        head(out, 5, n);
        match node_of {
            EntryNode::Dir => {
                text(out, "children");
                let none = Vec::new();
                let children = self.children.get(&k).unwrap_or(&none);
                head(out, 5, children.len() as u64);
                for &c in children {
                    let name = &self.tree.entries[c].name;
                    encoded_name(out, name);
                    path.push(name.clone());
                    self.node(out, Some(c), path)?;
                    path.pop();
                }
            }
            EntryNode::File(c) => {
                text(out, "content");
                bytes(out, &self.contents[*c]);
            }
            EntryNode::Link(target) => {
                text(out, "target");
                encoded_name(out, target);
            }
            EntryNode::Fifo => {
                text(out, "kind");
                text(out, "fifo");
            }
            EntryNode::Device { block, major, minor } => {
                text(out, "kind");
                text(out, if *block { "block-device" } else { "char-device" });
                text(out, "device");
                head(out, 4, 2);
                head(out, 0, *major);
                head(out, 0, *minor);
            }
            EntryNode::Special(kind) => {
                let Node::Special(_, data) = &self.entry(node).node else {
                    return invalid("an unknown node not one of the segment inspected");
                };
                text(out, "kind");
                head(out, 0, *kind);
                text(out, "data");
                bytes(out, data);
            }
        }
        if let Some(g) = link_group {
            text(out, "link_group");
            int(out, g);
        }
        if let Some(members) = members {
            text(out, "extra");
            head(out, 5, members.len() as u64);
            for m in members {
                out.extend_from_slice(&m.key_cbor);
                out.extend_from_slice(&m.value_cbor);
            }
        }
        if !x.forks.is_empty() {
            text(out, "forks");
            head(out, 5, x.forks.len() as u64);
            for (name, (_, c)) in fork_names.iter().zip(&x.forks) {
                out.extend_from_slice(name);
                bytes(out, &self.contents[*c]);
            }
        }
        Ok(())
    }
}

/// The keys of map `d` as they are encoded.
fn map_keys(d: &[u8]) -> Decoded<Vec<Vec<u8>>> {
    let mut at = 0;
    let (major, _, arg) = head_of(d, &mut at)?;
    if major != 5 {
        return invalid("forks not a map");
    }
    let n = count(d, at, arg)?;
    let mut keys = Vec::new();
    while !ended(d, &mut at, n, keys.len()) {
        let k = at;
        skip(d, &mut at, 1)?;
        keys.push(d[k..at].to_vec());
        skip(d, &mut at, 1)?;
    }
    Ok(keys)
}

fn text(out: &mut Vec<u8>, t: &str) {
    head(out, 3, t.len() as u64);
    out.extend_from_slice(t.as_bytes());
}

fn bytes(out: &mut Vec<u8>, b: &[u8]) {
    head(out, 2, b.len() as u64);
    out.extend_from_slice(b);
}

/// An entry name or link target as an aqr-text: a text string of a "wtf-8" one of UTF-8, tag
/// 273 of its bytes of one of lone surrogates, a byte string of a "raw" one.
fn encoded_name(out: &mut Vec<u8>, name: &[u8]) {
    match tree::name_bytes(name) {
        (b, true) => bytes(out, b),
        (b, false) if std::str::from_utf8(b).is_ok() => {
            head(out, 3, b.len() as u64);
            out.extend_from_slice(b);
        }
        (b, false) => {
            head(out, 6, 273);
            bytes(out, b);
        }
    }
}

/// Well-formed CBOR data item `d` in EDN (RFC 8610, appendix G), by its data model: lengths
/// and widths not told; byte strings of printable UTF-8 as `'...'`, others in hexadecimal; an
/// array or map on several lines, indented, where it doesn't fit on one.
pub fn edn(d: &[u8]) -> String {
    let mut at = 0;
    match edn_item(d, &mut at, 0) {
        Ok(s) if at == d.len() => s,
        _ => "<bad CBOR>".into(),
    }
}

/// The width an array or map is put on one line within.
const LINE: usize = 100;

fn edn_item(d: &[u8], at: &mut usize, depth: usize) -> Decoded<String> {
    use std::fmt::Write;
    if depth > 256 {
        return super::resource("CBOR nested too deep");
    }
    let (major, ai, arg) = head_of(d, at)?;
    let string = |major: u8, arg: Option<u64>, at: &mut usize| -> Decoded<Vec<u8>> {
        let mut s = Vec::new();
        match arg {
            Some(n) => {
                let n = usize::try_from(n).ok().filter(|&n| n <= d.len() - *at);
                let n = n.ok_or(super::Failure::Invalid("bad CBOR length".into()))?;
                s.extend_from_slice(&d[*at..*at + n]);
                *at += n;
            }
            None => {
                while d.get(*at) != Some(&0xff) {
                    let (m, _, n) = head_of(d, at)?;
                    let n = n.and_then(|n| usize::try_from(n).ok()).filter(|&n| m == major && n <= d.len() - *at);
                    let n = n.ok_or(super::Failure::Invalid("bad CBOR chunk".into()))?;
                    s.extend_from_slice(&d[*at..*at + n]);
                    *at += n;
                }
                *at += 1;
            }
        }
        Ok(s)
    };
    Ok(match (major, arg) {
        (0, Some(v)) => v.to_string(),
        (1, Some(v)) => (-1 - v as i128).to_string(),
        (2, arg) => {
            let b = string(2, arg, at)?;
            match std::str::from_utf8(&b) {
                Ok(t) if !t.is_empty() && t.chars().all(|c| !c.is_control() || matches!(c, '\n' | '\t')) => {
                    quoted(t, '\'')
                }
                _ => {
                    let mut s = String::from("h'");
                    b.iter().for_each(|x| _ = write!(s, "{x:02x}"));
                    s + "'"
                }
            }
        }
        (3, arg) => {
            let b = string(3, arg, at)?;
            match std::str::from_utf8(&b) {
                Ok(t) => quoted(t, '"'),
                Err(_) => {
                    let mut s = String::from("/ not UTF-8 / h'");
                    b.iter().for_each(|x| _ = write!(s, "{x:02x}"));
                    s + "'"
                }
            }
        }
        (4 | 5, arg) => {
            let n = count(d, *at, arg)?;
            let mut items = Vec::new();
            while !ended(d, at, n, items.len()) {
                let mut s = edn_item(d, at, depth + 1)?;
                if major == 5 {
                    s += ": ";
                    s += &edn_item(d, at, depth + 1)?;
                }
                items.push(s);
            }
            let (open, close) = if major == 4 { ('[', ']') } else { ('{', '}') };
            let one = items.iter().map(|s| s.len() + 2).sum::<usize>() + 2 * depth < LINE;
            if items.is_empty() {
                format!("{open}{close}")
            } else if one && items.iter().all(|s| !s.contains('\n')) {
                format!("{open}{}{close}", items.join(", "))
            } else {
                let pad = "  ".repeat(depth + 1);
                let mut s = format!("{open}\n");
                for i in items {
                    // (an item over lines already indented by its depth)
                    _ = writeln!(s, "{pad}{i},");
                }
                s + &"  ".repeat(depth) + &close.to_string()
            }
        }
        (6, Some(tag)) => format!("{tag}({})", edn_item(d, at, depth + 1)?),
        (7, Some(v)) => match ai {
            20 => "false".into(),
            21 => "true".into(),
            22 => "null".into(),
            23 => "undefined".into(),
            0..=24 => format!("simple({v})"),
            25..28 => {
                let f = match ai {
                    25 => tree::half(v as u16),
                    26 => f32::from_bits(v as u32) as f64,
                    _ => f64::from_bits(v),
                };
                match f {
                    f if f.is_nan() => "NaN".into(),
                    f if f.is_infinite() => if f > 0.0 { "Infinity" } else { "-Infinity" }.into(),
                    f => format!("{f:?}"),
                }
            }
            _ => return invalid("bad CBOR"),
        },
        _ => return invalid("bad CBOR"),
    })
}

/// Text `t` quoted by `q`, escaped as JSON's strings are.
fn quoted(t: &str, q: char) -> String {
    use std::fmt::Write;
    let mut s = String::from(q);
    for c in t.chars() {
        match c {
            '\\' => s += "\\\\",
            '\n' => s += "\\n",
            '\t' => s += "\\t",
            '\r' => s += "\\r",
            c if c == q => _ = write!(s, "\\{c}"),
            c if c.is_control() => _ = write!(s, "\\u{:04x}", c as u32),
            c => s.push(c),
        }
    }
    s.push(q);
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edn_by_data_model() {
        // {_ "a": (_ h'01', h'02'), -7: [1.5, simple(16), 273('x')]}
        let d = [
            0xbf, 0x61, b'a', 0x5f, 0x41, 1, 0x41, 2, 0xff, 0x26, 0x83, 0xf9, 0x3e, 0x00, 0xf0, 0xd9, 0x01, 0x11, 0x41,
            b'x', 0xff,
        ];
        assert_eq!(edn(&d), r#"{"a": h'0102', -7: [1.5, simple(16), 273('x')]}"#);
        assert_eq!(edn(&[0x63, b'"', b'\\', 0x01]), r#""\"\\\u0001""#);
    }
}
