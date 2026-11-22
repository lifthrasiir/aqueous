//! A bundle telling more than it holds fails, rather than reserving what it tells: a frame of a
//! declared length of 4 GiB (fuzz/sfx found the stub's unpacking abort, the allocation failing).
#![cfg(all(feature = "encode", feature = "xz"))]

#[global_allocator]
static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(1 << 30);

/// A blob of an image of `n` bytes of one section.
fn blob(n: usize) -> Vec<u8> {
    aqr_blob::Blob {
        apply_rva: 0x1010,
        info_rva: 0,
        pdata_rva: 0,
        pdata_count: 0,
        import_dir_rva: 0,
        first: 0x1000,
        image_base: 0x1_8000_0000,
        name: String::new(),
        binds: Vec::new(),
        relocs: Vec::new(),
        sections: vec![aqr_blob::Section { rva: 0x1000, len: n as u32, flags: aqr_blob::SCN_READ }],
        image: (0..n).map(|i| i as u8).collect(),
    }
    .to_bytes()
}

#[test]
fn frames_of_lengths_beyond_them() {
    let blobs = vec![(format!("{}/1", "0".repeat(64)), blob(5000))];
    for (codec, b) in [("zstd", aqr_sfx::bundle::pack(&blobs)), ("xz", aqr_sfx::bundle::pack_xz(&blobs))] {
        let mut b = b.unwrap();
        assert_eq!(aqr_sfx::bundle::unpack(&b).unwrap().len(), 1);
        // (the first frame's raw length, after the magic and the bundle's length)
        b[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        aqr_testkit::case("frames_of_lengths_beyond_them");
        assert!(aqr_sfx::bundle::unpack(&b).is_err(), "{codec}");
        assert!(aqr_testkit::most() < 1 << 30, "{codec}: a request of {} bytes", aqr_testkit::most());
    }
}
