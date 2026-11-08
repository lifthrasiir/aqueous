//! `flac-recons` as a part module (windows of 16 pages, the inputs taken and the output put in
//! order, a pipeline of tasks) through the native emulation of the host, against the whole
//! decoding: on files whose metadata and streams are far past the windows (a picture of
//! megabytes, junk between frames, an ID3 tag), and with every stream damaged, cut and extended
//! in turn.

use aqr_api::Window;
use aqr_api::emu::{self, Entry, Kind};
use aqr_flac::{flac, module};
use aqr_formula::Apply;

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
        Window::Output(0, module::WINDOW),
    ],
    arity: 12,
    call: |a| module::decode_part(a[0], a[1], [a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10]], a[11]),
};

#[derive(Clone)]
struct Case {
    params: Vec<u8>,
    resource: Vec<u8>,
    inputs: Vec<Vec<u8>>,
    len: u64,
}

impl Case {
    fn of(file: &[u8]) -> Case {
        let f = aqr_flac::formula::encode(file, 1).expect("coded");
        let got = std::sync::Mutex::new(None);
        let r = f.eval(&|a: &Apply, inputs: &[&[u8]]| {
            *got.lock().unwrap() = Some(Case {
                params: a.params.clone(),
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
        module::decode(&self.params, &[&self.resource], &inputs, self.len, threads).map_err(|e| e.0)
    }

    fn part(&self) -> Result<Vec<u8>, String> {
        let inputs: Vec<&[u8]> = self.inputs.iter().map(|i| &i[..]).collect();
        let out = emu::apply(&ENTRY, &inputs, &[&self.resource], &self.params, &[self.len]);
        out.map(|mut o| o.pop().unwrap()).map_err(|f| f.what)
    }
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// A file made of `d`: its metadata, a picture block of `picture` bytes after it, and its frames
/// `reps` times; with an ID3 tag before where `id3`.
fn big(d: &[u8], reps: usize, picture: usize, id3: bool) -> Vec<u8> {
    let (_, start) = flac::parse_metadata(d).unwrap();
    let mut meta = d[..start].to_vec();
    // (the last block of the metadata is no longer last)
    let mut p = 4;
    loop {
        let len = (meta[p + 1] as usize) << 16 | (meta[p + 2] as usize) << 8 | meta[p + 3] as usize;
        if meta[p] & 0x80 != 0 {
            meta[p] &= 0x7f;
            break;
        }
        p += 4 + len;
    }
    meta.extend_from_slice(&[0x86, (picture >> 16) as u8, (picture >> 8) as u8, picture as u8]);
    let mut r = 1u32;
    meta.extend((0..picture).map(|_| {
        r = r.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (r >> 24) as u8
    }));
    let mut out = Vec::new();
    if id3 {
        out.extend_from_slice(b"ID3\x04\0\0\0\0\x01\x00");
        out.extend((0..128).map(|i| i as u8));
    }
    out.extend_from_slice(&meta);
    for _ in 0..reps {
        out.extend_from_slice(&d[start..]);
    }
    out
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
    }
}

#[test]
fn through_windows() {
    // (a window is 1 MiB: the picture, the junk and the coded streams are over several of them)
    for (name, reps, picture, id3) in [
        ("s16_f5.flac", 300, 3 << 20, false),
        ("s16_junk.flac", 150, 1 << 20, true),
        ("s24_f8.flac", 120, 70_000, false),
        ("c6.flac", 100, 0, true),
        ("s32.flac", 100, 2 << 20, false),
    ] {
        let d = big(&fixture(name), reps, picture, id3);
        let c = Case::of(&d);
        let longest = *c.lens().iter().max().unwrap();
        eprintln!("{name}: {} bytes, inputs of {:?}", d.len(), c.lens());
        assert!(c.whole(1).unwrap() == d, "{name}: whole");
        assert!(c.part().unwrap_or_else(|e| panic!("{name}: {e}")) == d, "{name}: part");
        assert!(longest > (module::WINDOW as u64) << 16, "{name}: past a window");
    }
}

#[test]
fn damaged_cut_extended() {
    let d = big(&fixture("s16_junk.flac"), 3, 5000, true);
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
            if k == 6 {
                bad.inputs[k].truncate(cut / 4 * 4);
            }
            same(&format!("stream {k} cut to {cut}"), &bad);
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
    let c = Case::of(&big(&fixture("s16_f5.flac"), 2, 100, false));
    let small = module::plan(&c.params, &[c.resource.len() as u64], &c.lens(), c.len, 1 << 20).unwrap();
    let big = module::plan(&c.params, &[c.resource.len() as u64], &[1 << 30; 9], 1 << 32, 1 << 20);
    // (the room of the frames is the params')
    assert!(big.is_none() || big == Some(small));
}
