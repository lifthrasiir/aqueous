use super::*;
use aqr_decompress::FLATE;

/// The table sample is all of a small text, else windows from the start to the end of the
/// text, of whole lines.
#[test]
fn table_sample_spreads() {
    let line = |i: usize| format!("{i:08};{};x\n", i * 7);
    let small: String = (0..1000).map(line).collect();
    assert!(matches!(table_sample(small.as_bytes()), Cow::Borrowed(s) if s == small.as_bytes()));
    let big: String = (0..300_000).map(line).collect();
    let (window, windows) = (TABLE_WINDOW.get(), TABLE_WINDOWS.get());
    assert!(big.len() > window * windows * 2);
    let s = table_sample(big.as_bytes());
    assert!(s.len() <= window * windows && s.len() > window * (windows - 1));
    assert!(s.starts_with(&big.as_bytes()[..30]));
    assert!(s.ends_with(&big.as_bytes()[big.len() - 30..]));
    let text = std::str::from_utf8(&s).unwrap();
    assert!(text.lines().all(|l| l.ends_with(";x")));
    // (the lines are in order, and some are from the middle)
    let ids: Vec<usize> = text.lines().map(|l| l[..8].parse().unwrap()).collect();
    assert!(ids.windows(2).all(|w| w[0] < w[1]));
    assert!(ids.iter().any(|&i| (140_000..160_000).contains(&i)));
}

/// A formula kept beside its data holds its streams but those of the alternatives within it,
/// held already as they were kept.
#[test]
fn kept_bytes_but_alternatives() {
    let mut f = Formula::default();
    let (raw, made, own) = (f.push("raw", vec![0; 100]), f.push("x", vec![1; 300]), f.push("y", vec![2; 50]));
    let either = Node::Either(Box::new(aqr_formula::Either { kept: raw, made, len: 100, sizes: (0, 0) }));
    f.root = Node::concat([either, own]);
    assert_eq!(kept_bytes(&f), 50);
    f.root = Node::concat([Node::slice(0, 0, 100), Node::slice(1, 0, 300)]);
    assert_eq!(kept_bytes(&f), 450);
}

/// A scan whose files were all kept beside their data is not compared with the data (which
/// it is, with them kept): it is kept beside it in turn, so that its parent keeps the data as
/// it would have, had the files been turned down.
#[test]
fn scan_of_kept_files_kept() {
    let d = aqr_common::rng::Rng::new(7).bytes(100_000);
    let mut f = Formula::default();
    let raw = f.push("raw", d.clone());
    let made = f.push("x", d.clone());
    f.root = Node::Either(Box::new(aqr_formula::Either { kept: raw, made, len: d.len() as u64, sizes: (0, 0) }));
    let all = 0..d.len();
    let m = Made { f, gaps: Some(vec![all]), close: None };
    // (none compared: a scan would be taken whatever its size)
    let opt = Options { smaller: false, ..Options::default() };
    let cx = Cx::new(&d, &opt);
    cx.defers.set(true);
    let (taken, close) = scanned(Some(m), &d, &opt, &cx);
    assert!(taken.is_none());
    assert_eq!(close.and_then(|c| c.close), Some((0, d.len())));
}

thread_local! {
    /// The formats given to recompressors, in order.
    pub(crate) static TRIED: std::cell::RefCell<Vec<Format>> = const { std::cell::RefCell::new(Vec::new()) };
    /// The DEFLATE streams of gzip and zlib deconstructed.
    pub(crate) static DECONSTRUCTED: Cell<usize> = const { Cell::new(0) };
    /// The files of [`pure`] formats given to their recompressors.
    pub(crate) static PURE: Cell<usize> = const { Cell::new(0) };
    /// The lengths of the data compared with their formulas by [`smaller`].
    pub(crate) static COMPARED: std::cell::RefCell<Vec<usize>> = const { std::cell::RefCell::new(Vec::new()) };
    /// [`direct_min`] of the tests, where set.
    pub(crate) static DIRECT_MIN: Cell<Option<usize>> = const { Cell::new(None) };
    /// The room [`direct_room`] leaves, where set.
    pub(crate) static DIRECT_ROOM: Cell<Option<u64>> = const { Cell::new(None) };
    /// The archives written directly ([`written_direct`]).
    pub(crate) static DIRECTED: Cell<usize> = const { Cell::new(0) };
    /// The bytes of a segment of aqr-tensors ([`tensors`]), where set.
    pub(crate) static TENSORS_SEGMENT: Cell<Option<usize>> = const { Cell::new(None) };
}

/// The lengths of the texts given to aqr-wrt, on any thread.
pub(crate) static TEXTS: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());

// (an allocator that tells the heap of recompressions to their allowances, as the binaries'
// does: the system's, as some tests here expect of it)
#[global_allocator]
static ALLOC: aqr_common::mem::Counting = aqr_common::mem::Counting;

/// The recompressor of a format, given data of a length, takes more heap than one may at
/// its start ([`hog`]), on any thread.
static HOGS: std::sync::Mutex<Vec<(Format, usize)>> = std::sync::Mutex::new(Vec::new());

/// The heap that one recompression told of [`HOGS`] may take.
const HOG_LIMIT: u64 = 16 << 20;

/// The recompressor of a format, given data of a length, panics ([`panics`]), on any thread.
static PANICS: std::sync::Mutex<Vec<(Format, usize)>> = std::sync::Mutex::new(Vec::new());

/// Held by the tests that look at [`fallbacks`], of the process: one at a time.
pub(crate) static FALLBACKS_SEEN: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The heap a recompression as `format` of data of `len` bytes may take, where it takes
/// more at its start ([`HOGS`], [`attempted`]).
pub(crate) fn hog(format: Format, len: usize) -> Option<u64> {
    HOGS.lock().unwrap().contains(&(format, len)).then_some(HOG_LIMIT)
}

/// Whether the recompressor of `format`, given data of `len` bytes, panics at its start
/// ([`PANICS`], [`attempted`]).
pub(crate) fn panics(format: Format, len: usize) -> bool {
    PANICS.lock().unwrap().contains(&(format, len))
}

/// A recompression that panics is told ([`fallbacks`]) and the data is kept as it is (the
/// archive giving it back).
#[test]
fn panic_falls_back() {
    let text: Vec<u8> =
        (0..30013).flat_map(|i| format!("line {} of the panicking text\n", i * 7 % 1000).into_bytes()).collect();
    let mut z = flate2::GzBuilder::new().filename("text.txt").write(Vec::new(), flate2::Compression::new(6));
    std::io::Write::write_all(&mut z, &text).unwrap();
    let gz = z.finish().unwrap();
    let _seen = FALLBACKS_SEEN.lock().unwrap_or_else(|e| e.into_inner());
    let opt = Options { smaller: false, ..Options::default() };
    let made = |d: &[u8]| {
        let f = recompress(d, &opt);
        assert!(f.eval(&Native::default()).is_ok_and(|b| b == d));
        f.applies().len()
    };
    assert!(made(&gz) > 0);
    let _ = fallbacks();
    PANICS.lock().unwrap().push((Format::Gzip, gz.len()));
    let kept = made(&gz);
    PANICS.lock().unwrap().clear();
    assert_eq!(kept, 0, "kept as it is");
    let told: Vec<_> = fallbacks().into_iter().filter(|f| f.bytes == gz.len() as u64).collect();
    assert!(
        told.len() == 1
            && told[0].module == "gzip"
            && told[0].panicked
            && told[0].message == "a bug of the recompressor",
        "{told:?}"
    );
    assert!(!told[0].heap && !told[0].kept);
}

/// A recompression that takes more heap than one may gives up, and the data is kept as it
/// is (the archive giving it back), what gave up told ([`fallbacks`]); a recompression
/// within it (of its contents) that does so gives up alone.
#[test]
fn heap_over_falls_back() {
    let text: Vec<u8> = babble(30011);
    let gz = |t: &[u8]| {
        let mut z = flate2::GzBuilder::new().filename("text.txt").write(Vec::new(), flate2::Compression::new(6));
        std::io::Write::write_all(&mut z, t).unwrap();
        z.finish().unwrap()
    };
    let outer = gz(&text);
    let _seen = FALLBACKS_SEEN.lock().unwrap_or_else(|e| e.into_inner());
    let opt = Options { smaller: false, ..Options::default() };
    let made = |d: &[u8]| {
        let f = recompress(d, &opt);
        assert!(f.eval(&Native::default()).is_ok_and(|b| b == d));
        f.applies().iter().map(|a| a.module.name).collect::<Vec<_>>()
    };
    let mine = |told: Vec<Fallback>, len: usize| -> Vec<Fallback> {
        told.into_iter().filter(|f| f.heap && f.bytes == len as u64).collect()
    };
    assert!(!made(&outer).is_empty());
    HOGS.lock().unwrap().push((Format::Gzip, outer.len()));
    assert!(made(&outer).is_empty(), "kept as it is");
    let told = mine(fallbacks(), outer.len());
    assert!(told.len() == 1 && told[0].module == "gzip" && told[0].needed > HOG_LIMIT, "{told:?}");
    // (the text in it given up alone: the gzip stream deconstructed still)
    HOGS.lock().unwrap().retain(|h| h.1 != outer.len());
    HOGS.lock().unwrap().push((Format::Text, text.len()));
    let modules = made(&outer);
    assert!(!modules.is_empty() && !modules.iter().any(|m| m.contains("wrt")), "{modules:?}");
    let told = mine(fallbacks(), text.len());
    assert!(told.len() == 1 && told[0].module == "text", "{told:?}");
    HOGS.lock().unwrap().clear();
}

