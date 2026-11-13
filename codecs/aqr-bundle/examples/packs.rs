//! What the readers of game packs (`aqr_bundle::packs`) take of files: for each, its pack, the
//! files whose contents are in it and their bytes (of them compressed, as `Opaque`), the rest,
//! and whether the formula evaluated gives the file back (of files up to 2 GiB; larger ones are
//! read and laid out only).
//!
//! ```sh
//! cargo run --release -p aqr-bundle --example aqr-bundle-packs -- FILE...
//! ```

use aqr_bundle::packs;

fn main() {
    let none = |_: &aqr_formula::Apply, _: &[&[u8]]| -> aqr_formula::Result<Vec<u8>> { unreachable!("no module") };
    for p in std::env::args().skip(1) {
        let Ok(d) = std::fs::read(&p) else {
            println!("{p}: not read");
            continue;
        };
        let Some(pack) = packs::detect(&d) else {
            println!("{p}: no pack");
            continue;
        };
        let t = std::time::Instant::now();
        let Some(files) = pack.files(&d) else {
            println!("{p}: {} not read", pack.name());
            continue;
        };
        let pieces = aqr_disk::pieces(&d, &files);
        let (mut taken, mut opaque) = (0, 0);
        for q in &pieces {
            if let Some((i, _)) = q.file {
                taken += q.span.len();
                if files[i].compressed.is_some() {
                    opaque += q.span.len();
                }
            }
        }
        let checked = match d.len() <= 2 << 30 {
            true => match packs::encode_with(pack, &d, |_| aqr_formula::Kind::Unknown) {
                Some(f) => {
                    if f.eval(&none).is_ok_and(|e| e == d) {
                        "rebuilt"
                    } else {
                        "NOT REBUILT"
                    }
                }
                None => "no formula",
            },
            false => "not evaluated",
        };
        println!(
            "{p}: {} {} files, {:.1} of {:.1} MB in files ({:.1} MB opaque), rest {:.2} MB; {checked}, {:.2} s",
            pack.name(),
            files.len(),
            taken as f64 / 1e6,
            d.len() as f64 / 1e6,
            opaque as f64 / 1e6,
            (d.len() - taken) as f64 / 1e6,
            t.elapsed().as_secs_f64()
        );
    }
}
