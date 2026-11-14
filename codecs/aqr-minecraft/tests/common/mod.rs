//! Chunks' NBT made up for the tests (no world's): sections of a palette and packed indices of a
//! terrain of layers, caves and ores, and their block light and sky light, as Minecraft writes
//! them (FORMAT.md, §4).

use aqr_common::rng::Rng;

fn name(d: &mut Vec<u8>, ty: u8, n: &str) {
    d.push(ty);
    d.extend((n.len() as u16).to_be_bytes());
    d.extend_from_slice(n.as_bytes());
}

fn string(d: &mut Vec<u8>, s: &str) {
    d.extend((s.len() as u16).to_be_bytes());
    d.extend_from_slice(s.as_bytes());
}

const BLOCKS: [&str; 8] = [
    "minecraft:air",
    "minecraft:stone",
    "minecraft:dirt",
    "minecraft:grass_block",
    "minecraft:deepslate",
    "minecraft:iron_ore",
    "minecraft:water",
    "minecraft:torch",
];

/// The NBT of a chunk of `sections` sections from Y -4, of `seed`: its terrain's surface about
/// Y 4 (section 8 or so), sections above it air (one palette entry, no data), and some sections
/// without light.
pub fn chunk(seed: u64, sections: usize) -> Vec<u8> {
    let mut r = Rng::new(seed);
    let mut d = Vec::new();
    name(&mut d, 10, "");
    name(&mut d, 3, "DataVersion");
    d.extend(4189i32.to_be_bytes());
    name(&mut d, 8, "Status");
    string(&mut d, "minecraft:full");
    name(&mut d, 9, "sections");
    d.push(10);
    d.extend((sections as i32).to_be_bytes());
    let surface = 140 + r.below(8) as i32;
    for s in 0..sections {
        let y0 = s as i32 * 16;
        // the blocks, and the palette of those used in order of first use
        let mut blocks = vec![0usize; 4096];
        for (i, b) in blocks.iter_mut().enumerate() {
            let (x, z, y) = (i % 16, i / 16 % 16, y0 + (i / 256) as i32);
            let top = surface + ((x * 3 + z * 5) % 7) as i32 / 3;
            *b = if y > top {
                if y < 132 { 6 } else { 0 }
            } else if y == top {
                3
            } else if y > top - 4 {
                2
            } else if (x * 7 + z * 13 + y as usize * 3).is_multiple_of(97) || r.below(500) == 0 {
                5
            } else if (x as i32 - 8).pow(2) + (y - 60).pow(2) < 20 {
                0
            } else if y < 64 {
                4
            } else {
                1
            };
            if *b == 0 && r.below(300) == 0 {
                *b = 7;
            }
        }
        let mut palette: Vec<usize> = Vec::new();
        let idx: Vec<usize> = blocks
            .iter()
            .map(|b| match palette.iter().position(|p| p == b) {
                Some(k) => k,
                None => {
                    palette.push(*b);
                    palette.len() - 1
                }
            })
            .collect();
        name(&mut d, 1, "Y");
        d.push((s as i8 - 4) as u8);
        name(&mut d, 10, "block_states");
        name(&mut d, 9, "palette");
        d.push(10);
        d.extend((palette.len() as i32).to_be_bytes());
        for &p in &palette {
            name(&mut d, 8, "Name");
            string(&mut d, BLOCKS[p]);
            if p == 6 {
                name(&mut d, 10, "Properties");
                name(&mut d, 8, "level");
                string(&mut d, "0");
                d.push(0);
            }
            d.push(0);
        }
        if palette.len() > 1 {
            let bits = (usize::BITS - (palette.len() - 1).leading_zeros()).max(4) as usize;
            let per = 64 / bits;
            let longs = 4096usize.div_ceil(per);
            name(&mut d, 12, "data");
            d.extend((longs as i32).to_be_bytes());
            for l in 0..longs {
                let mut w = 0u64;
                for j in 0..per {
                    if let Some(&v) = idx.get(l * per + j) {
                        w |= (v as u64) << (j * bits);
                    }
                }
                d.extend(w.to_be_bytes());
            }
        }
        d.push(0);
        name(&mut d, 10, "biomes");
        name(&mut d, 9, "palette");
        d.push(8);
        d.extend(1i32.to_be_bytes());
        string(&mut d, "minecraft:plains");
        d.push(0);
        // the light: the sky's from above down to the first block not air nor water, then less;
        // the block light about the torches
        if s % 5 != 4 {
            for (n, kind) in ["BlockLight", "SkyLight"].iter().enumerate() {
                name(&mut d, 7, kind);
                d.extend(2048i32.to_be_bytes());
                for k in 0..2048 {
                    let nib = |i: usize| -> u8 {
                        let y = y0 + (i / 256) as i32;
                        let open = blocks[i] == 0 || blocks[i] == 6 || blocks[i] == 7;
                        match n {
                            0 => (blocks[i] == 7) as u8 * 14,
                            _ if open && y > surface - 2 => 15,
                            _ if open => (15 - (surface - y).min(15)) as u8,
                            _ => 0,
                        }
                    };
                    d.push(nib(2 * k) | nib(2 * k + 1) << 4);
                }
            }
        }
        d.push(0);
    }
    name(&mut d, 4, "LastUpdate");
    d.extend((r.u64() >> 20).to_be_bytes());
    d.push(0);
    d
}
