//! Edge cases of the directory trees: images made here, of what the fixtures lack (multi-extent
//! files, sparse and chained UDF extents) and of what a hostile image does (directories of the
//! image's size pointing at each other).

use aqr_disk::Plain;
use aqr_testkit::within;
use std::time::Duration;

const SECTOR: usize = 2048;

/// An ISO 9660 directory record.
fn record(lba: u32, size: u32, flags: u8, id: &[u8]) -> Vec<u8> {
    let mut r = vec![0u8; 33];
    r[2..6].copy_from_slice(&lba.to_le_bytes());
    r[10..14].copy_from_slice(&size.to_le_bytes());
    r[25] = flags;
    r[32] = id.len() as u8;
    r.extend_from_slice(id);
    if r.len() % 2 == 1 {
        r.push(0);
    }
    r[0] = r.len() as u8;
    r
}

/// An image of `sectors` sectors with a primary volume descriptor whose root directory is at
/// sector 20 of `root_size` bytes.
fn iso(sectors: usize, root_size: u32) -> Vec<u8> {
    let mut d = vec![0u8; sectors * SECTOR];
    let v = &mut d[16 * SECTOR..17 * SECTOR];
    v[0] = 1;
    v[1..6].copy_from_slice(b"CD001");
    v[128..130].copy_from_slice(&2048u16.to_le_bytes());
    v[156..190].copy_from_slice(&record(20, root_size, 2, &[0]));
    let t = &mut d[17 * SECTOR..18 * SECTOR];
    t[0] = 255;
    t[1..6].copy_from_slice(b"CD001");
    d
}

fn names(files: &[aqr_iso::File]) -> Vec<String> {
    files.iter().map(|f| String::from_utf8_lossy(&f.name).into_owned()).collect()
}

/// The records of a file of several extents are one file, in the order of the records; a record of
/// another name ends it.
#[test]
fn multi_extent_file() {
    let mut d = iso(64, SECTOR as u32);
    let mut dir = Vec::new();
    dir.extend(record(20, 2048, 2, &[0]));
    dir.extend(record(20, 2048, 2, &[1]));
    dir.extend(record(30, 2048, 0x80, b"BIG.BIN;1"));
    dir.extend(record(40, 100, 0x80, b"BIG.BIN;1"));
    dir.extend(record(35, 2048, 0, b"BIG.BIN;1"));
    dir.extend(record(50, 10, 0x80, b"CUT.BIN;1"));
    dir.extend(record(52, 20, 0, b"NEXT.BIN;1"));
    d[20 * SECTOR..20 * SECTOR + dir.len()].copy_from_slice(&dir);
    let files = aqr_iso::iso9660::files(&Plain(&d));
    let at = |s: usize| s * SECTOR;
    let big = files.iter().find(|f| f.name == b"BIG.BIN").unwrap();
    assert_eq!(big.extents, [at(30)..at(31), at(40)..at(40) + 100, at(35)..at(36)]);
    let cut = files.iter().find(|f| f.name == b"CUT.BIN").unwrap();
    assert_eq!(cut.extents, [at(50)..at(50) + 10]);
    assert_eq!(files.len(), 3, "{:?}", names(&files));
}

/// Directories of the image's size, each pointing at the others: what is read and found is
/// bounded by the image, not by the square of its records.
#[test]
fn directories_of_the_images_size() {
    let sectors = 40;
    let mut d = iso(sectors, (sectors * SECTOR) as u32);
    let len = d.len() as u32;
    let mut n = 0u32;
    for s in 20..sectors {
        let mut dir = Vec::new();
        while dir.len() + 40 < SECTOR {
            n += 1;
            // (alternating: a directory of a new place, and a file)
            let name = format!("{:04}", n % 10000);
            match n % 2 {
                0 => dir.extend(record(20 + n % 20, len, 2, name.as_bytes())),
                _ => dir.extend(record(n % 30, 5, 0, name.as_bytes())),
            }
        }
        d[s * SECTOR..s * SECTOR + dir.len()].copy_from_slice(&dir);
    }
    let files = within(Duration::from_secs(20), || aqr_iso::iso9660::files(&Plain(&d))).unwrap();
    assert!(files.len() <= d.len() / 34, "{} files in {} bytes", files.len(), d.len());
}

fn tag(id: u16, v: &mut [u8]) {
    v[..2].copy_from_slice(&id.to_le_bytes());
    v[4] = v[..16].iter().enumerate().filter(|&(i, _)| i != 4).fold(0u8, |s, (_, &b)| s.wrapping_add(b));
}

const PART: usize = 300;

/// A file identifier descriptor of a file whose ICB is logical block `lbn`.
fn fid(lbn: u32, name: &str, chars: u8) -> Vec<u8> {
    let id: Vec<u8> = std::iter::once(8).chain(name.bytes()).collect();
    let mut f = vec![0u8; 38];
    f[18] = chars;
    f[19] = id.len() as u8;
    f[24..28].copy_from_slice(&lbn.to_le_bytes());
    f.extend(id);
    f.resize(f.len().div_ceil(4) * 4, 0);
    tag(257, &mut f);
    f
}

/// A file entry of `info` bytes whose allocation descriptors (of kind `flags`, 0 short, 1 long,
/// 3 inline) are `ads`.
fn entry(flags: u16, info: u64, ads: &[u8]) -> Vec<u8> {
    let mut e = vec![0u8; 176];
    e[34..36].copy_from_slice(&flags.to_le_bytes());
    e[56..64].copy_from_slice(&info.to_le_bytes());
    e[172..176].copy_from_slice(&(ads.len() as u32).to_le_bytes());
    e.extend_from_slice(ads);
    tag(261, &mut e);
    e
}