/// An ar, tar or cpio archive is worth what the files in it are: a probe counts them, not
/// the archive, and scans data of its kind.
#[test]
fn bundles_probed_by_members() {
    let fixture = |p: &str| std::fs::read(format!("{}/../../{p}", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let tar = fixture("codecs/aqr-bundle/tests/data/py-gnu.tar");
    assert_eq!(files_len(&tar), 0);
    let jpeg = fixture("codecs/aqr-jpeg/tests/data/gradient.jpg");
    let mut with = tar[..1024].to_vec();
    with.extend_from_slice(&jpeg);
    with.extend_from_slice(&tar[1024..]);
    assert_eq!(files_len(&with), jpeg.len());
    let probe = probe(&Options::default(), 0).unwrap();
    assert_eq!((probe.look)(Kind::Format("tar")), Look::Scan);
    assert_eq!((probe.look)(Kind::Format("jpeg")), Look::Whole);
}

/// An ISO 9660 or UDF image is taken apart into its files, recompressed in turn: a JPEG file
/// in it by aqr-jpeg.
#[test]
fn images_taken_apart() {
    let fixture = |p: &str| std::fs::read(format!("{}/../../{p}", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let z = fixture("codecs/aqr-iso/tests/data/udf201.iso.zst");
    let mut d = zstd::stream::decode_all(&z[..]).unwrap();
    let jpeg = fixture("codecs/aqr-jpeg/tests/data/gradient.jpg");
    let files = aqr_disk::files(&d, bundle::FILE_SYSTEMS);
    let at = files.iter().find(|f| f.name == b"bin/data.bin").unwrap().extents[0].start;
    d[at..at + jpeg.len()].copy_from_slice(&jpeg);
    assert_eq!(detect(&d), Some(Format::Disk));
    // (the modules the archive's anyway)
    let f = recompress(&d, &Options { shared_modules: true, ..Options::default() });
    assert!(f.streams.iter().any(|s| s.class == aqr_disk::formula::REST));
    assert!(f.applies().iter().any(|a| a.module == aqr_decompress::JPEG));
    assert!(f.eval(&Native::default()).unwrap() == d);
    // the files of an image known to the store (mapped) shared, not copied: the same archive
    let plain = compress(&d, &Options::default());
    let shared = aqr_formula::Bytes::shared(Arc::new(d));
    let _known = aqr_formula::store::known(&shared);
    let f = bundle::disk(&shared, 1).unwrap();
    let large: Vec<_> = f.streams.iter().filter(|s| s.content && s.data.len() >= aqr_formula::store::MIN).collect();
    assert!(!large.is_empty() && large.iter().all(|s| s.data.is_shared()));
    assert!(compress(&shared, &Options::default()) == plain);
    // a file all of its stream takes its bytes, cut out of them as it is made (let go of
    // then): the JPEG file's stream empty, its formula of no gaps left
    let mut f = bundle::disk(&shared, 1).unwrap();
    let opt = Options { shared_modules: true, ..Options::default() };
    let cx = Cx::new(&shared, &opt);
    let (children, _) = nested(&mut f, &shared, &opt, 0, &cx).unwrap();
    let (n, child) = children.iter().find(|c| c.1.as_ref().is_some_and(|c| !c.f.applies().is_empty())).unwrap();
    let &Node::Slice { stream, .. } = n else { panic!("a slice") };
    assert!(f.streams[stream].data.is_empty() && child.as_ref().unwrap().gaps.is_none());
}

/// A WIM file whose bytes are known to the store (mapped) has its data made as each content
/// is come to rather than held ([`wim`]): the same archive as where it is not, giving the
/// file back.
#[test]
fn wim_data_made_as_needed() {
    let d = std::fs::read(format!("{}/../../codecs/aqr-wim/tests/data/lzx.wim", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let opt = Options { threads: 2, ..Options::default() };
    let plain = compress(&d, &opt);
    let shared = aqr_formula::Bytes::shared(Arc::new(d.clone()));
    let _known = aqr_formula::store::known(&shared);
    assert!(wim(&shared, 2).unwrap().streams.iter().any(|s| s.data.is_lazy()));
    assert!(compress(&shared, &opt) == plain);
    assert!(aqr_decompress::decompress(&plain, 1).unwrap() == d);
}

/// `d` as a file mapped (of a file made for it and gone), and the bytes of the mapping
/// resident in the process (Linux: its `Rss` in `/proc/self/smaps`).
#[cfg(target_os = "linux")]
fn mapped(d: &[u8]) -> (aqr_formula::Bytes, impl Fn(&aqr_formula::Bytes) -> usize) {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("aqr-compress-mapped-{}-{n}", std::process::id()));
    std::fs::write(&path, d).unwrap();
    // SAFETY: a file of this test's alone, not changed while mapped
    let map = unsafe { memmap2::Mmap::map(&std::fs::File::open(&path).unwrap()) }.unwrap();
    std::fs::remove_file(&path).unwrap();
    let rss = |b: &aqr_formula::Bytes| {
        let at = b.as_ptr() as usize;
        let smaps = std::fs::read_to_string("/proc/self/smaps").unwrap();
        let mut lines = smaps.lines().skip_while(|l| {
            let Some((a, b)) = l.split(' ').next().and_then(|r| r.split_once('-')) else { return true };
            let (a, b) = (usize::from_str_radix(a, 16), usize::from_str_radix(b, 16));
            !matches!((a, b), (Ok(a), Ok(b)) if (a..b).contains(&at))
        });
        lines.next().expect("the mapping");
        let rss = lines.find_map(|l| l.strip_prefix("Rss:")).expect("its Rss");
        rss.trim().trim_end_matches(" kB").trim().parse::<usize>().unwrap() << 10
    };
    (aqr_formula::Bytes::shared(Arc::new(map)), rss)
}

/// Every page of `b` read.
#[cfg(target_os = "linux")]
fn touch(b: &[u8]) {
    b.iter().step_by(4096).for_each(|&x| _ = std::hint::black_box(x));
}

/// The pages of an input mapped are let go of as it is read through (its contents sealed, its
/// streams encoded, its bytes compared in verifying): not resident as compressing ends,
/// the archive the same.
#[cfg(target_os = "linux")]
#[test]
fn input_let_go() {
    let fixture = |p: &str| std::fs::read(format!("{}/../../{p}", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let d = zstd::stream::decode_all(&fixture("codecs/aqr-iso/tests/data/udf201.iso.zst")[..]).unwrap();
    let (mapped, rss) = mapped(&d);
    let opt = Options { threads: 2, ..Options::default() };
    let plain = compress(&d, &opt);
    let _known = aqr_formula::store::known_mapped(&mapped);
    touch(&mapped);
    assert!(rss(&mapped) >= d.len() / 2);
    assert!(compress(&mapped, &opt) == plain);
    assert!(rss(&mapped) < d.len() / 8);
    // and as it is checksummed (as the archive is encoded: of all of it)
    touch(&mapped);
    assert_eq!(through(&mapped, 2).sum, aqr_archive::checksum(&d));
    assert!(rss(&mapped) < d.len() / 8);
}

/// The chunks of a WIM file mapped are let go of once split, and once the data of each
/// application is made of them again.
#[cfg(target_os = "linux")]
#[test]
fn wim_chunks_let_go() {
    let d = std::fs::read(format!("{}/../../codecs/aqr-wim/tests/data/lzx.wim", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let (mapped, rss) = mapped(&d);
    let _known = aqr_formula::store::known_mapped(&mapped);
    touch(&mapped);
    assert!(rss(&mapped) >= d.len() / 2);
    let f = wim(&mapped, 2).unwrap();
    // (the pages of the headers and of what is between the resources are left)
    let left = rss(&mapped);
    assert!(left < d.len() / 2, "{left} of {}", d.len());
    for s in f.streams.iter().filter(|s| s.data.is_lazy()) {
        drop(s.data.made());
    }
    assert!(rss(&mapped) <= left);
}

/// Forgetting data the encoder has not compressed (an image found smaller as its formula)
/// reads none of it: not every page of an input mapped made resident again to hash it.
#[cfg(target_os = "linux")]
#[test]
fn forget_reads_nothing() {
    let d = vec![7; 1 << 20];
    let (mapped, rss) = mapped(&d);
    let cx = Cx::new(&mapped, &Options::default());
    let before = rss(&mapped);
    cx.enc.forget(&mapped);
    assert_eq!(rss(&mapped), before);
}

/// An archive mapped from its file, decoded with its blocks let go of as they are read
/// ([`aqr_archive::Archive::let_go_read`]): its pages not resident once its file is written, the file
/// the same.
#[cfg(target_os = "linux")]
#[test]
fn archive_let_go_read() {
    // two streams of noise (stored as they are), a formula of them; and noise alone (a leaf
    // stored)
    let mut rng = aqr_common::rng::Rng::new(7);
    let (a, b) = (rng.bytes(512 << 10), rng.bytes(512 << 10));
    let mut f = Formula::default();
    f.root = Node::concat([f.push("a", a.clone()), f.push("b", b.clone())]);
    let whole = [a.clone(), b.clone()].concat();
    let raw = Formula::raw("raw", whole.clone());
    for f in [f, raw] {
        let c = aqr_archive::encode(&f, None, aqr_archive::Level::MID).unwrap();
        let (mapped, rss) = mapped(&c);
        let native = Native { threads: 2 };
        let runner = aqr_decompress::natives(&native);
        let mut archive = aqr_archive::open(&mapped, &runner).unwrap();
        // SAFETY: a file mapped read only
        unsafe { archive.let_go_read() };
        let mut out = Vec::new();
        archive
            .write_file(2, &mut |p: &[u8]| {
                out.extend_from_slice(p);
                Ok(())
            })
            .unwrap();
        assert!(out == whole);
        assert!(rss(&mapped) < c.len() / 8, "{} of {}", rss(&mapped), c.len());
    }
}

/// The chunks of a WIM file mapped are let go of again once the contents of its data are
/// made and its applications prevalidated (hashing the chunks they make).
#[cfg(target_os = "linux")]
#[test]
fn wim_chunks_let_go_prevalidated() {
    let d = std::fs::read(format!("{}/../../codecs/aqr-wim/tests/data/lzx.wim", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let (mapped, rss) = mapped(&d);
    let _known = aqr_formula::store::known_mapped(&mapped);
    let opt = Options { threads: 2, ..Options::default() };
    let mut f = wim(&mapped, 2).unwrap();
    let left = rss(&mapped);
    let mut cx = Cx::new(&mapped, &opt);
    cx.pre = Some(Arc::new(verify::Prevalidator::spawn(2, &[&mapped])));
    touch(&mapped);
    assert!(rss(&mapped) > left);
    nested(&mut f, &mapped, &opt, 0, &cx);
    assert!(cx.parts_checked.get());
    assert!(rss(&mapped) <= left, "{} > {left}", rss(&mapped));
}

/// The files of an image archived alone are sealed into the sink as they are made: the
/// archive the same whatever the threads, and giving the image back.
#[test]
fn images_sealed() {
    let fixture = |p: &str| std::fs::read(format!("{}/../../{p}", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let mut d = zstd::stream::decode_all(&fixture("codecs/aqr-iso/tests/data/udf201.iso.zst")[..]).unwrap();
    let jpeg = fixture("codecs/aqr-jpeg/tests/data/gradient.jpg");
    let files = aqr_disk::files(&d, bundle::FILE_SYSTEMS);
    let at = files.iter().find(|f| f.name == b"bin/data.bin").unwrap().extents[0].start;
    d[at..at + jpeg.len()].copy_from_slice(&jpeg);
    let archived = |threads| compress_formula(&d, &Options { threads, shared_modules: true, ..Options::default() });
    let (c, f) = archived(1);
    assert!(f.streams.iter().any(|s| s.data.sunk_in().is_some()));
    assert!(f.applies().iter().any(|a| a.module == aqr_decompress::JPEG));
    assert!(aqr_decompress::decompress(&c, 1).unwrap() == d);
    assert!(archived(4).0 == c);
}

/// An image in data whose formula is compared with the data ([`smaller`]), a gzip member: its
/// files are not sealed (the comparison would read the streams sunk), and the archive gives
/// the data back.
#[test]
fn images_compared_not_sealed() {
    let fixture = |p: &str| std::fs::read(format!("{}/../../{p}", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let mut iso = zstd::stream::decode_all(&fixture("codecs/aqr-iso/tests/data/udf201.iso.zst")[..]).unwrap();
    let jpeg = fixture("codecs/aqr-jpeg/tests/data/gradient.jpg");
    let files = aqr_disk::files(&iso, bundle::FILE_SYSTEMS);
    let at = files.iter().find(|f| f.name == b"bin/data.bin").unwrap().extents[0].start;
    iso[at..at + jpeg.len()].copy_from_slice(&jpeg);
    let mut z = flate2::GzBuilder::new().filename("udf.iso").write(Vec::new(), flate2::Compression::new(6));
    std::io::Write::write_all(&mut z, &iso).unwrap();
    let d = z.finish().unwrap();
    let (c, f) = compress_formula(&d, &Options { threads: 2, shared_modules: true, ..Options::default() });
    assert!(f.streams.iter().all(|s| s.data.sunk_in().is_none()));
    assert!(f.applies().iter().any(|a| a.module == aqr_decompress::JPEG));
    assert!(aqr_decompress::decompress(&c, 1).unwrap() == d);
}

/// An image in a tar archive, the image's files sealed: where its formula passes the bound of
/// the description, it is kept raw, its bytes as they are rather than its formula evaluated
/// (which would read the streams sunk).
#[test]
fn sealed_over_the_bound_kept_raw() {
    let tar = tar_of;
    let fixture = |p: &str| std::fs::read(format!("{}/../../{p}", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let mut iso = zstd::stream::decode_all(&fixture("codecs/aqr-iso/tests/data/udf201.iso.zst")[..]).unwrap();
    let jpeg = fixture("codecs/aqr-jpeg/tests/data/gradient.jpg");
    let files = aqr_disk::files(&iso, bundle::FILE_SYSTEMS);
    let at = files.iter().find(|f| f.name == b"bin/data.bin").unwrap().extents[0].start;
    iso[at..at + jpeg.len()].copy_from_slice(&jpeg);
    let text = b"some text, ".repeat(1000);
    let d = tar(&[("udf.iso", &iso), ("c.txt", &text)]);
    assert_eq!(detect(&d), Some(Format::Tar));
    // (the JPEG file within the image's bound, 165 bytes, the image not within the tar
    // archive's, 196)
    let opt = Options { description: 180, threads: 2, shared_modules: true, ..Options::default() };
    let (c, f) = compress_formula(&d, &opt);
    assert!(f.applies().iter().all(|a| a.module != aqr_decompress::JPEG));
    assert!(aqr_decompress::decompress(&c, 1).unwrap() == d);
}

/// An image whose first file of no kind (stream 0) is taken by its content's formula, and
/// dropped from the image's, is still taken as an image once its contents are in place: not
/// compared with its data (which would compress all of it).
#[test]
fn images_not_compared() {
    let fixture = |p: &str| std::fs::read(format!("{}/../../{p}", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let mut d = zstd::stream::decode_all(&fixture("codecs/aqr-iso/tests/data/udf201.iso.zst")[..]).unwrap();
    let jpeg = fixture("codecs/aqr-jpeg/tests/data/gradient.jpg");
    let files = aqr_disk::files(&d, bundle::FILE_SYSTEMS);
    let first = aqr_disk::pieces(&d, &files)
        .into_iter()
        .find(|p| p.file.is_some_and(|(i, _)| kind_by_name(&files[i].name) == Kind::Unknown))
        .unwrap();
    assert!(first.span.len() >= jpeg.len());
    d[first.span.start..][..jpeg.len()].copy_from_slice(&jpeg);
    COMPARED.with(|c| c.borrow_mut().clear());
    let f = recompress(&d, &Options { shared_modules: true, ..Options::default() });
    assert!(f.applies().iter().any(|a| a.module == aqr_decompress::JPEG));
    assert!(f.eval(&Native::default()).unwrap() == d);
    assert!(!COMPARED.with(|c| c.borrow().contains(&d.len())));
}

/// A CAB file's MSZIP folder is taken apart into its files, recompressed in turn: a JPEG file
/// in it by aqr-jpeg.
#[test]
fn cabs_taken_apart() {
    let d =
        std::fs::read(format!("{}/../../codecs/aqr-mszip/tests/data/jpeg.cab", env!("CARGO_MANIFEST_DIR"))).unwrap();
    assert_eq!(detect(&d), Some(Format::Cab));
    // (the modules the archive's anyway)
    let f = recompress(&d, &Options { shared_modules: true, ..Options::default() });
    assert!(f.applies().iter().any(|a| a.module == aqr_decompress::MSZIP));
    assert!(f.applies().iter().any(|a| a.module == aqr_decompress::JPEG));
    assert!(f.eval(&Native::default()).unwrap() == d);
}

/// A CAB file's stored folder is its files' contents as they are, recompressed in turn: a
/// JPEG file in it by aqr-jpeg (installers' `Data1.cab` are often of stored folders).
#[test]
fn stored_cab_folders_taken_apart() {
    let jpeg =
        std::fs::read(format!("{}/../../codecs/aqr-jpeg/tests/data/gradient.jpg", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let name = b"photo.jpg\0";
    // the header, a folder of one stored block (at `data`), the file, the block
    let data = 36 + 8 + 16 + name.len();
    let len = data + 8 + jpeg.len();
    let mut d = Vec::with_capacity(len);
    d.extend_from_slice(b"MSCF\0\0\0\0");
    d.extend_from_slice(&(len as u32).to_le_bytes());
    d.extend_from_slice(&[0; 4]);
    d.extend_from_slice(&44u32.to_le_bytes()); // the first file's entry
    d.extend_from_slice(&[0; 4]);
    d.extend_from_slice(&[3, 1, 1, 0, 1, 0, 0, 0, 0, 0, 0, 0]); // 1.3, a folder, a file
    d.extend_from_slice(&(data as u32).to_le_bytes());
    d.extend_from_slice(&[1, 0, 0, 0]); // a block, stored
    d.extend_from_slice(&(jpeg.len() as u32).to_le_bytes());
    d.extend_from_slice(&[0; 6]); // at 0, of folder 0
    d.extend_from_slice(&[0x21, 0x5a, 0, 0, 0x20, 0]); // a date, a time, attributes
    d.extend_from_slice(name);
    d.extend_from_slice(&[0; 4]); // no checksum
    d.extend_from_slice(&(jpeg.len() as u16).to_le_bytes());
    d.extend_from_slice(&(jpeg.len() as u16).to_le_bytes());
    d.extend_from_slice(&jpeg);
    assert_eq!(detect(&d), Some(Format::Cab));
    let f = recompress(&d, &Options { shared_modules: true, ..Options::default() });
    assert!(f.applies().iter().any(|a| a.module == aqr_decompress::MSZIP));
    assert!(f.applies().iter().any(|a| a.module == aqr_decompress::JPEG));
    assert!(f.eval(&Native::default()).unwrap() == d);
}

/// LZX streams (CAB's folders, CHM's sections) are taken from level 9 only.
#[test]
fn lzx_from_level_9() {
    let at = |level| lzx_on(&Options { level, ..Options::default() });
    assert!(!at(Level::MID) && !at(Level::HIGH) && at(Level::Of(9)));
    // (and so CHM files, and streams of LZFSE and LZVN blocks)
    for format in [Format::Chm, Format::Bvx] {
        let at = |level| format_on(format, &Options { level, ..Options::default() });
        assert!(!at(Level::MID) && !at(Level::HIGH) && at(Level::Of(9)), "{format:?}");
    }
}

/// Levels 0 and 1 take no format at all: not even the files made beforehand ([`prepare`])
/// are made, as they would only be thrown away.
#[test]
fn nothing_taken_at_level_1() {
    let jpeg =
        std::fs::read(format!("{}/../../codecs/aqr-jpeg/tests/data/gradient.jpg", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let mut f = Formula::default();
    f.root = Node::concat([f.push_content("raw", Kind::Unknown, jpeg)]);
    for (level, n) in [(Level::STORED, 0), (Level::Of(1), 0), (Level::LOW, 1), (Level::MID, 1)] {
        let opt = Options { level, ..Options::default() };
        let mut jobs = Vec::new();
        pure_contents(&f.root, &f, &opt, &mut jobs);
        assert_eq!(jobs.len(), n, "{level:?}");
        assert_eq!(format_on(Format::Jpeg, &opt), n > 0, "{level:?}");
    }
}

/// LZMA and xz streams, 7z archives and GIF files are taken from level 4 only (up to level 3
/// kept as they are).
#[test]
fn heavy_from_level_4() {
    let fixture = |p: &str| std::fs::read(format!("{}/../../{p}", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let text: Vec<u8> = (0..30000).flat_map(|i| format!("line {} of the text\n", i * 7 % 1000).into_bytes()).collect();
    let files = [
        ("lzma", aqr_lzma::greedy::alone(&text)),
        ("7z", aqr_7z::write::solid(&[("a.txt", &text)])),
        ("gif", fixture("codecs/aqr-gif/tests/data/im-art.gif")),
    ];
    for (name, d) in &files {
        for (level, taken) in [(Level::LOW, false), (Level::Of(3), false), (Level::Of(4), true), (Level::MID, true)] {
            let f = recompress(d, &Options { level, shared_modules: true, ..Options::default() });
            assert_eq!(!f.applies().is_empty(), taken, "{name} {level:?}");
            assert!(f.eval(&Native::default()).unwrap() == *d, "{name} {level:?}");
        }
    }
}

#[test]
fn installshield_cabinet_taken_apart() {
    // a data volume of chunks of zlib's DEFLATE, two files of text
    let text: Vec<u8> =
        (0..120_000u32).flat_map(|i| format!("setup {} file{} ", i % 977, i % 31).into_bytes()).collect();
    let mut d = b"ISc(\xb0\x04\x00\x02\0\0\0\0".to_vec();
    d.extend(0x200u32.to_le_bytes());
    d.extend([0; 4]);
    d.extend(0x200u32.to_le_bytes());
    d.resize(0x200, 0);
    for c in text[..100_000].chunks(32768).chain(text[100_000..].chunks(32768)) {
        let z = aqr_flate::zlib::compress(c, aqr_flate::zlib::Params::default()).unwrap();
        d.extend((z.len() as u16).to_le_bytes());
        d.extend(z);
    }
    assert_eq!(detect(&d), Some(Format::Isc));
    let opt = Options { shared_modules: true, level: Level::Of(9), ..Options::default() };
    let f = recompress(&d, &opt);
    assert!(f.applies().iter().any(|a| a.module == aqr_decompress::FLATE));
    assert!(f.eval(&Native::default()).unwrap() == d);
    // (known at the start of data only: not found in other data)
    assert_ne!(detect([&b"some data before "[..], &d].concat().as_slice()), Some(Format::Isc));
}

/// An ALZ archive is taken apart: its DEFLATE bodies by aqr-flate, its bzip2 bodies by
/// aqr-bz2's module of ALZip's streams; found in other data too.
#[test]
fn alz_taken_apart() {
    let dir = format!("{}/../../codecs/aqr-alz/tests/data", env!("CARGO_MANIFEST_DIR"));
    for (name, module) in [("normal", aqr_decompress::FLATE), ("bzip2", aqr_decompress::BZ2_ALZ)] {
        let d = std::fs::read(format!("{dir}/{name}.alz")).unwrap();
        assert_eq!(detect(&d), Some(Format::Alz));
        // (ALZip's DEFLATE is zlib's, which the adaptive profile emulates, from level 7)
        let opt = Options { shared_modules: true, level: Level::Of(9), ..Options::default() };
        let f = recompress(&d, &opt);
        assert!(f.applies().iter().any(|a| a.module == module), "{name}");
        assert!(f.eval(&Native::default()).unwrap() == d, "{name}");
        let x = [&b"some data before "[..], &d, b" and after"].concat();
        assert_eq!(detect::starts(&x[17..]), Some(Format::Alz));
        assert_eq!(extent(Format::Alz, &x[17..]), Some(d.len()));
    }
}

/// An Aqueous archive is taken apart: its module block the outer archive's own block of the
/// module, its resource block one of the data as it is, and (at level 0) the data of its
/// uncompressed blocks contents of their own; the rest as it is, damaged or not.
#[test]
fn aqr_taken_apart() {
    let dir = format!("{}/../../codecs/aqr-flac/tests/data", env!("CARGO_MANIFEST_DIR"));
    let flac = std::fs::read(format!("{dir}/s16_f5.flac")).unwrap();
    // (a file alone too small for the module to pay for itself: its formula taken anyway)
    let inner = compress(&flac, &Options { shared_modules: true, smaller: false, ..Options::default() });
    assert_eq!(detect(&inner), Some(Format::Aqr));
    let f = aqueous::archive(&inner).unwrap();
    let mut module_bytes = false;
    f.root.visit(&mut |n| module_bytes |= matches!(n, Node::ModuleBytes(m, _) if *m == aqr_decompress::FLAC));
    assert!(module_bytes);
    assert!(f.eval(&Native::default()).unwrap() == inner);
    // an archive of it, which has the module's block once
    let opt = Options { shared_modules: true, ..Options::default() };
    assert!(aqueous::is(&recompress(&inner, &opt)));
    let c = compress(&[&inner[..], &flac].concat(), &Options::default());
    assert!(aqr_decompress::decompress(&c, 1).unwrap() == [&inner[..], &flac].concat());
    // at level 0, the file in an uncompressed block: a content
    let stored = compress(&flac, &Options { level: Level::STORED, ..Options::default() });
    let f = aqueous::archive(&stored).unwrap();
    assert!(f.streams.iter().any(|s| s.content && s.data[..] == flac[..]));
    assert!(f.eval(&Native::default()).unwrap() == stored);
    // cut short, it is still given back
    for n in [7, 100, inner.len() / 2] {
        if let Some(f) = aqueous::archive(&inner[..n]) {
            assert!(f.eval(&Native::default()).unwrap() == inner[..n], "{n}");
        }
    }
    assert!(aqueous::archive(&flac).is_none());
}

/// An LHA archive is taken apart, its streams by aqr-lha's module, its stored files their
/// contents; at the start of data only (its headers have no signature to look for). (The
/// fixtures are too small for the module to pay for itself: the formula is kept all the
/// same.)
#[test]
fn lha_taken_apart() {
    let dir = format!("{}/../../codecs/aqr-lha/tests/data", env!("CARGO_MANIFEST_DIR"));
    for name in ["unix_h2_lh7", "dos255e_lh0", "unix_lh6_long"] {
        let d = std::fs::read(format!("{dir}/{name}.lzh")).unwrap();
        assert_eq!(detect(&d), Some(Format::Lha), "{name}");
        let opt = Options { shared_modules: true, smaller: false, ..Options::default() };
        let f = recompress(&d, &opt);
        let module = f.applies().iter().any(|a| a.module == aqr_decompress::LHA);
        assert_eq!(module, name != "dos255e_lh0", "{name}");
        assert!(f.eval(&Native::default()).unwrap() == d, "{name}");
        let x = [&b"some data before "[..], &d].concat();
        assert_eq!(detect::starts(&x[17..]), None, "{name}");
    }
}

/// A RAR archive (RAR5 or RAR 3.x) is taken apart, its streams by aqr-rar's modules, its stored
/// files their contents; found in other data too (after a self-extracting program).
#[test]
fn rar_taken_apart() {
    let dir = format!("{}/../../codecs/aqr-rar/tests/data", env!("CARGO_MANIFEST_DIR"));
    for name in ["r5_solid", "r4_solid", "r4_ppmd", "r5_stored"] {
        let d = std::fs::read(format!("{dir}/{name}.rar")).unwrap();
        assert_eq!(detect(&d), Some(Format::Rar), "{name}");
        // (PPMd from level 9 only)
        for level in [Level::MID, Level::Of(9)] {
            let opt = Options { shared_modules: true, smaller: false, level, ..Options::default() };
            let f = recompress(&d, &opt);
            let module =
                f.applies().iter().any(|a| a.module == aqr_decompress::RAR5 || a.module == aqr_decompress::RAR3);
            let ppmd = name == "r4_ppmd";
            assert_eq!(module, name != "r5_stored" && (!ppmd || ppmd_on(&opt)), "{name} {level:?}");
            assert!(f.eval(&Native::default()).unwrap() == d, "{name}");
        }
        let x = [&b"some data before "[..], &d].concat();
        assert_eq!(detect::starts(&x[17..]), Some(Format::Rar), "{name}");
    }
}

/// An EGG archive is taken apart: its DEFLATE blocks by aqr-flate, its LZMA blocks by
/// aqr-lzma; found in other data too.
#[test]
fn egg_taken_apart() {
    use std::io::Write;
    // (DEFLATE of zlib's, of words enough for its module to pay)
    let mut x = 1u32;
    let text: Vec<u8> = (0..40_000)
        .flat_map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            format!("w{} ", (x % 1000) * (x % 1000) / 1000).into_bytes()
        })
        .collect();
    let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(6));
    z.write_all(&text).unwrap();
    let n = text.len() as u32;
    let deflated = aqr_egg::egg::write(false, &[("a.txt", aqr_egg::egg::DEFLATE, &z.finish().unwrap(), n)]);
    // (an LZMA block: 4 bytes, the properties and the stream of a `.lzma` file)
    let alone = aqr_lzma::greedy::alone(&text);
    let body = [&[0; 4], &alone[..5], &alone[13..]].concat();
    let lzma = aqr_egg::egg::write(false, &[("a.txt", aqr_egg::egg::LZMA, &body, n)]);
    for (name, d, module) in [("deflate", deflated, aqr_decompress::FLATE), ("lzma", lzma, aqr_decompress::LZMA)] {
        assert_eq!(detect(&d), Some(Format::Egg));
        let opt = Options { shared_modules: true, level: Level::Of(9), ..Options::default() };
        let f = recompress(&d, &opt);
        assert!(f.applies().iter().any(|a| a.module == module), "{name}");
        assert!(f.eval(&Native::default()).unwrap() == d, "{name}");
        let x = [&b"some data before "[..], &d, b" and after"].concat();
        assert_eq!(detect::starts(&x[17..]), Some(Format::Egg));
        assert_eq!(extent(Format::Egg, &x[17..]), Some(d.len()));
    }
}

/// The contents of an application of several outputs have applications of several outputs
/// of their own (a PDF file's streams, in an ALZ archive's DEFLATE bodies): those come before
/// it, whose inputs take their outputs.
#[test]
fn shared_within_shared() {
    use std::io::Write;
    let deflate = |d: &[u8]| {
        let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(6));
        z.write_all(d).unwrap();
        z.finish().unwrap()
    };
    let mut x = 1u32;
    let text: Vec<u8> = (0..40_000)
        .flat_map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            format!("w{} ", (x % 1000) * (x % 1000) / 1000).into_bytes()
        })
        .collect();
    // a PDF file of three streams of zlib's, which aqr-pdf takes by one application of
    // several outputs
    let mut pdf = b"%PDF-1.4\n".to_vec();
    for (i, data) in text.chunks(text.len() / 3 + 1).enumerate() {
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
        z.write_all(data).unwrap();
        let z = z.finish().unwrap();
        write!(pdf, "{} 0 obj\n<< /Length {} /Filter /FlateDecode >>\nstream\n", i + 1, z.len()).unwrap();
        pdf.extend_from_slice(&z);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");
    }
    pdf.extend_from_slice(b"trailer\n<< >>\n%%EOF\n");
    let text = &text[..20_000];
    let (a, b) = (deflate(&pdf), deflate(text));
    // (local headers of sizes of 4 bytes, of DEFLATE)
    let mut d = b"ALZ\x01\x0a\0\0\0".to_vec();
    for (name, body, size) in [(&b"a.pdf"[..], &a, pdf.len()), (b"b.txt", &b, text.len())] {
        d.extend_from_slice(b"BLZ\x01\x05\x00\x20\0\0\0\0\x40\x00\x02\x00\0\0\0\0");
        d.extend_from_slice(&(body.len() as u32).to_le_bytes());
        d.extend_from_slice(&(size as u32).to_le_bytes());
        d.extend_from_slice(name);
        d.extend_from_slice(body);
    }
    d.extend_from_slice(b"CLZ\x02");
    let opt = Options { shared_modules: true, level: Level::Of(9), ..Options::default() };
    let f = recompress(&d, &opt);
    assert!(f.shared.len() >= 2, "{}", f.shared.len());
    f.check_outputs().unwrap();
    assert!(f.eval(&Native::default()).unwrap() == d);
}

/// The contents of pure formats are made beforehand, each once: those of a ZIP archive's
/// entries are not made again where the first entry's own contents are looked for.
#[test]
fn contents_made_once() {
    let fixture = |p: &str| std::fs::read(format!("{}/../../{p}", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let files = [
        fixture("codecs/aqr-jpeg/tests/data/gradient.jpg"),
        fixture("codecs/aqr-jpeg/tests/data/prog_script_recode.jpg"),
    ];
    let (mut d, mut cd) = (Vec::new(), Vec::new());
    for (i, data) in files.iter().enumerate() {
        let mut h = vec![20, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        h.extend_from_slice(&aqr_common::crc32::hash(data).to_le_bytes());
        h.extend_from_slice(&(data.len() as u32).to_le_bytes());
        h.extend_from_slice(&(data.len() as u32).to_le_bytes());
        h.extend_from_slice(&[5, 0, 0, 0]);
        let name = [b'a' + i as u8, b'.', b'j', b'p', b'g'];
        cd.extend_from_slice(b"PK\x01\x02\x14\x03");
        cd.extend_from_slice(&h);
        cd.extend_from_slice(&[0; 10]);
        cd.extend_from_slice(&(d.len() as u32).to_le_bytes());
        cd.extend_from_slice(&name);
        d.extend_from_slice(b"PK\x03\x04");
        d.extend_from_slice(&h);
        d.extend_from_slice(&name);
        d.extend_from_slice(data);
    }
    let at = d.len() as u32;
    d.extend_from_slice(&cd);
    d.extend_from_slice(b"PK\x05\x06\0\0\0\0\x02\0\x02\0");
    d.extend_from_slice(&(cd.len() as u32).to_le_bytes());
    d.extend_from_slice(&at.to_le_bytes());
    d.extend_from_slice(&[0, 0]);
    PURE.with(|n| n.set(0));
    // (the modules the archive's anyway)
    let f = recompress(&d, &Options { threads: 1, shared_modules: true, ..Options::default() });
    assert!(f.applies().iter().any(|a| a.module == aqr_decompress::JPEG));
    assert_eq!(PURE.with(Cell::get), 2);
    assert!(f.eval(&Native::default()).unwrap() == d);
}

/// A text file, and a ZIP archive of it, archived together: the text is coded once, the
/// archive's entry taking what was coded of the file.
#[test]
fn text_coded_once() {
    let mut t = Vec::new();
    let mut x = 7u64;
    let words = ["alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta"];
    while t.len() < 1_000_003 {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        std::io::Write::write_fmt(&mut t, format_args!("{} {} ", words[(x >> 61) as usize], (x >> 40) % 1000)).unwrap();
    }
    t.truncate(1_000_003);
    let zip = stored_zip(&[("t.txt", &t)]);
    let tree = aqr_archive::Tree {
        entries: ["t.txt", "t.zip"]
            .iter()
            .enumerate()
            .map(|(c, name)| aqr_archive::Entry {
                parent: None,
                name: name.as_bytes().to_vec(),
                node: aqr_archive::EntryNode::File(c),
                extra: Default::default(),
            })
            .collect(),
        ..Default::default()
    };
    let inputs = [Input { name: b"t.txt", data: &t }, Input { name: b"t.zip", data: &zip }];
    let a = compress_tree(&tree, &inputs, &Options { threads: 4, ..Options::default() });
    assert!(a.report.files.iter().all(|f| f.modules.iter().any(|m| m.name == "wrt-recons")), "{:?}", a.report);
    assert!(a.report.unverified.is_empty());
    assert_eq!(TEXTS.lock().unwrap().iter().filter(|&&n| n == t.len()).count(), 1);
}

/// With [`Options::mirrors`], every block whose damage would lose more than a file has a
/// mirror block, an archive of a single file and of a tree alike, and gives its files back
/// (with a block damaged and restored too).
#[test]
fn mirrors() {
    let mut z = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
    std::io::Write::write_all(&mut z, &unruled(100_000)).unwrap();
    let gz = z.finish().unwrap();
    // (recompressed however small: a module block and a formula block)
    let opt = Options { threads: 2, mirrors: true, smaller: false, ..Options::default() };
    let checked = |c: &[u8]| {
        let m = aqr_archive::mirror::check(c);
        assert!(m.walked && m.broken == 0 && m.near(false) == 0, "{m:?}");
        assert!(m.targets.len() >= 2 && m.unmirrored() == 0);
        assert_eq!(m.differing(c).count(), 0);
        m
    };
    let single = compress(&gz, &opt);
    assert!(single.len() > compress(&gz, &Options { mirrors: false, ..opt.clone() }).len());
    checked(&single);
    assert_eq!(aqr_decompress::decompress(&single, 2).unwrap(), gz);

    let text = unruled(50_000);
    let tree = aqr_archive::Tree {
        entries: ["a.gz", "b.txt"]
            .iter()
            .enumerate()
            .map(|(c, name)| aqr_archive::Entry {
                parent: None,
                name: name.as_bytes().to_vec(),
                node: aqr_archive::EntryNode::File(c),
                extra: Default::default(),
            })
            .collect(),
        ..Default::default()
    };
    let inputs = [Input { name: b"a.gz", data: &gz }, Input { name: b"b.txt", data: &text }];
    let a = compress_tree(&tree, &inputs, &opt);
    assert_eq!(a.report.len, a.archive.len());
    assert!(a.report.mirrors > 0);
    let m = checked(&a.archive);
    let files = |c: &[u8]| match aqr_decompress::decompress_value(c, 2).unwrap() {
        aqr_archive::Value::Tree(_, files) => files,
        aqr_archive::Value::File(_) => panic!("a tree"),
    };
    assert_eq!(files(&a.archive), [gz.clone(), text.clone()]);
    let mut d = a.archive.clone();
    let meta = m.mirrors.last().unwrap();
    d[meta.target_end() - 1] ^= 0x10;
    assert!(aqr_decompress::decompress_value(&d, 2).is_err());
    let cd = aqr_archive::mirror::check(&d);
    assert_eq!(aqr_archive::mirror::repair(&d, &cd, false, &[]).unwrap().bytes, a.archive);
    // the metadata block's mirror block before the files (the archive is small: all of them)
    let meta = m.targets.last().unwrap().0;
    assert!(m.mirrors.iter().any(|f| f.target == meta && f.at < meta && f.far(false)));
    let d = &a.archive[..meta + 1];
    let r = aqr_archive::mirror::repair(d, &aqr_archive::mirror::check(d), false, &[]).unwrap();
    assert!(r.restored == 1 && r.footer && r.bytes.len() < a.archive.len());
    assert_eq!(files(&r.bytes), [gz.clone(), text.clone()]);
}

/// Of a text coded once for two, the formula kept is not evaluated beforehand again; nor is
/// the first where the text is large.
#[test]
fn once_evaluated() {
    let (small, large) = (vec![b'a'; 1000], vec![b'a'; ONCE_LARGE.get() + 1]);
    let key = |d: &[u8]| (d.len(), xxhash_rust::xxh3::xxh3_128(d));
    let once = Once::of([key(&small), key(&large)]);
    let (small2, large2) = (small.clone(), large.clone());
    for d in [&small, &small2, &large, &large2] {
        once.code(d, || Some(Formula::default()));
    }
    assert!(once.evaluated(&small) && !once.evaluated(&small2));
    assert!(!once.evaluated(&large) && !once.evaluated(&large2));
}

/// A stored ZIP archive of `files` (name, data).
fn stored_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
    let (mut d, mut cd) = (Vec::new(), Vec::new());
    for &(name, data) in files {
        let mut h = vec![20, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        h.extend_from_slice(&aqr_common::crc32::hash(data).to_le_bytes());
        h.extend_from_slice(&(data.len() as u32).to_le_bytes());
        h.extend_from_slice(&(data.len() as u32).to_le_bytes());
        h.extend_from_slice(&(name.len() as u16).to_le_bytes());
        h.extend_from_slice(&[0, 0]);
        cd.extend_from_slice(b"PK\x01\x02\x14\x03");
        cd.extend_from_slice(&h);
        cd.extend_from_slice(&[0; 10]);
        cd.extend_from_slice(&(d.len() as u32).to_le_bytes());
        cd.extend_from_slice(name.as_bytes());
        d.extend_from_slice(b"PK\x03\x04");
        d.extend_from_slice(&h);
        d.extend_from_slice(name.as_bytes());
        d.extend_from_slice(data);
    }
    let (at, n) = (d.len() as u32, files.len() as u16);
    d.extend_from_slice(&cd);
    d.extend_from_slice(b"PK\x05\x06\0\0\0\0");
    d.extend_from_slice(&n.to_le_bytes());
    d.extend_from_slice(&n.to_le_bytes());
    d.extend_from_slice(&(cd.len() as u32).to_le_bytes());
    d.extend_from_slice(&at.to_le_bytes());
    d.extend_from_slice(&[0, 0]);
    d
}

/// A gray PNG file of a gradient of `w` x `h`, with the pixels of `rect` (x, y, width,
/// height) changed.
fn variant(w: usize, h: usize, rect: Option<(usize, usize, usize, usize)>) -> Vec<u8> {
    use std::io::Write;
    let mut data = Vec::new();
    for y in 0..h {
        data.push(0);
        for x in 0..w {
            let inside = rect.is_some_and(|(rx, ry, rw, rh)| (rx..rx + rw).contains(&x) && (ry..ry + rh).contains(&y));
            data.push(if inside { ((x * 7) ^ (y * 3)) as u8 } else { (x + 2 * y) as u8 / 3 });
        }
    }
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
    z.write_all(&data).unwrap();
    let mut ihdr = (w as u32).to_be_bytes().to_vec();
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 0, 0, 0, 0]);
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    for (kind, d) in [(b"IHDR", ihdr), (b"IDAT", z.finish().unwrap()), (b"IEND", vec![])] {
        out.extend_from_slice(&(d.len() as u32).to_be_bytes());
        let start = out.len();
        out.extend_from_slice(kind);
        out.extend_from_slice(&d);
        out.extend_from_slice(&aqr_common::crc32::hash(&out[start..]).to_be_bytes());
    }
    out
}

/// A content that is all of its stream, whose formula is kept beside its data, keeps that
/// stream (of the class its parent gave it) as the data: not the data between the files a scan
/// of it found, of the scan's class (10gb/progs: a PNG file in a ZIP file, whose ZIP file's
/// formula was then taken, alike to raw files that compress better along with it).
#[test]
fn streams_of_a_file_alone_pay_for_their_module_together() {
    // zlib streams in noise, each of text that repeats past DEFLATE's window: each gains
    // less than flate-set's module block, the four more
    let mut rng = aqr_common::rng::Rng::new(11);
    let mut hex = |n| rng.bytes(n).iter().map(|b| format!("{b:02x}")).collect::<String>().into_bytes();
    let mut d = hex(2000);
    for _ in 0..4 {
        let (r, gaps) = (hex(10_000), [hex(20_000), hex(20_000)]);
        let data = [&r[..], &gaps[0], &r, &gaps[1], &r].concat();
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
        z.write_all(&data).unwrap();
        d.extend(z.finish().unwrap());
        d.extend(hex(2000));
    }
    let flate = |f: &Formula| f.applies().iter().filter(|a| a.module == aqr_decompress::FLATE).count();
    let f = recompress(&d, &Options::default());
    assert!(f.eval(&Native::default()).unwrap() == d);
    assert!(flate(&f) >= 4, "{f}");
    let _off = DEFER_MODULES.set_here(false);
    assert_eq!(flate(&recompress(&d, &Options::default())), 0);
}

#[test]
fn content_kept_beside_in_its_stream() {
    let (_slack, _min) = (DEFER_SLACK.set_here(Some(100.0)), DEFER_MIN.set_here(1 << 10));
    let png = variant(200, 150, Some((0, 0, 200, 150)));
    let text = babble(2000);
    let zip = stored_zip(&[("a.png", &png), ("b.txt", &text)]);
    let opt = Options { level: Level::Of(9), threads: 1, ..Options::default() };
    let cx = Cx::new(&zip, &opt);
    let m = made_deferring(&zip, &opt, 0, Nest::Exact, &cx).expect("a formula");
    let f = m.f;
    // (the PNG file's formula loses to its data, by its module: kept beside it, outermost;
    // within it, the scan's)
    let eithers = f.eithers();
    let outer = eithers.iter().filter(|(_, e)| e.len == png.len() as u64).min_by_key(|e| e.0);
    let (_, e) = outer.expect("the PNG file kept beside its formula");
    let mut classes = Vec::new();
    e.kept.visit(&mut |n| {
        if let &Node::Slice { stream, .. } = n {
            classes.push(&f.streams[stream].class[..]);
        }
    });
    assert_eq!(classes, ["zip/png"]);
}

/// The PNG files of a set are coded alone only where they have no parent: the others are
/// coded against theirs only.
#[test]
fn set_members_not_coded_alone() {
    let files =
        [variant(200, 150, None), variant(200, 150, Some((20, 30, 40, 50))), variant(200, 150, Some((90, 10, 30, 90)))];
    let d = stored_zip(&[("a.png", &files[0]), ("b.png", &files[1]), ("c.png", &files[2])]);
    PURE.with(|n| n.set(0));
    let f = recompress(&d, &Options { threads: 1, shared_modules: true, ..Options::default() });
    assert_eq!(f.shared.len(), 1);
    assert_eq!(f.shared[0].sizes.len(), 3);
    assert_eq!(PURE.with(Cell::get), 1);
    assert!(f.eval(&Native::default()).unwrap() == d);
}

/// The sets planned are made where the formula of the contents is not the one the
/// prevalidator evaluates (a copy of it, sharing its bytes).
#[test]
fn sets_of_copies() {
    let files =
        [variant(200, 150, None), variant(200, 150, Some((20, 30, 40, 50))), variant(200, 150, Some((90, 10, 30, 90)))];
    let d = stored_zip(&[("a.png", &files[0]), ("b.png", &files[1]), ("c.png", &files[2])]);
    let (c, f) = compress_formula(&d, &Options { threads: 2, shared_modules: true, ..Options::default() });
    assert_eq!(f.shared.len(), 1);
    assert!(aqr_decompress::decompress(&c, 1).unwrap() == d);
}

/// Among files made apart (an image's) of which sets are planned, those not in a set still
/// take the bytes of their streams as they are made ([`make_later`]); the members keep
/// theirs, which the sets are made of.
#[test]
fn taken_beside_sets() {
    let jpeg =
        std::fs::read(format!("{}/../../codecs/aqr-jpeg/tests/data/gradient.jpg", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let named = [
        (&b"a.png"[..], variant(200, 150, None)),
        (b"b.png", variant(200, 150, Some((20, 30, 40, 50)))),
        (b"c.png", variant(200, 150, Some((90, 10, 30, 90)))),
        (b"j.jpg", jpeg),
    ];
    let (mut d, mut files, mut pieces) = (Vec::new(), Vec::new(), Vec::new());
    for (i, (name, data)) in named.iter().enumerate() {
        let span = d.len()..d.len() + data.len();
        d.extend_from_slice(data);
        files.push(aqr_disk::File { name: name.to_vec(), extents: vec![span.clone()], rank: 0, compressed: None });
        pieces.push(aqr_disk::Piece { span, file: Some((i, 0)) });
    }
    let opt =
        aqr_disk::formula::Options { file_systems: bundle::FILE_SYSTEMS, kind: kind_by_name, ..Default::default() };
    let mut f = aqr_disk::formula::build(&d, &files, &pieces, &opt);
    let opt = Options { threads: 2, shared_modules: true, ..Options::default() };
    let cx = Cx::new(&d, &opt);
    let (_, plan) = nested(&mut f, &d, &opt, 0, &cx).unwrap();
    assert!(!plan.is_empty());
    let of = |class: &str| f.streams.iter().filter(|s| s.class == class).map(|s| s.data.len()).collect::<Vec<_>>();
    assert!(of("disk/png").len() == 3 && of("disk/png").iter().all(|&n| n > 0));
    assert_eq!(of("disk/jpeg"), [0]);
}

/// A FAT volume's JPEG file in pieces (aqr-fat's fixture) is recompressed by aqr-jpeg as one
/// content, through the archive; the archive the same whatever the threads.
#[test]
fn fat_files_in_pieces() {
    let z =
        std::fs::read(format!("{}/../../codecs/aqr-fat/tests/data/fat32.img.zst", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let d = zstd::stream::decode_all(&z[..]).unwrap();
    assert_eq!(detect(&d), Some(Format::Disk));
    let files = aqr_disk::files(&d, bundle::FILE_SYSTEMS);
    assert!(files.iter().find(|f| f.name == b"photo.jpg").unwrap().extents.len() > 1);
    let archived = |threads| compress_formula(&d, &Options { threads, shared_modules: true, ..Options::default() });
    let (c, f) = archived(2);
    assert!(f.applies().iter().any(|a| a.module == aqr_decompress::JPEG));
    assert!(!f.shared.is_empty());
    assert!(aqr_decompress::decompress(&c, 1).unwrap() == d);
    assert!(archived(1).0 == c);
}

/// An NTFS volume's files (aqr-ntfs's fixture): its JPEG file recompressed by aqr-jpeg, its
/// files in pieces split, the archive giving it back.
#[test]
fn ntfs_volumes_taken_apart() {
    let z =
        std::fs::read(format!("{}/../../codecs/aqr-ntfs/tests/data/ntfs.img.zst", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let d = zstd::stream::decode_all(&z[..]).unwrap();
    assert_eq!(detect(&d), Some(Format::Disk));
    let (c, f) = compress_formula(&d, &Options { threads: 2, shared_modules: true, ..Options::default() });
    assert!(f.applies().iter().any(|a| a.module == aqr_decompress::JPEG));
    assert!(!f.shared.is_empty());
    assert!(aqr_decompress::decompress(&c, 1).unwrap() == d);
}

/// UDIF images (aqr-disk's fixtures, of an HFS+ volume): of zlib's, bzip2's or LZFSE's chunks, its own
/// formula (the disk a content, taken apart in turn: its JPEG file by aqr-jpeg; zlib's chunks
/// by zlib's emulation), of raw ones a container of the disk; all given back.
#[test]
fn dmg_taken_apart() {
    let fixture = |name: &str| {
        let p = format!("{}/../../codecs/aqr-disk/tests/data/{name}.zst", env!("CARGO_MANIFEST_DIR"));
        zstd::stream::decode_all(&std::fs::read(p).unwrap()[..]).unwrap()
    };
    let opt = Options { threads: 2, shared_modules: true, ..Options::default() };
    let d = fixture("dmg-udzo.dmg");
    assert_eq!(detect(&d), Some(Format::Dmg));
    // (hdiutil's zlib emulated: its records next to nothing)
    let f = chunked::dmg(&d, &opt, &Cx::new(&d, &opt)).unwrap();
    let records: usize = f.streams.iter().filter(|s| s.class == "flate/record").map(|s| s.data.len()).sum();
    let chunks = aqr_disk::dmg::parse(&d).unwrap().chunks;
    let zlib: usize = chunks.iter().filter(|c| c.how == aqr_disk::dmg::How::Zlib).map(|c| c.file.len()).sum();
    assert!(records * 50 < zlib, "records {records} of {zlib} B");
    let (c, f) = compress_formula(&d, &opt);
    assert!(f.applies().iter().any(|a| a.module == aqr_flate::formula::MODULE));
    assert!(f.applies().iter().any(|a| a.module == aqr_decompress::JPEG));
    assert!(aqr_decompress::decompress(&c, 1).unwrap() == d);
    let d = fixture("dmg-udbz.dmg");
    assert_eq!(detect(&d), Some(Format::Dmg));
    let (c, f) = compress_formula(&d, &opt);
    assert!(f.applies().iter().any(|a| a.module == aqr_bz2::formula::MODULE));
    assert!(f.applies().iter().any(|a| a.module == aqr_decompress::JPEG));
    assert!(aqr_decompress::decompress(&c, 1).unwrap() == d);
    let d = fixture("dmg-ulfo.dmg");
    assert_eq!(detect(&d), Some(Format::Dmg));
    let (c, f) = compress_formula(&d, &opt);
    assert!(f.applies().iter().any(|a| a.module == aqr_bvx::formula::MODULE));
    assert!(aqr_decompress::decompress(&c, 1).unwrap() == d);
    let d = fixture("dmg-udro.dmg");
    assert_eq!(detect(&d), Some(Format::Disk));
    let (c, f) = compress_formula(&d, &opt);
    assert!(f.applies().iter().any(|a| a.module == aqr_decompress::JPEG));
    assert!(aqr_decompress::decompress(&c, 1).unwrap() == d);
}

/// Compressed containers (aqr-disk's fixtures, of qemu-img: a qcow2 image of compressed clusters,
/// raw DEFLATE of zlib's level 6 of a window of 4 KiB; a streamOptimized VMDK extent, zlib's
/// level 6): their own formula, as a DMG's (the compressed chunks by zlib's emulation, the
/// files taken apart: its JPEG file by aqr-jpeg), given back, and verified by their hashes
/// without running a module.
#[test]
fn compressed_containers_taken_apart() {
    for name in ["disk-c.qcow2", "disk-s.vmdk", "disk.E01"] {
        let p = format!("{}/../../codecs/aqr-disk/tests/data/{name}.zst", env!("CARGO_MANIFEST_DIR"));
        let d = zstd::stream::decode_all(&std::fs::read(p).unwrap()[..]).unwrap();
        assert_eq!(detect(&d), Some(Format::Disk), "{name}");
        let opt = Options { threads: 2, shared_modules: true, ..Options::default() };
        let f = chunked::container(&d, &opt, &Cx::new(&d, &opt)).unwrap();
        let records: usize = f.streams.iter().filter(|s| s.class == "flate/record").map(|s| s.data.len()).sum();
        let chunks = aqr_disk::chunked::container(&d).unwrap().chunks;
        let compressed: usize = (chunks.iter())
            .filter(|c| matches!(c.how, aqr_disk::chunked::How::Deflate | aqr_disk::chunked::How::Zlib))
            .map(|c| c.file.len())
            .sum();
        // (an EWF image's chunks are of 32 KiB, most of zeros: each a record of some bytes)
        let most = if name.ends_with(".E01") { 15 } else { 50 };
        assert!(records * most < compressed, "{name}: records {records} of {compressed} B");
        let (c, f) = compress_formula(&d, &opt);
        assert!(f.applies().iter().any(|a| a.module == aqr_flate::formula::MODULE), "{name}");
        assert!(f.applies().iter().any(|a| a.module == aqr_decompress::JPEG), "{name}");
        assert!(aqr_decompress::decompress(&c, 1).unwrap() == d, "{name}");
        let opt = Options { smaller: false, ..opt };
        let mut cx = Cx::new(&d, &opt);
        cx.pre = Some(Arc::new(verify::Prevalidator::spawn(2, &[&d])));
        let f = recompress_at(&d, &opt, 0, Kind::Unknown, &cx);
        let memo = verify::Prevalidator::finish_shared(cx.pre.take().unwrap());
        let c = cx.enc.encode(&f, Some(aqr_archive::checksum(&d))).unwrap();
        let roots = aqr_archive::open(&c, &aqr_archive::NoModules).unwrap().hashed(&memo, &aqr_decompress::identify, 2);
        let roots = roots.map_err(|e| e.to_string()).unwrap().expect("told");
        let whole = roots.iter().fold(aqr_archive::chash::Ch::EMPTY, |w, (h, _)| w.cat(*h));
        assert!(whole == aqr_archive::chash::Ch::of(&d), "{name}");
    }
}

/// An EWF image's segment files, each taken by itself (the second of raw chunks only): each
/// a disk image, its chunks rebuilt.
#[test]
fn ewf_segments_taken_apart() {
    for name in ["disk-s.E01", "disk-s.E02", "disk-s.E03"] {
        let p = format!("{}/../../codecs/aqr-disk/tests/data/{name}.zst", env!("CARGO_MANIFEST_DIR"));
        let d = zstd::stream::decode_all(&std::fs::read(p).unwrap()[..]).unwrap();
        assert_eq!(detect(&d), Some(Format::Disk), "{name}");
        let opt = Options { threads: 2, shared_modules: true, ..Options::default() };
        assert!(chunked::container(&d, &opt, &Cx::new(&d, &opt)).is_some(), "{name}");
        let (c, _) = compress_formula(&d, &opt);
        assert!(c.len() * 4 < d.len(), "{name}: {} of {} B", c.len(), d.len());
        assert!(aqr_decompress::decompress(&c, 1).unwrap() == d, "{name}");
    }
}

/// A DMG's archive is verified by its hashes without running a module: its disk's Split into
/// the chunks, of pieces that the chunks do not cut where they are cut, is told by what the
/// formula recorded of it as it was made (the disk's bytes at hand then), and the disk's
/// applications (its JPEG file's) by the prevalidation.
#[test]
fn dmg_verified_by_hashes() {
    let p = format!("{}/../../codecs/aqr-disk/tests/data/dmg-udzo.dmg.zst", env!("CARGO_MANIFEST_DIR"));
    let d = zstd::stream::decode_all(&std::fs::read(p).unwrap()[..]).unwrap();
    let opt = Options { threads: 2, smaller: false, ..Options::default() };
    let mut cx = Cx::new(&d, &opt);
    cx.pre = Some(Arc::new(verify::Prevalidator::spawn(2, &[&d])));
    let f = recompress_at(&d, &opt, 0, Kind::Unknown, &cx);
    let memo = verify::Prevalidator::finish_shared(cx.pre.take().unwrap());
    assert!(f.applies().iter().any(|a| a.module == aqr_decompress::JPEG));
    let c = cx.enc.encode(&f, Some(aqr_archive::checksum(&d))).unwrap();
    let a = aqr_archive::open(&c, &aqr_archive::NoModules).unwrap();
    let roots = a.hashed(&memo, &aqr_decompress::identify, 2).map_err(|e| e.to_string());
    let roots = roots.unwrap().expect("told");
    let whole = roots.iter().fold(aqr_archive::chash::Ch::EMPTY, |w, (h, _)| w.cat(*h));
    assert!(whole == aqr_archive::chash::Ch::of(&d));
}

/// A UDIF image of zlib's chunks whose block tables are changed (chunks of other runs of the
/// disk or the file, overlapping, of another kind): no formula, or one giving it back.
#[test]
fn dmg_damaged() {
    let p = format!("{}/../../codecs/aqr-disk/tests/data/dmg-udzo.dmg.zst", env!("CARGO_MANIFEST_DIR"));
    let d = zstd::stream::decode_all(&std::fs::read(p).unwrap()[..]).unwrap();
    let u = aqr_disk::dmg::parse(&d).unwrap();
    let opt = Options { threads: 2, ..Options::default() };
    let mut tried = 0;
    for p in u.xml.clone().step_by(5) {
        for v in [b'A', b'/', d[p] ^ 0x01] {
            let mut e = d.clone();
            e[p] = v;
            // (where the chunks read are others)
            if aqr_disk::dmg::parse(&e).is_none_or(|t| t.chunks == u.chunks) {
                continue;
            }
            tried += 1;
            if let Some(f) = chunked::dmg(&e, &opt, &Cx::new(&e, &opt)) {
                assert!(f.eval(&Native::default()).unwrap() == e, "byte {p} = {v:#x}");
            }
        }
    }
    assert!(tried > 10, "{tried}");
}

/// An APFS container's files (aqr-apfs's fixture, of a GPT): its JPEG file recompressed by
/// aqr-jpeg, the image given back.
#[test]
fn apfs_containers_taken_apart() {
    let z =
        std::fs::read(format!("{}/../../codecs/aqr-apfs/tests/data/apfs.img.zst", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let d = zstd::stream::decode_all(&z[..]).unwrap();
    assert_eq!(detect(&d), Some(Format::Disk));
    let (c, f) = compress_formula(&d, &Options { threads: 2, shared_modules: true, ..Options::default() });
    assert!(f.applies().iter().any(|a| a.module == aqr_decompress::JPEG));
    assert!(aqr_decompress::decompress(&c, 1).unwrap() == d);
}

/// An ext4 volume's files (aqr-ext's fixture): its JPEG file recompressed by aqr-jpeg, its
/// files in pieces split, the archive giving it back.
#[test]
fn ext_volumes_taken_apart() {
    let z =
        std::fs::read(format!("{}/../../codecs/aqr-ext/tests/data/ext4.img.zst", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let d = zstd::stream::decode_all(&z[..]).unwrap();
    assert_eq!(detect(&d), Some(Format::Disk));
    let (c, f) = compress_formula(&d, &Options { threads: 2, shared_modules: true, ..Options::default() });
    assert!(f.applies().iter().any(|a| a.module == aqr_decompress::JPEG));
    assert!(!f.shared.is_empty());
    assert!(aqr_decompress::decompress(&c, 1).unwrap() == d);
}

/// An NTFS volume's files Windows compressed by WOF's LZX and XPRESS (aqr-ntfs's fixture):
/// their data contents, their chunks rebuilt by aqr-lzx's and aqr-xpress's modules, the
/// volume given back.
#[test]
fn wof_taken_apart() {
    let z = std::fs::read(format!("{}/../../codecs/aqr-ntfs/tests/data/wof-small.vhd.zst", env!("CARGO_MANIFEST_DIR")))
        .unwrap();
    let d = zstd::stream::decode_all(&z[..]).unwrap();
    let (c, f) = compress_formula(&d, &Options { threads: 2, shared_modules: true, ..Options::default() });
    assert!(f.applies().iter().any(|a| a.module == aqr_decompress::LZX));
    assert!(f.applies().iter().any(|a| a.module == aqr_decompress::XPRESS));
    assert!(aqr_decompress::decompress(&c, 1).unwrap() == d);
}

/// A virtual disk in a container (aqr-disk's VHDX fixture, of a GPT of ext volumes): its files
/// taken where the container holds them, its JPEG files recompressed by aqr-jpeg, the
/// container given back.
#[test]
fn containers_taken_apart() {
    let z = std::fs::read(format!("{}/../../codecs/aqr-disk/tests/data/disk.vhdx.zst", env!("CARGO_MANIFEST_DIR")))
        .unwrap();
    let d = zstd::stream::decode_all(&z[..]).unwrap();
    assert_eq!(detect(&d), Some(Format::Disk));
    let (c, f) = compress_formula(&d, &Options { threads: 2, shared_modules: true, ..Options::default() });
    assert!(f.applies().iter().any(|a| a.module == aqr_decompress::JPEG));
    assert!(aqr_decompress::decompress(&c, 1).unwrap() == d);
}

/// A file of an image in pieces, split from its stream ([`aqr_formula::Of::Node`]), is one
/// content: a JPEG file in three pieces out of order, about a text file, recompressed by
/// aqr-jpeg, and the image rebuilt.
#[test]
#[allow(clippy::single_range_in_vec_init)]
fn files_in_pieces_taken() {
    let jpeg =
        std::fs::read(format!("{}/../../codecs/aqr-jpeg/tests/data/gradient.jpg", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let text = b"some text between the pieces, ".repeat(50);
    let (n, a, b) = (jpeg.len(), jpeg.len() / 3, jpeg.len() * 2 / 3);
    // its middle, the text, its end, its start
    let d = [&jpeg[a..b], &text, &jpeg[b..], &jpeg[..a]].concat();
    let (t, e, s) = (b - a, b - a + text.len(), b - a + text.len() + n - b);
    let files = [
        aqr_disk::File {
            name: b"j.jpg".to_vec(),
            extents: vec![s..s + a, 0..b - a, e..e + n - b],
            rank: 0,
            compressed: None,
        },
        aqr_disk::File { name: b"t.txt".to_vec(), extents: vec![t..e], rank: 0, compressed: None },
    ];
    let pieces = aqr_disk::pieces(&d, &files);
    let opt =
        aqr_disk::formula::Options { file_systems: bundle::FILE_SYSTEMS, kind: kind_by_name, ..Default::default() };
    let mut f = aqr_disk::formula::build(&d, &files, &pieces, &opt);
    assert_eq!(f.shared.len(), 1);
    let opt = Options { threads: 2, shared_modules: true, ..Options::default() };
    let cx = Cx::new(&d, &opt);
    let (children, plan) = nested(&mut f, &d, &opt, 0, &cx).unwrap();
    assert!(plan.is_empty());
    place_all(&mut f, children.into_iter().map(|c| c.1).collect(), Vec::new());
    let jpegs = |n: &Node| matches!(n, Node::Apply(a) if a.module == aqr_decompress::JPEG);
    assert!(f.shared.iter().any(|s| matches!(&s.of, aqr_formula::Of::Node(n) if jpegs(n))));
    assert!(f.eval(&Native::default()).unwrap() == d);
}

/// The DEFLATE stream of a gzip file is deconstructed where its data may archive smaller
/// than the stream by more than the record, and not where it would not: noise gains
/// nothing, and is not split for nothing.
#[test]
fn flate_where_promising() {
    let gz = |name: &str, d: &[u8], level| {
        let mut z = flate2::GzBuilder::new().filename(name).write(Vec::new(), flate2::Compression::new(level));
        std::io::Write::write_all(&mut z, d).unwrap();
        z.finish().unwrap()
    };
    let deconstructed = |d: &[u8], smaller| {
        DECONSTRUCTED.with(|n| n.set(0));
        let f = recompress(d, &Options { smaller, ..Options::default() });
        (DECONSTRUCTED.with(Cell::get), f.applies().len())
    };
    // (named as compressed data, which is not scanned for files)
    let noise = aqr_common::rng::noise(300_000);
    assert_eq!(deconstructed(&gz("noise.mp4", &noise, 6), true), (0, 0));
    // DEFLATE at level 1 leaves much to the archive
    let text: Vec<u8> = (0..30000).flat_map(|i| format!("line {} of the text\n", i * 7 % 1000).into_bytes()).collect();
    // (kept however large it archives with its modules)
    let (n, applies) = deconstructed(&gz("text.txt", &text, 1), false);
    assert_eq!(n, 1);
    assert!(applies > 0);
}

/// A recompression whose formula has an application over `application_bound` is not used,
/// and what fell back is told ([`fallbacks`]); under the default bound it is used.
#[test]
fn application_bound_falls_back() {
    let mut z = flate2::GzBuilder::new().filename("text.txt").write(Vec::new(), flate2::Compression::new(1));
    let text: Vec<u8> = (0..30000).flat_map(|i| format!("line {} of the text\n", i * 7 % 1000).into_bytes()).collect();
    std::io::Write::write_all(&mut z, &text).unwrap();
    let gz = z.finish().unwrap();
    let made = |application_bound| {
        let opt = Options { smaller: false, application_bound, ..Options::default() };
        let f = recompress(&gz, &opt);
        let back = f.eval(&Native::default());
        assert!(back.as_ref().is_ok_and(|b| *b == gz));
        f.applies().len()
    };
    let _seen = FALLBACKS_SEEN.lock().unwrap_or_else(|e| e.into_inner());
    let _ = fallbacks();
    assert!(made(None) > 0);
    assert!(fallbacks().is_empty());
    // (what fell back tells the exact minimal bound the application is declared with)
    let mut f = recompress(&gz, &Options { smaller: false, ..Options::default() });
    declare_memory(&mut f);
    let declared = f.applies().iter().map(|a| aqr_formula::minimal_bound(a.memory_limit, &a.task_limits)).max();
    let _ = fallbacks();
    // (the plan of an application is more than a byte)
    assert_eq!(made(Some(1)), 0);
    let told = fallbacks();
    assert!(!told.is_empty() && told.iter().all(|f| f.needed > 1 && !f.kept), "{told:?}");
    assert!(told.iter().any(|f| f.bytes == gz.len() as u64));
    assert_eq!(told.iter().map(|f| f.needed).max(), declared);
    assert_eq!(bound_text(256 << 20), "268435456 B = 256.00 MiB");
    assert_eq!(bound_text((256 << 20) + 1), "268435457 B = 256.01 MiB");
}

/// The members of a gzip stream after its first (as gzip, pigz's `--independent` and
/// aqr-pack write a stream in parts) are deconstructed too, their data one stream with the
/// first's; a member that is not one ends them, what follows it kept as it is.
#[test]
fn gzip_members_as_one() {
    let text: Vec<u8> = babble(60000);
    let member = |name: Option<&str>, d: &[u8]| {
        let b = flate2::GzBuilder::new();
        let b = if let Some(n) = name { b.filename(n) } else { b };
        let mut z = b.write(Vec::new(), flate2::Compression::new(6));
        std::io::Write::write_all(&mut z, d).unwrap();
        z.finish().unwrap()
    };
    let parts: Vec<&[u8]> = text.chunks(text.len() / 3 + 1).collect();
    let mut gz = member(Some("text.txt"), parts[0]);
    gz.extend(member(None, parts[1]));
    gz.extend(member(None, parts[2]));
    gz.extend(b"\x1f\x8b\x08 not a member");
    DECONSTRUCTED.with(|n| n.set(0));
    let f = recompress(&gz, &Options { smaller: false, ..Options::default() });
    assert_eq!(DECONSTRUCTED.with(Cell::get), 3);
    let back = f.eval(&Native::default());
    assert!(back.as_ref().is_ok_and(|b| *b == gz), "{:?}", back.map(|b| b.len()));
    // (one text of them all)
    let texts: Vec<usize> = f.streams.iter().filter(|s| s.kind == Kind::Text).map(|s| s.data.len()).collect();
    assert_eq!(texts.len(), 1, "{texts:?}");
    // (what follows, not a member, kept as it is)
    assert!(f.streams.iter().any(|s| s.data.windows(12).any(|w| w == b"not a member")));
}

/// Level 1 recompresses nothing; levels 2 and 3 keep DEFLATE streams as they are
/// ([`inflated`]); from 7 on, they are always deconstructed (no probe), with the adaptive
/// profile.
#[test]
fn levels() {
    let gz = |name: &str, d: &[u8], level| {
        let mut z = flate2::GzBuilder::new().filename(name).write(Vec::new(), flate2::Compression::new(level));
        std::io::Write::write_all(&mut z, d).unwrap();
        z.finish().unwrap()
    };
    let made = |d: &[u8], level| {
        DECONSTRUCTED.with(|n| n.set(0));
        // (sized without the module blocks, as among many files)
        let f = recompress(d, &Options { level, shared_modules: true, ..Options::default() });
        let profile =
            f.streams.iter().find(|s| s.class == "flate/record").map(|s| aqr_flate::record_profile(&s.data).unwrap());
        (DECONSTRUCTED.with(Cell::get), profile)
    };
    let noise = gz("noise.mp4", &aqr_common::rng::noise(300_000), 6);
    let text: Vec<u8> = (0..30000).flat_map(|i| format!("line {} of the text\n", i * 7 % 1000).into_bytes()).collect();
    // (DEFLATE at level 1 leaves much to the archive)
    let text = gz("text.txt", &text, 1);
    for level in
        [Level::Of(1), Level::LOW, Level::Of(3), Level::Of(4), Level::MID, Level::Of(7), Level::HIGH, Level::Of(9)]
    {
        let n = level.scale();
        // (noise turned down by the probe, or else after it is deconstructed)
        assert_eq!(made(&noise, level).0, (n >= 7) as usize, "{level:?}");
        let profile = match n {
            ..4 => None,
            4..7 => Some(aqr_flate::DEFAULT_PROFILE),
            _ => Some(aqr_flate::ADAPTIVE),
        };
        assert_eq!(made(&text, level), ((n >= 4) as usize, profile), "{level:?}");
    }
}

/// PDF files at the archive's level: the records of their DEFLATE streams of aqr-flate's
/// profile as [`Options::flate`] tells, and from level 7 on with no time budget (every
/// FlateDecode stream inflated, none in the token mode or raw).
#[test]
fn pdf_levels() {
    let mut rng = aqr_common::rng::Rng::new(1);
    let mut pdf = b"%PDF-1.7\n".to_vec();
    for i in 0..200 {
        let content: String = (0..200)
            .map(|_| format!("BT /F1 12 Tf {} {} Td (w{}) Tj ET\n", rng.below(600), rng.below(800), rng.below(50)))
            .collect();
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
        std::io::Write::write_all(&mut z, content.as_bytes()).unwrap();
        let z = z.finish().unwrap();
        pdf.extend_from_slice(
            format!("{} 0 obj\n<< /Length {} /Filter /FlateDecode >>\nstream\n", i + 1, z.len()).as_bytes(),
        );
        pdf.extend_from_slice(&z);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");
    }
    pdf.extend_from_slice(b"trailer\n<< /Size 201 >>\n%%EOF\n");
    assert_eq!(detect(&pdf), Some(Format::Pdf));
    for level in [Level::MID, Level::HIGH] {
        let f = recompress(&pdf, &Options { level, shared_modules: true, ..Options::default() });
        let stream = |class: &str| &f.streams.iter().find(|s| s.class == class).expect(class).data;
        // (none at mid, where the budget leaves them all to the token mode)
        let profile = aqr_flate::record_profile(stream("pdf/meta")).ok();
        let want = Options { level, ..Options::default() }.flate_profile();
        assert!(profile.is_none_or(|p| p == want) && (level == Level::MID || profile.is_some()), "{level:?}");
        let raw_or_tok = !stream("pdf/raw").is_empty() || !stream("pdf/tok-symbols").is_empty();
        assert_eq!(raw_or_tok, level == Level::MID, "{level:?}");
    }
}

/// A ustar archive of `files`.
fn tar_of(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut t = Vec::new();
    for (name, d) in files {
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..107].copy_from_slice(b"0000644");
        h[108..115].copy_from_slice(b"0000000");
        h[116..123].copy_from_slice(b"0000000");
        h[124..135].copy_from_slice(format!("{:011o}", d.len()).as_bytes());
        h[136..147].copy_from_slice(b"00000000000");
        h[156] = b'0';
        h[257..265].copy_from_slice(b"ustar\x0000");
        h[148..156].fill(b' ');
        let sum: u32 = h.iter().map(|&b| b as u32).sum();
        h[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
        t.extend_from_slice(&h);
        t.extend_from_slice(d);
        t.resize(t.len().next_multiple_of(512), 0);
    }
    t.resize(t.len() + 1024, 0);
    t
}

/// The text members of a tar archive are coded with one dictionary, so that the same
/// words code alike in files apart (each with a dictionary of its own, the zstd of the
/// coded files finds nothing across them).
#[test]
fn tar_members_share_words() {
    let words: Vec<String> = (0..300)
        .map(|i| {
            let n = i * 7919 + 13;
            format!(
                "{}{}{}",
                ["ab", "ka", "to", "mi", "re"][n % 5],
                ["lon", "pra", "esh", "ud"][n / 5 % 4],
                ["ing", "ed", "s", "ly", "er"][n / 20 % 5]
            )
        })
        .collect();
    let text = |seed: u64| {
        let mut x = seed;
        let mut t = Vec::new();
        while t.len() < 300_000 {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            t.extend_from_slice(words[(x >> 40) as usize % words.len()].as_bytes());
            t.push(b' ');
        }
        t
    };
    let texts: Vec<Vec<u8>> = (1..=4).map(text).collect();
    let names: Vec<String> = (0..4).map(|i| format!("t{i}.dat")).collect();
    let files: Vec<(&str, &[u8])> = names.iter().map(|n| n.as_str()).zip(texts.iter().map(|t| &t[..])).collect();
    let d = tar_of(&files);
    let opt = Options { threads: 2, ..Options::default() };
    let (c, f) = compress_formula(&d, &opt);
    let coded: Vec<_> = f.applies().into_iter().filter(|a| a.module == aqr_decompress::WRT).collect();
    assert!(coded.len() >= 2, "{} wrt applications", coded.len());
    assert!(coded.iter().all(|a| a.resources == coded[0].resources));
    assert!(aqr_decompress::decompress(&c, 1).unwrap() == d);
}

/// A file of text that aqr-wrt does not take is kept raw as text, archived as text is
/// (dense from level 8 on).
#[test]
fn text_kept_raw() {
    // (of no words, which aqr-wrt would take, nor of a table of numbers, which aqr-xsv would, nor
    // of lines that repeat their shape, which aqr-log would: hexadecimal digits of an LCG's)
    let mut x = 1u64;
    let mut text = Vec::new();
    for i in 0..40000 {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let third = if i % 2 == 0 { String::new() } else { format!(" {:08x}", x >> 20 & 0xffff_ffff) };
        text.extend(format!("{:012x} {:010x}{third}\n", x >> 16, x >> 24).into_bytes());
    }
    assert!(text.len() >= aqr_wrt::formula::MIN_LEN);
    assert_eq!(detect(&text), Some(Format::Text));
    let f = recompress(&text, &Options::default());
    assert!(f.applies().is_empty(), "{:?}", f.applies().iter().map(|a| a.module.name).collect::<Vec<_>>());
    assert_eq!(f.streams[0].kind, Kind::Text);

    // a file alone of a kind by its name, as one in an archive of many: text too short for
    // aqr-wrt by a name of text, compressed data stored
    let opt = Options { verify: false, ..Options::default() };
    let short = &text[..100_000];
    assert_eq!(detect(short), None);
    for (name, kind) in [(&b"short.txt"[..], Kind::Text), (b"short.log", Kind::Text), (b"short", Kind::Unknown)] {
        let (c, f) = compress_formula_as(short, kind_by_name(name), &opt);
        assert_eq!(f.streams[0].kind, kind, "{}", String::from_utf8_lossy(name));
        assert_eq!(
            aqr_archive::decode(&c, &aqr_decompress::natives(&aqr_decompress::Native { threads: 1 })).unwrap(),
            short
        );
    }
    let noise = aqr_common::rng::noise(100_000);
    let (_, f) = compress_formula_as(&noise, kind_by_name(b"movie.mp4"), &opt);
    assert_eq!(f.streams[0].kind, Kind::Compressed);
    // but a format by its name is looked for as in a file of no name, as a scan finds what
    // the file starts with (and what is in it where that is turned down)
    let mut z = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
    std::io::Write::write_all(&mut z, &text).unwrap();
    let gz = z.finish().unwrap();
    let opt = Options { smaller: false, ..opt };
    let (_, f) = compress_formula_as(&gz, kind_by_name(b"named.png"), &opt);
    assert!(!f.applies().is_empty());
}

/// Lines of words, each of a few of a vocabulary of 4000 that follow one another as words of
/// prose do (the next a function of the last: a phrase repeats as a whole, but no word is
/// seen often enough to be a template's own), as many on each line but every other one: no
/// table of spaces (aqr-xsv), and no lines of templates (aqr-log), which would lose the
/// phrases to its columns; aqr-wrt takes it.
fn babble(lines: usize) -> Vec<u8> {
    let mut x = 1u64;
    let word = |k: u64| [k / 676, k / 26 % 26, k % 26].map(|c| (b'a' + c as u8) as char).iter().collect::<String>();
    let mut t = String::new();
    for i in 0..lines {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let a = (x >> 33) % 4000;
        let (b, c) = ((a * 37 + 11) % 4000, (a * 37 * 37 + 11 * 38) % 4000);
        let more = if i % 2 == 0 { format!("{} ", word((c * 37 + 11) % 4000)) } else { String::new() };
        t += &format!("line {} of the {} {} {} {more}text\n", i * 7 % 1000, word(a), word(b), word(c));
    }
    t.into_bytes()
}

/// Bytes of 16 values in no order (of an LCG): DEFLATE takes 4 bits off each.
fn unruled(n: usize) -> Vec<u8> {
    let mut x = 1u64;
    (0..n)
        .map(|_| {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            0x80 | (x >> 60) as u8
        })
        .collect()
}

/// A formula whose archive does not give the file back (a module's bug) is not taken: the
/// file is archived as it is. So where the output differs only at its end, is shorter or
/// longer (verified as it is decoded, part by part).
#[test]
fn not_given_back_raw() {
    let d: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
    let opt = Options::default();
    let mut wrong_end = d.clone();
    *wrong_end.last_mut().unwrap() ^= 1;
    for other in [d.clone(), wrong_end, d[..d.len() - 1].to_vec(), [&d[..], b"x"].concat()] {
        let mut f = Formula::default();
        f.push("a", other[..1000].to_vec());
        f.push("b", other[1000..].to_vec());
        f.root = Node::concat([Node::slice(0, 0, 1000), Node::slice(1, 0, other.len() as u64 - 1000)]);
        let cx = Cx::new(&d, &opt);
        let c = finished(f, &d, &opt, &cx);
        assert!(aqr_decompress::decompress(&c, 1).unwrap() == d);
    }
}

/// [`finish`] into memory, of as many bytes as it tells.
fn finished(f: Formula, d: &[u8], opt: &Options, cx: &Cx) -> Vec<u8> {
    let mut c = Vec::new();
    let (n, _) = finish(choose::kept(f), d, opt, cx, Default::default(), None, false, None, &mut c).unwrap();
    assert_eq!(n, c.len());
    c
}

/// A GGUF file archived to a file directly ([`direct_room`]): its blocks written as they are
/// made, its formula into the room before them, the archive the same whatever the threads
/// and giving the file back; where the formula takes more than the room, the blocks moved
/// after it. Archived to memory, or below the size, it is archived as before.
#[test]
fn gguf_direct() {
    let d = aqr_tensors::synth::file(3, 8);
    let path = std::env::temp_dir().join(format!("aqr-compress-direct-{}.aqr", std::process::id()));
    let archived = |threads: usize, min: usize, room: Option<u64>| {
        DIRECT_MIN.set(Some(min));
        DIRECT_ROOM.set(room);
        TENSORS_SEGMENT.set(Some(64 << 10));
        let opt = Options { threads, ..Options::default() };
        let mut file =
            std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&path).unwrap();
        let before = DIRECTED.get();
        let (n, _) = compress_with(&d, Kind::Unknown, &opt, false, None, &mut file).unwrap();
        drop(file);
        let c = std::fs::read(&path).unwrap();
        assert_eq!(n, c.len());
        (c, DIRECTED.get() - before)
    };
    let (one, directed) = archived(1, 0, None);
    assert_eq!(directed, 1);
    assert!(aqr_decompress::decompress(&one, 1).unwrap() == d);
    let (four, _) = archived(4, 0, None);
    assert!(four == one, "the same whatever the threads");
    // (the room too small: the blocks moved after the formula)
    let (moved, _) = archived(2, 0, Some(2));
    assert!(moved.len() < one.len() && aqr_decompress::decompress(&moved, 1).unwrap() == d);
    let (held, directed) = archived(2, usize::MAX, None);
    assert_eq!(directed, 0);
    assert!(held != one && aqr_decompress::decompress(&held, 1).unwrap() == d);
    // (of the formula, not the file as it is: the room's spacer, or 2 bytes of it, more)
    assert!(one.len() < d.len() * 9 / 10 && moved.len() < held.len() + 64, "{} {}", one.len(), held.len());
    // (in memory: archived as before)
    DIRECT_MIN.set(Some(0));
    let opt = Options { threads: 2, ..Options::default() };
    let (mut c, before) = (Vec::new(), DIRECTED.get());
    compress_with(&d, Kind::Unknown, &opt, false, None, &mut c).unwrap();
    assert!(DIRECTED.get() == before && c == held);
    _ = std::fs::remove_file(&path);
}

/// [`gguf_direct`], of an encrypted archive: its payload written at offsets, each chunk
/// sealed once it is whole, the first after the room is filled; its blocks moved under a new
/// nonce where the room is too small. It decrypts to an archive giving the file back.
#[test]
fn gguf_direct_sealed() {
    let d = aqr_tensors::synth::file(3, 8);
    let path = std::env::temp_dir().join(format!("aqr-compress-direct-sealed-{}.aqr", std::process::id()));
    let archived = |threads: usize, min: usize, room: Option<u64>| {
        DIRECT_MIN.set(Some(min));
        DIRECT_ROOM.set(room);
        TENSORS_SEGMENT.set(Some(64 << 10));
        let opt = Options { threads, size_hint: true, ..Options::default() };
        let mut file =
            std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&path).unwrap();
        let before = DIRECTED.get();
        let mut s = aqr_archive::envelope::Sealer::new(&mut file, b"p", 12, true).unwrap();
        compress_with(&d, Kind::Unknown, &opt, false, None, &mut s).unwrap();
        s.finish().unwrap();
        drop(s);
        drop(file);
        let c = std::fs::read(&path).unwrap();
        let payload = aqr_archive::envelope::open(&c).unwrap().unlock(b"p").unwrap().decrypt(2).unwrap();
        assert!(aqr_decompress::decompress(&payload, 1).unwrap() == d);
        (payload, DIRECTED.get() - before)
    };
    let (one, directed) = archived(1, 0, None);
    assert_eq!(directed, 1);
    assert!(one.len() > 4 << 16, "of several chunks");
    let (four, _) = archived(4, 0, None);
    assert!(four == one, "the same whatever the threads");
    let (moved, _) = archived(2, 0, Some(2));
    assert!(moved.len() < one.len());
    let (held, directed) = archived(2, usize::MAX, None);
    assert_eq!(directed, 0);
    assert!(moved.len() < held.len() + 64);
    _ = std::fs::remove_file(&path);
}

/// An archive written to a file that does not give the file back is written again over it:
/// the archive of the file raw, shorter than it, the file cut to its length.
#[test]
fn written_again_to_file() {
    let d = aqr_common::rng::noise(100_000);
    let opt = Options::default();
    // (twice as long as the file: given back longer)
    let mut f = Formula::default();
    f.push("a", d.clone());
    f.push("b", d.iter().map(|b| b ^ 0x5a).collect::<Vec<u8>>());
    let n = d.len() as u64;
    f.root = Node::concat([Node::slice(0, 0, n), Node::slice(1, 0, n)]);
    let path = std::env::temp_dir().join(format!("aqr-compress-written-again-{}.aqr", std::process::id()));
    let mut file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&path).unwrap();
    let (n, _) =
        finish(choose::kept(f), &d, &opt, &Cx::new(&d, &opt), Default::default(), None, false, None, &mut file)
            .unwrap();
    drop(file);
    let c = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    assert_eq!(c.len(), n);
    assert!(c.len() < d.len() + 100);
    assert!(aqr_decompress::decompress(&c, 1).unwrap() == d);
}

/// A file read through ([`through`]) has its checksum and content hash, of several stretches
/// read at once as of one.
#[test]
fn read_through() {
    let d: Vec<u8> =
        (0..through_len() * 2 + 12345).map(|i| (i as u32).wrapping_mul(2654435761).to_le_bytes()[3]).collect();
    for threads in [1, 3] {
        let t = through(&d, threads);
        assert_eq!(t.sum, aqr_archive::checksum(&d));
        assert!(t.ch == aqr_archive::chash::Ch::of(&d));
    }
}

/// The formulas recompressors make are evaluated beforehand: the archive is verified by the
/// hashes recorded (not as a file it is not; without the records, by evaluating what they do
/// not tell); decoding it takes the applications recorded as the file's from the file, and
/// runs the module where they are not there.
#[test]
fn verified_beforehand() {
    // (of no records, which aqr-infer-recs would take, nor words, which aqr-wrt would)
    let text = unruled(240_000);
    let mut z = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
    std::io::Write::write_all(&mut z, &text).unwrap();
    let d = z.finish().unwrap();
    let opt = Options { smaller: false, ..Options::default() };
    let mut cx = Cx::new(&d, &opt);
    cx.pre = Some(Arc::new(verify::Prevalidator::spawn(2, &[&d])));
    let f = recompress_at(&d, &opt, 0, Kind::Unknown, &cx);
    let memo = verify::Prevalidator::finish_shared(cx.pre.take().unwrap());
    assert!(!f.applies().is_empty());
    let c = cx.enc.encode(&f, Some(aqr_archive::checksum(&d))).unwrap();
    let memo = std::cell::RefCell::new(memo);
    assert_eq!(hashed(Back::Plain(&c, false), &d, None, &memo, &opt), Some(true));
    let mut other = d.clone();
    other[1000] ^= 1;
    assert_eq!(hashed(Back::Plain(&c, false), &other, None, &memo, &opt), Some(false));
    // (as it was read through before: compared with that, not read again)
    assert_eq!(hashed(Back::Plain(&c, false), &other, Some(&through(&d, 2)), &memo, &opt), Some(true));
    assert_eq!(hashed(Back::Plain(&c, false), &d, Some(&through(&other, 2)), &memo, &opt), Some(false));
    // (without the records: evaluated where they do not tell)
    assert_eq!(hashed(Back::Plain(&c, false), &d, None, &Default::default(), &opt), Some(true));
    let memo = memo.into_inner();
    let decode = |c: &[u8], memo: verify::Memo| {
        let files = |_: usize, range: Range<usize>| d.get(range).map(<[u8]>::to_vec);
        let m = verify::Cached::new(Native { threads: 1 }, memo, &files);
        let out = aqr_archive::decode(c, &aqr_decompress::natives(&m));
        (out, m.hits.into_inner().unwrap())
    };
    let other = compress(&d[..d.len() - 1], &Options { verify: false, ..opt.clone() });
    // another archive's applications are not in it
    let (memo, hits) = {
        let files = |_: usize, range: Range<usize>| d.get(range).map(<[u8]>::to_vec);
        let m = verify::Cached::new(Native { threads: 1 }, memo, &files);
        aqr_archive::decode(&other, &aqr_decompress::natives(&m)).unwrap();
        (m.memo.into_inner().unwrap(), m.hits.into_inner().unwrap())
    };
    assert_eq!(hits, 0);
    assert_eq!(decode(&c, memo), (Ok(d.clone()), f.applies().len()));
    // without the memo, the same, with the modules run
    assert_eq!(decode(&c, Default::default()), (Ok(d.clone()), 0));
}

/// A Split of a value made of another Split's outputs (a disk of fragmented files, cut into a
/// DMG's chunks), its outputs taken by an application, is verified by its hashes: each value
/// evaluated there is pulled of its own states of nodes of several outputs, not of those that
/// another value already pulled.
#[test]
fn hashed_splits_of_splits() {
    let text = unruled(200_000);
    let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(6));
    std::io::Write::write_all(&mut z, &text).unwrap();
    let deflated = z.finish().unwrap();
    let tail = b"what follows the stream".to_vec();
    let d = [deflated.clone(), tail.clone()].concat();
    let opt = Options::default();
    let profile = opt.flate_profile();
    let fopt =
        aqr_flate::Options { threads: 1, zlib: true, store: true, ..aqr_flate::Options::for_profile(profile).unwrap() };
    let parsed = aqr_flate::Parsed::head(&deflated, &fopt).unwrap();
    assert_eq!(parsed.len(), deflated.len());
    let (data, record, _) = parsed.deconstruct_bytes(&fopt, &Default::default()).unwrap();
    assert!(data == text);
    // the text in two pieces, the second first, and the tail: cut by a Split into the text
    // and the tail, the text inflated again
    // (the pieces across two blocks, as a file's content of several streams is)
    let (half, m) = (text.len() / 2, text.len() / 2 + 777);
    let rotated = [&text[half..], &text[..half]].concat();
    let mut f = Formula::default();
    f.push("raw", rotated[..m].to_vec());
    f.push("dmg", [&rotated[m..], &tail[..]].concat());
    let (n, m) = (text.len() as u64, m as u64);
    let pieces = Node::concat([Node::slice(0, 0, m), Node::slice(1, 0, n - m)]);
    let inner = f.split_node(pieces, vec![n - half as u64, half as u64]);
    let joined = Node::concat([inner[1].clone(), inner[0].clone(), Node::slice(1, n - m, tail.len() as u64)]);
    let outer = f.split_node(joined, vec![n, tail.len() as u64]);
    let records = f.push("flate/record", record.clone());
    let index = f.push("flate/index", aqr_flate::module::index(&[(data.len(), record.len(), deflated.len())]));
    let a = aqr_flate::formula::apply(
        profile,
        outer[0].clone(),
        records,
        index,
        deflated.len() as u64,
        aqr_flate::formula::Derived::None,
    )
    .unwrap();
    f.root = Node::concat([Node::Apply(Box::new(a)), outer[1].clone()]);
    assert!(f.eval(&Native::default()).unwrap() == d);
    let c = Cx::new(&d, &opt).enc.encode(&f, Some(aqr_archive::checksum(&d))).unwrap();
    assert!(aqr_decompress::decompress(&c, 1).unwrap() == d);
    for threads in [1, 4] {
        assert_eq!(
            hashed(Back::Plain(&c, false), &d, None, &Default::default(), &Options { threads, ..opt.clone() }),
            Some(true)
        );
    }
}

/// An input that a recompressor would take more heap for than a recompression may is not
/// recompressed so; one taken in parts is, whatever its size.
#[test]
fn too_large_kept() {
    let limit = 512 << 20;
    assert!(fits_within(Format::Png, 40 << 20, limit));
    assert!(!fits_within(Format::Png, 50 << 20, limit));
    assert!(fits_within(Format::Text, 400 << 20, limit) && !fits_within(Format::Text, 500 << 20, limit));
    assert!(fits_within(Format::Disk, usize::MAX >> 8, limit));
    assert!(fits_within(Format::Jsonl, usize::MAX >> 8, limit));
}

/// The records of applications of large outputs hold for other batches and archives: one
/// whose formula is evaluated once though it is in several (`enwik9` and the entry of
/// `enwik9.zip` in another batch) is verified by its hashes there too, not decoded.
#[test]
fn large_records_for_others() {
    let text = unruled(3_000_000);
    let mut z = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
    std::io::Write::write_all(&mut z, &text).unwrap();
    let d = z.finish().unwrap();
    let opt = Options { smaller: false, ..Options::default() };
    let mut cx = Cx::new(&d, &opt);
    cx.pre = Some(Arc::new(verify::Prevalidator::spawn(2, &[&d])));
    let f = recompress_at(&d, &opt, 0, Kind::Unknown, &cx);
    drop(verify::Prevalidator::finish_shared(cx.pre.take().unwrap()));
    assert!(!f.applies().is_empty());
    let c = cx.enc.encode(&f, Some(aqr_archive::checksum(&d))).unwrap();
    // (the memo of the batch it was evaluated in gone)
    assert_eq!(hashed(Back::Plain(&c, false), &d, None, &Default::default(), &opt), Some(true));
}

/// A module run whose output is a whole file is run once: the same application again takes
/// the file's bytes, but where the output is not the file.
#[test]
fn whole_files_run_once() {
    let text = unruled(240_000);
    let mut z = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
    std::io::Write::write_all(&mut z, &text).unwrap();
    let d = z.finish().unwrap();
    let opt = Options { smaller: false, ..Options::default() };
    // (the DEFLATE stream, twice)
    let mut f = recompress(&d, &opt);
    let a = Node::Apply(Box::new(f.applies()[0].clone()));
    f.root = a.clone();
    let d = f.eval(&Native::default()).unwrap();
    f.root = Node::concat([a.clone(), a]);
    let c = Cx::new(&d, &opt).enc.encode(&f, None).unwrap();
    for (file, read) in [(d.clone(), 1), ([&d[..10], &[!d[10]], &d[11..]].concat(), 0)] {
        let reads = std::sync::atomic::AtomicUsize::new(0);
        let files = |_: usize, range: Range<usize>| {
            reads.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            file.get(range).map(<[u8]>::to_vec)
        };
        let mut m = verify::Cached::new(Native { threads: 1 }, Default::default(), &files);
        m.whole.insert(file.len(), vec![(0, xxhash_rust::xxh3::xxh3_128(&file))]);
        assert!(aqr_archive::decode(&c, &aqr_decompress::natives(&m)).unwrap() == [&d[..], &d[..]].concat());
        assert_eq!(reads.into_inner(), read);
    }
}

/// A slice of a stream that is content is scanned wherever it is, as an input of an
/// application that is not a content too; one of another stream is not.
#[test]
fn content_streams() {
    // (of no records, which aqr-infer-recs would take, nor words, which aqr-wrt would)
    let text = unruled(240_000);
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
    std::io::Write::write_all(&mut z, &text).unwrap();
    let d = [&b"header "[..], &z.finish().unwrap()].concat();
    let opt = Options { smaller: false, ..Options::default() };
    let cx = Cx::new(&d, &opt);
    for content in [false, true] {
        let mut f = Formula::default();
        let data = if content { f.push_content("data", Kind::Unknown, d.clone()) } else { f.push("data", d.clone()) };
        f.root = Node::apply(aqr_decompress::X86, vec![], vec![data], d.len() as u64);
        let children = nested(&mut f, &d, &opt, 0, &cx);
        assert_eq!(children.is_some(), content);
        if let Some((mut children, _)) = children {
            assert_eq!(children.len(), 1);
            let child = children.pop().unwrap().1.unwrap().cut(&d);
            assert_eq!(child.applies().iter().map(|a| a.module).collect::<Vec<_>>(), [FLATE]);
            assert!(child.eval(&Native::default()).unwrap() == d);
        }
    }
}

/// The data between the files a scan found is cut out of the stream of the data in place
/// where the stream is all and only the content, else copied out of it.
#[test]
fn gaps_cut_in_place() {
    // large enough to be cut to its length where it is (as allocators do)
    let n = 1 << 20;
    let data: Vec<u8> = (0..n).map(|i| (i % 251) as u8).collect();
    let gaps = [&data[2..5], &data[16..]].concat();
    let child = || {
        let mut f = Formula::default();
        f.push("raw", Vec::new());
        let found = f.push("found", b"FOUND".to_vec());
        f.root = Node::concat([Node::slice(0, 0, 3), found, Node::slice(0, 3, n as u64 - 16)]);
        Made { f, gaps: Some(vec![2..5, 16..n]), close: None }
    };
    let mut f = Formula::default();
    let whole = f.push("raw", data.clone());
    let record = f.push("flate/record", vec![0]);
    f.root = Node::apply(FLATE, vec![], vec![whole.clone(), record], n as u64);
    f.mark_contents();
    // the stream sliced again
    let mut copied = f.clone();
    copied.root = Node::concat([f.root.clone(), whole]);

    let at = f.streams[0].data.as_ptr();
    place_all(&mut f, vec![Some(child())], vec![]);
    let cut = f.streams.iter().find(|s| s.class == "raw").unwrap();
    assert!(cut.data.as_ptr() == at && cut.data == gaps);
    assert_eq!(f.streams.len(), 3);

    place_all(&mut copied, vec![Some(child())], vec![]);
    let raw: Vec<_> = copied.streams.iter().filter(|s| s.class == "raw").map(|s| &s.data[..]).collect();
    assert!(raw == [&data[..], &gaps[..]]);
}

/// An archive that does not give the file back is replaced by that of the file raw.
#[test]
fn finish_falls_back() {
    let d = b"some data, kept raw where its formula is wrong".repeat(100);
    let opt = Options::default();
    let cx = Cx::new(&d, &opt);
    let mut wrong = Formula::raw("raw", d.clone());
    wrong.streams[0].data[7] ^= 1;
    let raw = aqr_archive::encode(&Formula::raw("raw", d.clone()), Some(aqr_archive::checksum(&d)), opt.level).unwrap();
    // a leaf, whose checksum is not the output's, is not encoded at all
    assert_eq!(finished(wrong.clone(), &d, &Options { verify: false, ..opt.clone() }, &cx), raw);
    // two slices: the archive has the output's checksum
    wrong.root = Node::Concat(vec![Node::slice(0, 0, 100), Node::slice(0, 100, d.len() as u64 - 100)]);
    assert_eq!(finished(wrong.clone(), &d, &opt, &cx), raw);
    // a module that fails
    let fails = Formula {
        streams: vec![aqr_formula::Stream {
            class: "raw".into(),
            kind: Kind::Unknown,
            content: false,
            apart: false,
            data: vec![1, 2, 3].into(),
        }],
        root: Node::apply(FLATE, vec![], vec![Node::slice(0, 0, 3), Node::slice(0, 0, 3)], d.len() as u64),
        shared: vec![],
    };
    assert_eq!(finished(fails, &d, &opt, &cx), raw);
    // without verification, as it is
    let opt = Options { verify: false, ..opt };
    assert_eq!(
        aqr_decompress::decompress(&finished(wrong, &d, &opt, &cx), 1),
        Err(aqr_archive::Failure::Invalid("output checksum mismatch".into()))
    );
}

/// A PSD file of `w` x `h` 16-bit RGB (whose pixels aqr-psd does not code), its composite
/// `image` (raw), with color mode data `colors` and image resources `res`.
fn psd_file(w: u32, h: u32, colors: &[u8], res: &[u8], image: &[u8]) -> Vec<u8> {
    let mut d = b"8BPS\0\x01\0\0\0\0\0\0\0\x03".to_vec();
    d.extend_from_slice(&h.to_be_bytes());
    d.extend_from_slice(&w.to_be_bytes());
    d.extend_from_slice(&[0, 16, 0, 3]);
    for part in [colors, res] {
        d.extend_from_slice(&(part.len() as u32).to_be_bytes());
        d.extend_from_slice(part);
    }
    d.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
    d.extend_from_slice(image);
    d
}

/// In a file turned down whose recompressor tells what its parts are, pixels and structure
/// are not looked into (what a scan finds there is there by chance), the rest still is.
#[test]
fn scan_by_regions() {
    let (w, h) = (256, 64);
    // free-format MPEG audio headers, one frame apart, in noise its recompressor turns down,
    // and in a resource
    let mp3 = |d: &mut [u8], at: usize| {
        for at in [at, at + 500] {
            d[at..at + 4].copy_from_slice(&[0xff, 0xf3, 0x04, 0xc4]);
        }
    };
    let mut image = aqr_common::rng::noise(6 * w * h);
    mp3(&mut image, 1000);
    let mut res = b"8BIM\x04\x0f\0\0\0\0\x04\0".to_vec();
    res.extend(aqr_common::rng::noise(1024));
    mp3(&mut res, 100);
    // a gzip member where nothing is told: the color mode data
    let text: Vec<u8> = (0..2000).flat_map(|i| format!("line {i} of the text\n").into_bytes()).collect();
    let mut z = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
    std::io::Write::write_all(&mut z, &text).unwrap();
    let gz = z.finish().unwrap();
    let d = psd_file(w as u32, h as u32, &gz, &res, &[&[0, 0][..], &image].concat());
    for at in [26 + 4 + gz.len() + 4 + 100, d.len() - image.len() + 1000] {
        assert!(detect::starts(&d[at..]) == Some(Format::Mp3) && extent(Format::Mp3, &d[at..]).is_some());
    }
    TRIED.with(|t| t.borrow_mut().clear());
    let f = recompress(&d, &Options { smaller: false, ..Options::default() });
    let tried = TRIED.with(|t| t.borrow().clone());
    assert!(tried.contains(&Format::Psd), "{tried:?}");
    assert!(!tried.contains(&Format::Mp3), "{tried:?}");
    assert!(tried.contains(&Format::Gzip), "{tried:?}");
    assert!(f.applies().iter().any(|a| a.module == FLATE));
}

/// An archive turned down is not tried again from each of its later local headers, where an
/// archive also starts and runs to the same end record.
#[test]
fn scan_after_turned_down() {
    let (mut d, mut cd) = (Vec::new(), Vec::new());
    for i in 0..8u32 {
        let data = aqr_common::rng::noise(5000 + i as usize);
        // encrypted: kept as they are, so that the archive is turned down
        let mut h = vec![20, 0, 1, 0, 0, 0, 0, 0, 0, 0];
        h.extend_from_slice(&aqr_common::crc32::hash(&data).to_le_bytes());
        h.extend_from_slice(&(data.len() as u32).to_le_bytes());
        h.extend_from_slice(&(data.len() as u32).to_le_bytes());
        h.extend_from_slice(&[1, 0, 0, 0]);
        cd.extend_from_slice(b"PK\x01\x02\x14\x03");
        cd.extend_from_slice(&h);
        cd.extend_from_slice(&[0; 10]);
        cd.extend_from_slice(&(d.len() as u32).to_le_bytes());
        cd.push(b'a' + i as u8);
        d.extend_from_slice(b"PK\x03\x04");
        d.extend_from_slice(&h);
        d.push(b'a' + i as u8);
        d.extend_from_slice(&data);
    }
    let at = d.len() as u32;
    d.extend_from_slice(&cd);
    d.extend_from_slice(b"PK\x05\x06\0\0\0\0\x08\0\x08\0");
    d.extend_from_slice(&(cd.len() as u32).to_le_bytes());
    d.extend_from_slice(&at.to_le_bytes());
    d.extend_from_slice(&[0, 0]);
    TRIED.with(|t| t.borrow_mut().clear());
    let f = recompress(&d, &Options::default());
    assert!(f.applies().is_empty());
    let zips = TRIED.with(|t| t.borrow().iter().filter(|&&f| f == Format::Zip).count());
    assert_eq!(zips, 1);
}

/// Data given to recompressors that turn it down counts against a scan, which stops
/// looking for files past twice the data.
#[test]
fn scan_budget() {
    let jpeg =
        std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../codecs/aqr-jpeg/tests/data/gradient.jpg")).unwrap();
    // files of different formats running to the end of the data, each turned down (one of
    // a format already turned down there would not be tried)
    let fakes: [&[u8]; 3] = [
        &[&b"\x89PNG\r\n\x1a\n"[..], &0x7fff_0000u32.to_be_bytes(), b"IHDR"].concat(),
        b"fLaC\x80\0\0\x22",
        b"%PDF-1.7\n",
    ];
    let data = |n: usize| [fakes[..n].concat(), vec![0; 10000], jpeg.clone()].concat();
    let found = |d: &[u8]| {
        let opt = Options { shared_modules: true, ..Options::default() };
        scan_with(d, None, &opt, 0, 0, &Cx::new(d, &opt)).map_or(0, |m| m.f.applies().len())
    };
    assert_eq!(found(&data(1)), 1);
    assert_eq!(found(&data(3)), 0);
}
