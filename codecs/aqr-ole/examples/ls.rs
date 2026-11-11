//! `aqr-ole-ls FILE [STREAM OUT]`: the streams of a compound file with their runs, or one
//! stream written to `OUT`.

use aqr_disk::{Image, Plain};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let d = std::fs::read(&args[0]).expect("read");
    let files = aqr_ole::files(&Plain(&d));
    if let [_, name, out] = &args[..] {
        let f = files.iter().find(|f| f.name == name.as_bytes()).expect("no such stream");
        let bytes: Vec<u8> = f.extents.iter().flat_map(|r| Plain(&d).cut(r.clone()).into_owned()).collect();
        std::fs::write(out, bytes).expect("write");
        return;
    }
    for f in &files {
        let len: usize = f.extents.iter().map(|r| r.len()).sum();
        println!("{:?} {} bytes in {} runs", String::from_utf8_lossy(&f.name), len, f.extents.len());
    }
}
