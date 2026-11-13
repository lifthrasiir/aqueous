//! A `.wem` of Wwise's Vorbis: its RIFF chunks, the `vorb` fields and the packets.

use std::ops::Range;

fn u16le(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(at..at + 2)?.try_into().ok()?))
}

fn u32le(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

/// A medium's stream.
#[derive(Debug, Clone)]
pub struct Wem {
    pub channels: usize,
    pub rate: u32,
    pub samples: u32,
    /// The `vorb` fields' size (0x2A where they are in `fmt `).
    pub vorb_size: usize,
    /// The audio packets lack their packet type and window flags.
    pub mod_packets: bool,
    /// log2 of the short and long block sizes.
    pub blocksize_pows: [u8; 2],
    pub uid: u32,
    /// The stripped setup header's bytes (after its packet header).
    pub setup: Range<usize>,
    /// The audio packets' bytes (each after its packet header).
    pub packets: Vec<Range<usize>>,
    /// The `data` chunk's contents.
    pub data: Range<usize>,
    /// Where the first audio packet's header is.
    pub audio: usize,
    /// The packets end inside a packet: the medium is cut short (a bank's prefetched part of a
    /// streamed medium).
    pub cut: bool,
}

/// Whether `d` starts as a medium of Wwise's Vorbis: a RIFF WAVE whose first chunk is `fmt `
/// of format `FFFF`.
pub fn starts(d: &[u8]) -> bool {
    d.get(..4) == Some(b"RIFF") && d.get(8..16) == Some(b"WAVEfmt ") && d.get(20..22) == Some(&[0xff, 0xff])
}

/// The bytes of medium `d` (its RIFF size), if it starts as one.
pub fn extent(d: &[u8]) -> Option<usize> {
    starts(d).then(|| (8 + u32le(d, 4).unwrap() as usize).min(d.len()))
}

/// Why a medium is not read.
pub type Error = &'static str;

/// Reads `d`, a `.wem`.
pub fn parse(d: &[u8]) -> Result<Wem, Error> {
    if d.get(..4) != Some(b"RIFF") || d.get(8..12) != Some(b"WAVE") {
        return Err("not RIFF WAVE");
    }
    let (mut fmt, mut vorb, mut data) = (None, None, None);
    let mut at = 12;
    while let (Some(tag), Some(size)) = (d.get(at..at + 4), u32le(d, at + 4)) {
        let r = at + 8..(at + 8).saturating_add(size as usize).min(d.len());
        match tag {
            b"fmt " => fmt = Some(r.clone()),
            b"vorb" => vorb = Some(r.clone()),
            b"data" => data = Some(r.clone()),
            _ => {}
        }
        at = r.end;
    }
    let fmt = fmt.ok_or("no fmt")?;
    let f = &d[fmt.clone()];
    if u16le(f, 0) != Some(0xFFFF) {
        return Err("not Vorbis");
    }
    let channels = u16le(f, 2).ok_or("fmt")? as usize;
    let rate = u32le(f, 4).ok_or("fmt")?;
    let vorb = match vorb {
        Some(v) => v,
        None if f.len() == 0x42 => fmt.start + 0x18..fmt.end,
        None => return Err("no vorb"),
    };
    let v = &d[vorb.clone()];
    let data = data.ok_or("no data")?;
    if v.len() != 0x2A {
        return Err(match v.len() {
            0x28 => "vorb of 0x28 bytes",
            0x2C => "vorb of 0x2C bytes",
            0x32 => "vorb of 0x32 bytes",
            0x34 => "vorb of 0x34 bytes",
            _ => "vorb of another size",
        });
    }
    let samples = u32le(v, 0).ok_or("vorb")?;
    let mod_signal = u32le(v, 4).ok_or("vorb")?;
    let mod_packets = !matches!(mod_signal, 0x4A | 0x4B | 0x69 | 0x70);
    let setup_at = data.start + u32le(v, 0x10).ok_or("vorb")? as usize;
    let audio_at = data.start + u32le(v, 0x14).ok_or("vorb")? as usize;
    let uid = u32le(v, 0x24).ok_or("vorb")?;
    let blocksize_pows = [v[0x28], v[0x29]];
    let packet = |at: usize| -> Option<Range<usize>> {
        let n = u16le(d, at)? as usize;
        (at + 2 + n <= data.end).then_some(at + 2..at + 2 + n)
    };
    let setup = packet(setup_at).ok_or("setup packet")?;
    if audio_at > data.end {
        return Err("audio packets");
    }
    let mut packets = Vec::new();
    let mut at = audio_at;
    let mut cut = false;
    while at < data.end {
        let Some(p) = packet(at) else {
            cut = true;
            break;
        };
        at = p.end;
        packets.push(p);
    }
    Ok(Wem {
        channels,
        rate,
        samples,
        vorb_size: v.len(),
        mod_packets,
        blocksize_pows,
        uid,
        setup,
        packets,
        data,
        audio: audio_at,
        cut,
    })
}
