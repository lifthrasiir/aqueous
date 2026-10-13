//! How concentrated symbol usage is over contexts.
fn main() {
    let mut c = mp3_recons::model::Counter::new();
    for a in std::env::args().skip(1) {
        for e in std::fs::read_dir(&a).unwrap().flatten() {
            mp3_recons::codec::count(&std::fs::read(e.path()).unwrap(), &mut c);
        }
    }
    let mut per: Vec<(u64, usize)> = c.counts.iter().enumerate().map(|(i, v)| (v.iter().sum::<u64>(), i)).collect();
    per.sort_by(|a, b| b.cmp(a));
    let total: u64 = per.iter().map(|p| p.0).sum();
    let mut acc = 0;
    for (k, &(n, _)) in per.iter().enumerate() {
        acc += n;
        if [10, 30, 100, 200, 400, 800, 1600].contains(&(k + 1)) {
            println!("top {:5} contexts: {:.1}% of {} symbols", k + 1, 100.0 * acc as f64 / total as f64, total);
        }
    }
    let tables = mp3_recons::codec::tables();
    let _ = tables;
}
