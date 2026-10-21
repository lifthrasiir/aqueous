// Model throughput on the RGBA layers of a file: model alone, encoding, decoding (profiling aid).
use aqr_psd::model::{GroupCoder, Player, Recorder};
use aqr_psd::rows::PlaneRows;
use std::time::Instant;

/// Decodes by replaying recorded symbols: the model's own cost on the decoding side.
struct Replay {
    syms: Vec<u8>,
    i: usize,
}
impl aqr_psd::model::Coder for Replay {
    const DECODES: bool = true;
    fn code(&mut self, _t: usize, _sym: u8) -> u8 {
        let s = self.syms[self.i];
        self.i += 1;
        s
    }
}

/// Records symbols and their tables.
struct Tape(Vec<u8>, Vec<usize>);
impl aqr_psd::model::Coder for Tape {
    const DECODES: bool = false;
    fn code(&mut self, t: usize, sym: u8) -> u8 {
        self.0.push(sym);
        self.1.push(t);
        sym
    }
}

struct Null;
impl aqr_psd::model::Coder for Null {
    const DECODES: bool = false;
    fn code(&mut self, _t: usize, sym: u8) -> u8 {
        sym
    }
}

fn main() {
    let f = std::env::args().nth(1).unwrap();
    let d = std::fs::read(&f).unwrap();
    let psd = aqr_psd::parse(&d).unwrap();
    // Planes of every RGB(A) layer, decoded.
    let mut groups: Vec<(usize, Vec<Vec<Vec<u8>>>)> = Vec::new();
    for (i, l) in psd.layer_info.layers.iter().enumerate() {
        let find = |id: i16| l.channels.iter().position(|c| c.id == id);
        let ids: Vec<usize> = [0, 1, 2, -1].iter().filter_map(|&id| find(id)).collect();
        if ids.len() < 3 || l.rect.width() == 0 {
            continue;
        }
        let w = l.rect.width() as usize;
        let mut rs: Vec<PlaneRows> = ids.iter().map(|&j| PlaneRows::channel(&psd, &d, i, j).unwrap()).collect();
        let mut rows = Vec::new();
        for _ in 0..l.rect.height() {
            rows.push(
                rs.iter_mut()
                    .map(|r| {
                        let mut v = vec![0; w];
                        r.next(&mut v).unwrap();
                        v
                    })
                    .collect(),
            );
        }
        groups.push((w, rows));
    }
    let samples: usize = groups.iter().map(|g| g.0 * g.1.len() * g.1[0].len()).sum();
    let once = |c: &mut dyn FnMut(&mut GroupCoder, &mut Vec<Vec<u8>>)| {
        let t = Instant::now();
        for (w, rows) in &groups {
            let n = rows[0].len();
            let mut gc = GroupCoder::new(*w, n, n == 4, vec![false; n], vec![None; n]);
            for r in rows {
                let mut r = r.clone();
                c(&mut gc, &mut r);
            }
        }
        t.elapsed().as_secs_f64()
    };
    let best = |f: &mut dyn FnMut() -> f64| (0..3).map(|_| f()).fold(f64::MAX, f64::min);
    let tm = best(&mut || once(&mut |gc, r| gc.code_row(&mut Null, r, None)));
    let mut out = Vec::new();
    let mut syms = 0;
    let te = best(&mut || {
        out.clear();
        let mut rec = Recorder::new(&mut out);
        let t = once(&mut |gc, r| gc.code_row(&mut rec, r, None));
        syms = rec.finish();
        t
    });
    let td = best(&mut || {
        let mut p = Player::new(&out);
        once(&mut |gc, r| gc.code_row(&mut p, r, None))
    });
    let mut tape = Tape(Vec::new(), Vec::new());
    once(&mut |gc, r| gc.code_row(&mut tape, r, None));
    let tr = best(&mut || {
        let mut p = Replay { syms: tape.0.clone(), i: 0 };
        once(&mut |gc, r| gc.code_row(&mut p, r, None))
    });
    // The entropy decoder alone, on the same symbols with their tables known in advance.
    let tt = best(&mut || {
        let mut p = Player::new(&out);
        let t = Instant::now();
        let mut acc = 0u32;
        for &tab in &tape.1 {
            acc = acc.wrapping_add(aqr_psd::model::Coder::code(&mut p, tab, 0) as u32);
        }
        std::hint::black_box(acc);
        t.elapsed().as_secs_f64()
    });
    println!("entropy decoding alone: {:.2} ns/sym", tt * 1e9 / syms as f64);
    // The bare rANS decoder on the same symbols, one table set (no chunks, no remapping).
    {
        use aqr_psd::rans::{self, BitW, Decoder, Tables};
        let nt = aqr_psd::model::TABLES;
        let mut counts = vec![vec![0u32; 256]; nt];
        let syms: Vec<(u16, u16)> = tape.1.iter().zip(&tape.0).map(|(&t, &s)| (t as u16, s as u16)).collect();
        for &(t, s) in &syms {
            counts[t as usize][s as usize] += 1;
        }
        let mut w = BitW::default();
        let freqs: Vec<Vec<u32>> = counts
            .iter()
            .map(|c| {
                if c.iter().any(|&x| x > 0) {
                    rans::write_table(&mut w, c)
                } else {
                    let mut f = vec![0; 256];
                    f[0] = rans::PROB_SCALE;
                    f
                }
            })
            .collect();
        let bytes = rans::encode(&syms, &freqs);
        let tables = Tables::new(&freqs);
        let tb = best(&mut || {
            let t = Instant::now();
            let mut d = Decoder::new(&bytes);
            let mut acc = 0u32;
            for &(tab, _) in &syms {
                acc = acc.wrapping_add(d.get(&tables, tab as usize));
            }
            std::hint::black_box(acc);
            t.elapsed().as_secs_f64()
        });
        println!("bare rANS decoding: {:.2} ns/sym", tb * 1e9 / syms.len() as f64);
    }
    println!(
        "{samples} samples, {syms} symbols: model {:.2} ns/sym, encode {:.2}, decode {:.2}, replay {:.2} (incl. row clones)",
        tm * 1e9 / syms as f64,
        te * 1e9 / syms as f64,
        td * 1e9 / syms as f64,
        tr * 1e9 / syms as f64
    );
}
