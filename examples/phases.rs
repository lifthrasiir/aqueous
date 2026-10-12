//! Times the phases of compression and decompression over the given files.
use std::time::Instant;

fn main() {
    let mut paths = Vec::new();
    for a in std::env::args().skip(1) {
        for e in std::fs::read_dir(&a)
            .map(|r| r.flatten().map(|e| e.path()).collect::<Vec<_>>())
            .unwrap_or_else(|_| vec![a.clone().into()])
        {
            paths.push(e);
        }
    }
    let (mut tin, mut t_seg, mut t_enc, mut t_zstd, mut t_dec, mut t_unz) = (0usize, 0f64, 0f64, 0f64, 0f64, 0f64);
    for p in &paths {
        let d = std::fs::read(p).unwrap();
        tin += d.len();
        let t = Instant::now();
        let _ = mp3_recons::stream::segment_pub(&d);
        t_seg += t.elapsed().as_secs_f64();
        let t = Instant::now();
        let c = mp3_recons::codec::encode(&d);
        t_enc += t.elapsed().as_secs_f64();
        let t = Instant::now();
        let zs: Vec<Vec<u8>> = c.streams.iter().map(|b| zstd::bulk::compress(b, 9).unwrap()).collect();
        t_zstd += t.elapsed().as_secs_f64();
        let t = Instant::now();
        for (z, b) in zs.iter().zip(&c.streams) {
            let _ = zstd::bulk::decompress(z, b.len()).unwrap();
        }
        t_unz += t.elapsed().as_secs_f64();
        let t = Instant::now();
        let out = mp3_recons::codec::decode(&c).unwrap();
        t_dec += t.elapsed().as_secs_f64();
        assert_eq!(out, d);
    }
    let mb = |t: f64| tin as f64 / t / 1e6;
    println!(
        "segment {:.0} MB/s, encode {:.0} MB/s, zstd {:.0} MB/s, unzstd {:.0} MB/s, decode {:.0} MB/s",
        mb(t_seg),
        mb(t_enc),
        mb(t_zstd),
        mb(t_unz),
        mb(t_dec)
    );
}
