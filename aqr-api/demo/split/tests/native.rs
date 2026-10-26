use aqr_api::emu::apply;
use aqr_demo_split::split::ENTRY;

fn split(data: &[u8]) -> Vec<Vec<u8>> {
    (0..2).map(|j| data.iter().skip(j).step_by(2).copied().collect()).collect()
}

#[test]
fn small() {
    for data in [&b"hello"[..], b"", b"x", b"ab"] {
        let sizes = [data.len().div_ceil(2) as u64, data.len() as u64 / 2];
        assert_eq!(apply(&ENTRY, &[data], &[], b"", &sizes).unwrap(), split(data));
    }
}

#[test]
fn multi_page() {
    let data: Vec<u8> = (0..200_003u32).map(|i| (i * 7) as u8).collect();
    let sizes = [100_002, 100_001];
    assert_eq!(apply(&ENTRY, &[&data], &[], b"", &sizes).unwrap(), split(&data));
}
