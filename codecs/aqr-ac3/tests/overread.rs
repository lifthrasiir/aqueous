//! A coded stream cut short decodes zeros: it must end in an error as it is read past its end, not decode what the
//! table declares; a table of many small chunks costs what they hold; the checksum is the table's and fast.

use std::time::{Duration, Instant};

use aqr_ac3::{crc16_bitwise, crc16_of, module, native, stream};
use aqr_walk::rc::Encoder;

fn table(entries: &[(u64, u64)]) -> Vec<u8> {
    let mut v = Vec::new();
    aqr_walk::chunks::put(&mut v, entries.iter().copied());
    v
}

fn both(coded: &[u8], len: u64) -> [Result<Vec<u8>, String>; 2] {
    [
        native::decode(&[], &[coded], len, 2).map_err(|e| e.to_string()),
        module::decode_serial(&[], &[coded], len, 1).map_err(|e| e.to_string()),
    ]
}

#[test]
fn a_few_bytes_do_not_decode_to_many_records() {
    // 3 bytes declared to hold 256 MiB: zeros decode as runs of other bytes, 4096 at a time
    let mut coded = table(&[(256 << 20, 3)]);
    coded.extend([0; 3]);
    let t = Instant::now();
    for r in both(&coded, 256 << 20) {
        assert!(r.is_err(), "decoded");
    }
    assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
}

fn chunk_of(data: &[u8]) -> Vec<u8> {
    let mut enc = Encoder::new();
    stream::encode_chunk(&mut enc, data, &stream::records(data));
    enc.finish()
}

#[test]
fn a_chunk_cut_short_is_an_error() {
    let data: Vec<u8> = (0..3000u32).map(|i| (i * 7 + i / 13) as u8).collect();
    let part = chunk_of(&data);
    let make = |part: &[u8]| {
        let mut coded = table(&[(data.len() as u64, part.len() as u64)]);
        coded.extend_from_slice(part);
        coded
    };
    for r in both(&make(&part), data.len() as u64) {
        assert!(r.unwrap() == data);
    }
    for cut in 6..part.len() - 8 {
        for r in both(&make(&part[..part.len() - cut]), data.len() as u64) {
            assert!(r.is_err(), "cut {cut} of {}", part.len());
        }
    }
}

#[test]
fn many_small_chunks_cost_what_they_hold() {
    // 20000 chunks of a few bytes: a model of 8 MiB made or reset for each took 1 ms
    const N: usize = 20000;
    let data = *b"abcdefgh";
    let part = chunk_of(&data);
    let mut coded = table(&vec![(8, part.len() as u64); N]);
    for _ in 0..N {
        coded.extend_from_slice(&part);
    }
    let t = Instant::now();
    for r in both(&coded, 8 * N as u64) {
        assert!(r.unwrap() == data.repeat(N));
    }
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
}

#[test]
fn the_checksum_is_the_bitwise_one() {
    let mut s = 12345u32;
    let mut d = Vec::new();
    for n in (0..300).chain([1000, 3840, 4097]) {
        d.clear();
        d.extend((0..n).map(|_| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            (s >> 3) as u8
        }));
        assert_eq!(crc16_of(&d), crc16_bitwise(&d), "{n} bytes");
    }
    assert_eq!(crc16_of(&[]), 0);
    assert_eq!(crc16_of(&[0xFF; 3840]), crc16_bitwise(&[0xFF; 3840]));
}

#[test]
fn the_checksum_is_fast() {
    // dense false syncwords are the worst case of the records: a checksum of a large frame at each
    let data: Vec<u8> = std::iter::repeat_n([0x0B, 0x77, 0, 0, 0x65], 1 << 20).flatten().collect();
    // what the records must check at least: the first checksum (of 5/8 of the frame) at each
    // syncword with room for its frame
    let checked: usize = (0..data.len())
        .filter_map(|p| aqr_ac3::frame_len(&data[p..]).filter(|n| p + n <= data.len()))
        .map(|n| ((n / 4) + (n / 16)) * 2 - 2)
        .sum();
    // timed against the checksum's speed here, not a speed of its own (8.8 MB/s on an M4, 4.9 on
    // a runner of CI running other tests at once): the fastest of a few runs of each
    let fastest = |f: &dyn Fn()| {
        (0..3)
            .map(|_| {
                let t = Instant::now();
                f();
                t.elapsed()
            })
            .min()
            .unwrap()
    };
    let block: Vec<u8> = (0..32u32 << 20).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
    let crc = fastest(&|| {
        std::hint::black_box(crc16_of(std::hint::black_box(&block)));
    });
    let expected = crc.mul_f64(checked as f64 / block.len() as f64);
    let dt = fastest(&|| {
        std::hint::black_box(stream::records(std::hint::black_box(&data)));
    });
    let mbps = data.len() as f64 / 1e6 / dt.as_secs_f64();
    eprintln!(
        "{} bytes of false syncwords: {dt:?} ({mbps:.1} MB/s), {checked} bytes checksummed: {expected:?}",
        data.len()
    );
    // (the bitwise checksum is 3.7-6 times as slow: the_checksum_runs_at_the_tables_speed)
    assert!(dt < expected * 3, "{dt:?} against {expected:?} of checksums");
}

#[test]
fn the_checksum_runs_at_the_tables_speed() {
    let d: Vec<u8> = (0..32 << 20).map(|i: u32| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
    // the fastest of a few runs each: the first run of a binary just built ran at a quarter of
    // the speed of the runs after it, and a runner of CI shares its cores
    let fastest = |f: fn(&[u8]) -> u16| {
        let runs = (0..3).map(|_| {
            let t = Instant::now();
            (f(&d), t.elapsed())
        });
        runs.reduce(|a, b| (a.0, a.1.min(b.1))).unwrap()
    };
    let (c, table) = fastest(crc16_of);
    let (b, bitwise) = fastest(crc16_bitwise);
    assert_eq!(c, b);
    eprintln!("32 MiB: tables {table:?}, bitwise {bitwise:?}");
    // (6 times as fast on an M4, 3.7 on CI's Windows runner: the tables' gain is the processor's)
    assert!(table * 2 < bitwise, "tables {table:?}, bitwise {bitwise:?}");
}
