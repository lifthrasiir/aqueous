//! `mp3-recons` as a part module (windows of 16 pages, the inputs taken and the output put in
//! order) through the native emulation of the host, against the whole decoding: on files whose
//! streams are far past the windows (megabytes of frames, junk between runs, a gap of many
//! pieces), and with every stream damaged, cut and extended in turn.

use aqr_api::Window;
use aqr_api::emu::{self, Entry, Kind};
use aqr_formula::Apply;
use aqr_mp3::frame::Header;
use aqr_mp3::{module, plan};

const ENTRY: Entry = Entry {
    kind: Kind::Part,
    windows: &[
        Window::Input(0, module::WINDOW),
        Window::Input(1, module::WINDOW),
        Window::Input(2, module::WINDOW),
        Window::Input(3, module::WINDOW),
        Window::Input(4, module::WINDOW),
        Window::Input(5, module::WINDOW),
        Window::Input(6, module::WINDOW),
        Window::Input(7, module::WINDOW),
        Window::Input(8, module::WINDOW),
        Window::Input(9, module::WINDOW),
        Window::Output(0, module::WINDOW),
    ],
    arity: 13,
    call: |a| module::decode_part(a[0], a[1], [a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11]], a[12]),
};

#[derive(Clone)]
struct Case {
    resource: Vec<u8>,
    inputs: Vec<Vec<u8>>,
    len: u64,
}

impl Case {
    fn of(file: &[u8]) -> Case {
        let f = aqr_mp3::formula::encode(file, 1).expect("coded");
        let got = std::sync::Mutex::new(None);
        let r = f.eval(&|a: &Apply, inputs: &[&[u8]]| {
            assert!(a.params.is_empty());
            *got.lock().unwrap() = Some(Case {
                resource: a.resources[0].to_vec(),
                inputs: inputs.iter().map(|i| i.to_vec()).collect(),
                len: a.len,
            });
            Ok(file[..a.len as usize].to_vec())
        });
        assert!(r.is_ok());
        got.into_inner().unwrap().expect("an application")
    }

    fn lens(&self) -> Vec<u64> {
        self.inputs.iter().map(|i| i.len() as u64).collect()
    }

    fn whole(&self, threads: usize) -> Result<Vec<u8>, String> {
        let inputs: Vec<&[u8]> = self.inputs.iter().map(|i| &i[..]).collect();
        module::decode(&[], &[&self.resource], &inputs, self.len, threads).map_err(|e| e.0)
    }