/// A short allocation descriptor of `len` bytes of `kind` at logical block `lbn`.
fn short(kind: u32, len: u32, lbn: u32) -> Vec<u8> {
    let mut a = (kind << 30 | len).to_le_bytes().to_vec();
    a.extend(lbn.to_le_bytes());
    a
}

/// A UDF image: the anchor, the main sequence (a partition at sector `PART`, a logical volume),
/// the file set at block 0, the root directory (inline) at block 1 with `files` (name, block of
/// its entry), and the blocks `blocks` (block, bytes).
fn udf(files: &[(&str, u32)], blocks: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut d = vec![0u8; (PART + 100) * SECTOR];
    let put = |d: &mut Vec<u8>, sector: usize, v: &[u8]| {
        d[sector * SECTOR..sector * SECTOR + v.len()].copy_from_slice(v);
    };
    let mut anchor = vec![0u8; 24];
    anchor[16..20].copy_from_slice(&(3 * SECTOR as u32).to_le_bytes());
    anchor[20..24].copy_from_slice(&257u32.to_le_bytes());
    tag(2, &mut anchor);
    put(&mut d, 256, &anchor);
    let mut pd = vec![0u8; 196];
    pd[22..24].copy_from_slice(&7u16.to_le_bytes());
    pd[188..192].copy_from_slice(&(PART as u32).to_le_bytes());
    tag(5, &mut pd);
    put(&mut d, 257, &pd);
    let mut lvd = vec![0u8; 446];
    lvd[212..216].copy_from_slice(&(SECTOR as u32).to_le_bytes());
    lvd[268..272].copy_from_slice(&1u32.to_le_bytes());
    lvd[440..446].copy_from_slice(&[1, 6, 0, 0, 7, 0]);
    tag(6, &mut lvd);
    put(&mut d, 258, &lvd);
    let mut end = vec![0u8; 16];
    tag(8, &mut end);
    put(&mut d, 259, &end);
    let mut fsd = vec![0u8; 416];
    fsd[404..408].copy_from_slice(&1u32.to_le_bytes());
    tag(256, &mut fsd);
    put(&mut d, PART, &fsd);
    let dir: Vec<u8> = files.iter().flat_map(|&(n, lbn)| fid(lbn, n, 0)).collect();
    put(&mut d, PART + 1, &entry(3, dir.len() as u64, &dir));
    for (b, v) in blocks {
        put(&mut d, PART + *b as usize, v);
    }
    d
}

/// Sparse extents (allocated or not, not recorded) are no part of the file's extents; long
/// descriptors read as short ones do.
#[test]
fn udf_sparse_and_long() {
    let ads = [short(0, 2048, 10), short(1, 2048, 0), short(2, 2048, 0), short(0, 100, 12)].concat();
    let mut long = Vec::new();
    for (len, lbn) in [(2048u32, 20u32), (500, 22)] {
        let mut a = len.to_le_bytes().to_vec();
        a.extend(lbn.to_le_bytes());
        a.resize(16, 0);
        long.extend(a);
    }
    let d =
        udf(&[("sparse", 2), ("long", 3)], &[(2, entry(0, 3 * 2048 + 100, &ads)), (3, entry(1, 2048 + 500, &long))]);
    let files = aqr_iso::udf::files(&Plain(&d));
    let at = |b: usize| (PART + b) * SECTOR;
    let get =
        |n: &str| files.iter().find(|f| f.name == n.as_bytes()).unwrap_or_else(|| panic!("{n}: {:?}", names(&files)));
    assert_eq!(get("sparse").extents, [at(10)..at(11), at(12)..at(12) + 100]);
    assert_eq!(get("long").extents, [at(20)..at(21), at(22)..at(22) + 500]);
}

/// A list of allocation descriptors continued in an allocation extent descriptor; a chain looping
/// on itself ends.
#[test]
fn udf_chained() {
    let first = [short(0, 2048, 10), short(3, 16, 20)].concat();
    let rest = [short(0, 2048, 15), short(0, 2048, 16)].concat();
    let mut aed = vec![0u8; 24];
    aed[20..24].copy_from_slice(&(rest.len() as u32).to_le_bytes());
    aed.extend(&rest);
    tag(258, &mut aed);
    // a chain of one descriptor only continuing in itself
    let looped = short(3, 8, 21);
    let mut again = vec![0u8; 24];
    again[20..24].copy_from_slice(&(looped.len() as u32).to_le_bytes());
    again.extend(&looped);
    tag(258, &mut again);
    let d = udf(
        &[("chained", 2), ("looped", 3)],
        &[(2, entry(0, 3 * 2048, &first)), (20, aed), (3, entry(0, 3 * 2048, &looped)), (21, again)],
    );
    let files = within(Duration::from_secs(20), || aqr_iso::udf::files(&Plain(&d))).unwrap();
    let at = |b: usize| (PART + b) * SECTOR;
    let c = files.iter().find(|f| f.name == b"chained").unwrap_or_else(|| panic!("{:?}", names(&files)));
    assert_eq!(c.extents, [at(10)..at(11), at(15)..at(17)]);
    assert!(files.iter().all(|f| f.name != b"looped"));
}
