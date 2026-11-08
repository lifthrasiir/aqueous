//! `pcm-recons` as a part module (windows of 16 pages, the inputs taken and the output put in
//! order, a pipeline of tasks) through the native emulation of the host, against the whole
//! decoding: on samples whose streams are far past the windows, in every format, and with every
//! stream damaged, cut and extended in turn.

use aqr_api::emu::{self, Entry, Kind};
use aqr_api::{Window, stream};
use aqr_formula::Apply;
use aqr_raw::audio::pcm::Format;
use aqr_raw::container::Layout;
use aqr_raw::formula;
use aqr_raw::module::pcm as module;

const ENTRY: Entry = Entry {
    kind: Kind::Part,
    windows: &[
        Window::Input(0, module::WINDOW),
        Window::Input(1, module::WINDOW),
        Window::Input(2, module::WINDOW),
        Window::Input(3, module::WINDOW),
        Window::Input(4, module::WINDOW),
        Window::Input(5, module::WINDOW),
        Window::Output(0, module::WINDOW),
    ],
    arity: 9,
    call: |a| module::decode_part(a[0], a[1], [a[2], a[3], a[4], a[5], a[6], a[7]], a[8]),
};

/// The application's pieces: params, its resource and the six inputs.
struct Case {
    params: Vec<u8>,
    resource: Vec<u8>,
    inputs: Vec<Vec<u8>>,
    len: u64,
}

impl Case {
    fn of(samples: &[u8], fmt: Format) -> Case {
        let f = formula::encode_samples(samples, Layout::Pcm(fmt), 1).expect("coded");
        let got = std::sync::Mutex::new(None);
        let r = f.eval(&|a: &Apply, inputs: &[&[u8]]| {
            *got.lock().unwrap() = Some(Case {
                params: a.params.clone(),
                resource: a.resources[0].to_vec(),
                inputs: inputs.iter().map(|i| i.to_vec()).collect(),
                len: a.len,
            });
            Ok(samples[..a.len as usize].to_vec())
        });
        assert!(r.is_ok());
        got.into_inner().unwrap().expect("an application")
    }

    fn lens(&self) -> Vec<u64> {
        self.inputs.iter().map(|i| i.len() as u64).collect()
    }

    /// The module whole on `threads` threads.
    fn whole(&self, threads: usize) -> Result<Vec<u8>, String> {
        let inputs: Vec<&[u8]> = self.inputs.iter().map(|i| &i[..]).collect();
        module::decode(&self.params, &[&self.resource], &inputs, self.len, threads).map_err(|e| e.0)
    }

    /// The module as the part module, in the emulation.
    fn part(&self) -> Result<Vec<u8>, String> {
        let inputs: Vec<&[u8]> = self.inputs.iter().map(|i| &i[..]).collect();
        let out = emu::apply(&ENTRY, &inputs, &[&self.resource], &self.params, &[self.len]);
        out.map(|mut o| o.pop().unwrap()).map_err(|f| f.what)
    }

    /// The plan holds the same for any length of the inputs.
    fn plan(&self, lens: &[u64], len: u64) -> Option<((u64, u64), u64)> {
        module::plan(&self.params, &[self.resource.len() as u64], lens, len, 1 << 20)
    }
}

/// `n` sample frames of `channels` channels of `bytes` bytes: tones and noise, with silence, a
/// constant run and full-scale noise (verbatim subframes) along the way.
fn samples(n: usize, channels: usize, bytes: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(n * channels * bytes);
    let mut r = 1u32;
    let bits = 8 * bytes as u32;
    for i in 0..n {
        for c in 0..channels {
            r = r.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let x = match (i / 6000) % 4 {
                0 | 1 => {
                    (((i as f64 * (0.01 + 0.003 * c as f64)).sin() * 0.3) * (1u64 << (bits - 1)) as f64) as i64
                        + (r >> 20) as i64
                        - 2048
                }
                2 if i % 6000 < 3000 => 0,
                2 => 1234,
                _ => (r as i64) >> (32 - bits),
            };
            let x = if bytes == 1 { (x + 128) as u64 } else { x as u64 };
            out.extend_from_slice(&x.to_le_bytes()[..bytes]);
        }
    }
    out
}