    fn part(&self) -> Result<Vec<u8>, String> {
        let inputs: Vec<&[u8]> = self.inputs.iter().map(|i| &i[..]).collect();
        let out = emu::apply(&ENTRY, &inputs, &[&self.resource], &[], &[self.len]);
        out.map(|mut o| o.pop().unwrap()).map_err(|f| f.what)
    }
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// `d` repeated up to at least `n` bytes.
fn repeated(d: &[u8], n: usize) -> Vec<u8> {
    d.iter().copied().cycle().take(d.len() * n.div_ceil(d.len())).collect()
}

/// Offsets of the frames of CBR file `d` without CRC from `from` on.
fn frames(d: &[u8]) -> Vec<usize> {
    let (mut v, mut p) = (Vec::new(), 0);
    while p + 4 <= d.len() && d[p] == 0xff {
        let h = Header::from_bytes(&d[p..]);
        if !h.valid_any() {
            break;
        }
        v.push(p);
        p += h.frame_len_free(0);
    }
    v
}

fn same(what: &str, c: &Case) {
    let (w, p) = (c.whole(1), c.part());
    assert_eq!(w.is_ok(), p.is_ok(), "{what}: whole {:?}, part {:?}", w.as_ref().err(), p.as_ref().err());
    if let (Ok(w), Ok(p)) = (w, p) {
        assert!(w == p, "{what}");
    }
}

#[test]
fn fixtures() {
    for e in std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data")).unwrap() {
        let path = e.unwrap().path();
        let d = std::fs::read(&path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let c = Case::of(&d);
        assert!(c.part().unwrap_or_else(|e| panic!("{name}: {e}")) == d, "{name}");
        // (the same, repeated: runs of frames, and the junk between them)
        let d = repeated(&d, 300 << 10);
        let c = Case::of(&d);
        assert!(c.part().unwrap_or_else(|e| panic!("{name} repeated: {e}")) == d, "{name} repeated");
    }
}

#[test]
fn through_windows() {
    // (a window is 1 MiB: the coded streams are over several of them)
    for (name, n) in [("cbr128.mp3", 14 << 20), ("vbr.mp3", 6 << 20), ("crc.mp3", 5 << 20), ("free.mp3", 4 << 20)] {
        let d = repeated(&fixture(name), n);
        let c = Case::of(&d);
        let longest = *c.lens().iter().max().unwrap();
        eprintln!("{name}: {} bytes, inputs of {:?}", d.len(), c.lens());
        assert!(c.whole(1).unwrap() == d, "{name}: whole");
        assert!(c.part().unwrap_or_else(|e| panic!("{name}: {e}")) == d, "{name}: part");
        assert!(longest > (module::WINDOW as u64) << 16 || n < 8 << 20, "{name}: past a window");
    }
}

#[test]
fn junk_and_gaps() {
    let one = fixture("cbr128.mp3");
    let mut d = repeated(&one, 3 << 20);
    let fr = frames(&d);
    assert!(fr.len() > 7000, "{}", fr.len());
    // frames whose main_data_begin is out of range: their payloads are one gap, many pieces long
    for &p in &fr[2000..6500] {
        d[p + 4] = 0xff;
        d[p + 5] |= 0x80;
    }
    let c = Case::of(&d);
    assert!(c.inputs[6].len() > 512 << 10, "{:?}", c.lens());
    assert!(c.part().unwrap() == d, "gap");
    // junk between runs (past the window too), before and after
    let mut j = b"ID3\x04\0\0\0\0\x01\x00".to_vec();
    j.extend((0..128).map(|i| i as u8));
    let cut = fr[1000];
    j.extend_from_slice(&d[..cut]);
    j.extend((0..(1200 << 10)).map(|i: u32| (i.wrapping_mul(2654435761) >> 25) as u8 & 0x7f));
    j.extend_from_slice(&d[cut..fr[5000]]);
    j.extend_from_slice(b"TAG");
    j.extend((0..3000).map(|i| i as u8 & 0x7f));
    let c = Case::of(&j);
    assert!(c.inputs[1].len() > 1 << 20, "{:?}", c.lens());
    assert!(c.part().unwrap() == j, "junk");
}

#[test]
fn damaged_cut_extended() {
    let d = repeated(&fixture("crc.mp3"), 120 << 10);
    let c = Case::of(&d);
    same("intact", &c);
    for k in 0..c.inputs.len() {
        let n = c.inputs[k].len();
        if n == 0 {
            continue;
        }
        for at in [1, 2, 3] {
            let mut bad = c.clone();
            bad.inputs[k][n * at / 4] ^= 0x10;
            same(&format!("stream {k} damaged at {at}/4"), &bad);
        }
        for cut in [0, n / 3, n - 1] {
            let mut bad = c.clone();
            bad.inputs[k].truncate(cut);
            // (the rANS words are a multiple of 4)
            if k == 8 {
                bad.inputs[k].truncate(cut / 4 * 4);
            }
            let (w, p) = (bad.whole(1), bad.part());
            // (the whole decoding reads zeros past the end of a string; the part fails there)
            assert!(p.is_err() || w == p, "stream {k} cut to {cut}: part ok, whole {:?}", w.err());
        }
        let mut bad = c.clone();
        bad.inputs[k].extend_from_slice(&[0; 16]);
        same(&format!("stream {k} extended"), &bad);
    }
    for len in [c.len - 1, c.len + 1, c.len - 5000, 0] {
        let mut bad = c.clone();
        bad.len = len;
        same(&format!("length {len}"), &bad);
    }
}

#[test]
fn plan_does_not_follow_lengths() {
    let c = Case::of(&repeated(&fixture("cbr128.mp3"), 200 << 10));
    let small = plan::decode(1 << 20, &[], &[c.resource.len() as u64], &c.lens(), c.len).unwrap();
    let big = plan::decode(1 << 20, &[], &[c.resource.len() as u64], &[1 << 30; 10], 1 << 28);
    assert_eq!(big, Some(small));
    // (the reservoir's positions are in bits)
    assert_eq!(plan::decode(1 << 20, &[], &[c.resource.len() as u64], &c.lens(), 1 << 30), None);
}
