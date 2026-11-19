//! A coded stream cut short decodes zeros: it must end in an error as it is read past its end, not decode
//! whatever the table declares; and a table of very many small chunks costs what they hold.

use std::time::{Duration, Instant};

use aqr_common::varint;
use aqr_div3::{Codec, Encoder, module, native};

const PARAMS: [u8; 4] = [64, 0, 48, 0];

fn layout_of(n: usize, len: u64) -> Vec<u8> {
    let mut v = Vec::new();
    for _ in 0..n {
        varint::put(&mut v, 0);
        varint::put(&mut v, len << 1);
    }
    v
}

fn table(entries: impl ExactSizeIterator<Item = (u64, u64)>) -> Vec<u8> {
    let mut v = Vec::new();
    aqr_walk::chunks::put(&mut v, entries);
    v
}

fn both(coded: &[u8], layout: &[u8], len: u64) -> [Result<Vec<u8>, String>; 2] {
    let i: [&[u8]; 4] = [coded, &[], layout, &[]];
    [
        native::decode(&PARAMS, &i, len, 2).map_err(|e| e.to_string()),
        module::decode_serial(&PARAMS, &i, len).map_err(|e| e.to_string()),
    ]
}

#[test]
fn a_few_bytes_do_not_decode_to_many_frames() {
    // 3 bytes declared to hold 5000 frames of 4 MiB: it was all of them, decoded from zeros
    let frame = module::FRAME_MAX as u64;
    let layout = layout_of(5000, frame);
    let mut coded = table([(5000, 3)].into_iter());
    coded.extend([0; 3]);
    let t = Instant::now();
    for r in both(&coded, &layout, 5000 * frame) {
        assert!(r.is_err(), "decoded");
    }
    assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
}

#[test]
fn a_chunk_cut_short_is_an_error() {
    let frames: Vec<Vec<u8>> = (0..40u8).map(|i| (0..30).map(|k| i.wrapping_mul(37) ^ k).collect()).collect();
    let mut codec = Codec::new(64, 48);
    let mut enc = Encoder::new();
    for f in &frames {
        codec.encode(&mut enc, f);
    }
    let part = enc.finish();
    let layout = layout_of(frames.len(), 30);
    let len = frames.len() as u64 * 30;
    let make = |part: &[u8]| {
        let mut coded = table([(frames.len() as u64, part.len() as u64)].into_iter());
        coded.extend_from_slice(part);
        coded
    };
    for r in both(&make(&part), &layout, len) {
        assert_eq!(r.unwrap(), frames.concat());
    }
    // cut anywhere short of the end by more than the coder's slack
    for cut in 6..part.len() - 8 {
        for r in both(&make(&part[..part.len() - cut]), &layout, len) {
            assert!(r.is_err(), "cut {cut} of {}", part.len());
        }
    }
}

#[test]
fn many_small_chunks_cost_what_they_hold() {
    // 20000 chunks of a frame each: a model of 16 MiB made or reset for each took 2 ms (minutes in all)
    const N: usize = 20000;
    let frames: Vec<Vec<u8>> = (0..N).map(|i| (0..8u32).map(|k| (i as u32 * 31 + k * 7) as u8).collect()).collect();
    let mut codec = Codec::new(64, 48);
    let parts: Vec<Vec<u8>> = frames
        .iter()
        .map(|f| {
            codec.reset();
            let mut enc = Encoder::new();
            codec.encode(&mut enc, f);
            enc.finish()
        })
        .collect();
    let mut coded = table(parts.iter().map(|p| (1, p.len() as u64)));
    for p in &parts {
        coded.extend_from_slice(p);
    }
    let layout = layout_of(N, 8);
    let t = Instant::now();
    for (name, r) in ["native", "serial"].iter().zip(both(&coded, &layout, N as u64 * 8)) {
        assert!(r.unwrap() == frames.concat(), "{name}");
    }
    assert!(t.elapsed() < Duration::from_secs(8), "{:?}", t.elapsed());
}
