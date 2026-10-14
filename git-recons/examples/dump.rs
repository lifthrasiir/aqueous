//! Writes each stream of a directory's deconstruction to OUT/<name>.
fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let d = git_recons::deconstruct(std::path::Path::new(&a[0]), &Default::default()).unwrap();
    std::fs::create_dir_all(&a[1]).unwrap();
    for (k, s) in d.streams.iter().enumerate() {
        std::fs::write(format!("{}/{}", a[1], git_recons::STREAM_NAMES[k]), s).unwrap();
    }
}