fn format(channels: usize, bytes: usize) -> Format {
    Format { channels, bytes, big_endian: false, unsigned: bytes == 1 }
}

#[test]
fn through_windows() {
    // (a window is 1 MiB: the coded streams of the first are over many of them)
    for (n, ch, bytes) in [(8_000_000, 2, 2), (300_000, 1, 1), (150_000, 6, 3), (200_000, 2, 4), (100_000, 8, 2)] {
        let d = samples(n, ch, bytes);
        let c = Case::of(&d, format(ch, bytes));
        let whole = c.whole(1).unwrap();
        assert!(whole == d, "{n}x{ch}x{bytes}: whole");
        let part = c.part().unwrap_or_else(|e| panic!("{n}x{ch}x{bytes}: {e}"));
        assert!(part == d, "{n}x{ch}x{bytes}: part");
        let longest = *c.lens().iter().max().unwrap();
        eprintln!("{n}x{ch}x{bytes}: {} byte inputs at most, windows of {}", longest, (module::WINDOW as u64) << 16);
        if ch == 2 && bytes == 2 {
            assert!(longest > 2 * ((module::WINDOW as u64) << 16), "past the windows many times");
        }
    }
}

#[test]
fn short_and_odd() {
    for (n, ch, bytes) in [(1, 2, 2), (4095, 2, 2), (4096, 2, 2), (4097, 1, 1), (10, 3, 3), (0, 2, 2)] {
        let d = samples(n, ch, bytes);
        let f = formula::encode_samples(&d, Layout::Pcm(format(ch, bytes)), 1);
        let Some(_) = f else { continue };
        let c = Case::of(&d, format(ch, bytes));
        assert!(c.part().unwrap() == d, "{n}x{ch}x{bytes}");
    }
}

#[test]
fn damaged_cut_extended() {
    let d = samples(120_000, 2, 2);
    let c = Case::of(&d, format(2, 2));
    let same = |what: &str, c: &Case| {
        let (w, p) = (c.whole(1), c.part());
        assert_eq!(w.is_ok(), p.is_ok(), "{what}: whole {:?}, part {:?}", w.as_ref().err(), p.as_ref().err());
        if let (Ok(w), Ok(p)) = (w, p) {
            assert!(w == p, "{what}");
        }
    };
    same("intact", &c);
    for k in 0..c.inputs.len() {
        let n = c.inputs[k].len();
        if n == 0 {
            continue;
        }
        for at in [1, 2, 3] {
            let mut bad =
                Case { inputs: c.inputs.clone(), params: c.params.clone(), resource: c.resource.clone(), len: c.len };
            bad.inputs[k][n * at / 4] ^= 0x10;
            same(&format!("stream {k} damaged at {at}/4"), &bad);
        }
        for cut in [0, n / 3, n - 1] {
            let mut bad =
                Case { inputs: c.inputs.clone(), params: c.params.clone(), resource: c.resource.clone(), len: c.len };
            bad.inputs[k].truncate(cut);
            // (the rANS words are a multiple of 4)
            if k == 3 {
                bad.inputs[k].truncate(cut / 4 * 4);
            }
            same(&format!("stream {k} cut to {cut}"), &bad);
        }
        let mut bad =
            Case { inputs: c.inputs.clone(), params: c.params.clone(), resource: c.resource.clone(), len: c.len };
        bad.inputs[k].extend_from_slice(&[0; 16]);
        same(&format!("stream {k} extended"), &bad);
    }
    // another length of samples
    for len in [c.len - 4, c.len + 4, c.len - 4096 * 4, 0] {
        let bad = Case { inputs: c.inputs.clone(), params: c.params.clone(), resource: c.resource.clone(), len };
        same(&format!("length {len}"), &bad);
    }
}

#[test]
fn plan_does_not_follow_lengths() {
    let d = samples(50_000, 2, 2);
    let c = Case::of(&d, format(2, 2));
    let small = c.plan(&c.lens(), c.len).unwrap();
    let big = c.plan(&[1 << 30; 6], 1 << 32).unwrap();
    assert_eq!(small, big);
    let _ = stream::Slice(&[]);
}
