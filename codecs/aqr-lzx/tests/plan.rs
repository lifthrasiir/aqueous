//! The memory plan of `lzx-recons` asks the data for its length only: an application whose data
//! is not at hand (sunk, as aqr-wim's of a large image are) still has its plan.

use aqr_formula::{Contents, Formula, Node, Usage};
use aqr_lzx::formula::{self, MODULE, Stream};

/// An application's inputs, of which the data (the first) can be made unavailable.
struct Inputs<'a>(Vec<&'a [u8]>, bool);

impl Contents for Inputs<'_> {
    fn input(&self, k: usize) -> Option<&[u8]> {
        if k == 0 && !self.1 { None } else { self.0.get(k).copied() }
    }
}

#[test]
fn plan_without_the_data() {
    let d = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/greedy.chunks")).unwrap();
    let (mut chunks, mut at) = (Vec::new(), 0);
    while at < d.len() {
        let n = u32::from_le_bytes(d[at..at + 4].try_into().unwrap()) as usize;
        let len = u32::from_le_bytes(d[at + 4..at + 8].try_into().unwrap()) as usize;
        chunks.push((&d[at + 8..at + 8 + len], n));
        at += 8 + len;
    }
    let streams = vec![chunks[..1].to_vec(), chunks[1..].to_vec()];
    let mut f = Formula::default();
    f.push("raw", Vec::new());
    f.push("kept", b"<before><between><after>".to_vec());
    let (mut parts, mut len) = (Vec::new(), 8 + 9 + 7);
    for (i, (s, sp)) in streams.iter().zip(formula::split(&streams, 1)).enumerate() {
        let sp = sp.unwrap();
        let at = f.streams[0].data.len() as u64;
        f.streams[0].data.extend_from_slice(&sp.data);
        let kept = if i == 0 { Node::slice(1, 0, 8) } else { Node::slice(1, 8, 9) };
        len += s.iter().map(|c| c.0.len()).sum::<usize>();
        parts.push(Stream { kept, data: Node::slice(0, at, sp.data.len() as u64), record: sp.record });
    }
    let Node::Apply(a) = formula::apply(&mut f, parts, Node::slice(1, 17, 7), len as u64) else { panic!("an application") };
    // (its inputs: the data, the records, the bytes kept and the index, the streams made)
    let inputs: Vec<Vec<u8>> = [0, 2, 1, 3].iter().map(|&s| f.streams[s].data.to_vec()).collect();
    let lens: Vec<u64> = inputs.iter().map(|i| i.len() as u64).collect();
    let plan = |data: bool| {
        let c = Inputs(inputs.iter().map(|i| &i[..]).collect(), data);
        let u = Usage { params: &a.params, resources: &[], inputs: &lens, outputs: &[a.len], contents: &c, initial_pages: 20 };
        MODULE.plan_for(&u)
    };
    assert!(plan(true).is_some());
    assert_eq!(plan(false), plan(true), "a plan of the data not at hand");
}
