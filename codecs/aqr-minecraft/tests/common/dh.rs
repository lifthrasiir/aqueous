//! Distant Horizons' databases made up for the tests (no world's): sections of level 0 of a
//! terrain of layers, their parents made of them as `aqr_minecraft::dh` makes them, every blob a
//! frame of libzstd at level 3, written by the sqlite3 tool into a `FullData` of Distant
//! Horizons' columns.

use std::io::Write;

use aqr_minecraft::dh::{self, Point, Section, States, Work};

const STATES: [&[u8]; 5] = [
    b"minecraft:plains_DH-BSW_minecraft:air_DH-BSW_AIR",
    b"minecraft:plains_DH-BSW_minecraft:grass_block_STATE_{snowy:false}",
    b"minecraft:plains_DH-BSW_minecraft:dirt_STATE_",
    b"minecraft:plains_DH-BSW_minecraft:stone_STATE_",
    b"minecraft:plains_DH-BSW_minecraft:iron_ore_STATE_",
];

fn rng(s: &mut u64) -> u64 {
    *s ^= *s << 13;
    *s ^= *s >> 7;
    *s ^= *s << 17;
    *s
}

/// A section of level 0 at `x`, `z`: each column air above its surface, grass, dirt, stone with
/// a vein of ore here and there, from the top down; its light bright above the surface.
fn ground(x: i64, z: i64, states: &mut States) -> Section {
    let ids: Vec<u32> = STATES.iter().map(|s| states.id(s)).collect();
    let mut seed = (x as u64).wrapping_mul(0x9e37_79b9) ^ (z as u64) << 20 ^ 0x1234_5678;
    let mut cols = Vec::with_capacity(dh::COLUMNS);
    for cx in 0..dh::W as i64 {
        for cz in 0..dh::W as i64 {
            let (wx, wz) = (x * 64 + cx, z * 64 + cz);
            let surface = 64 + ((wx * 3 + wz * 5).rem_euclid(17) / 4) as i32;
            let mut c =
                vec![Point { id: ids[0], height: (320 - surface - 1) as u32, bottom: surface + 1, light: 0x0f }];
            c.push(Point { id: ids[1], height: 1, bottom: surface, light: 0x0e });
            c.push(Point { id: ids[2], height: 3, bottom: surface - 3, light: 0 });
            let mut y = surface - 3;
            if rng(&mut seed).is_multiple_of(7) {
                let ore = (rng(&mut seed) % 20) as i32 + 4;
                c.push(Point { id: ids[3], height: (y - (surface - ore)) as u32, bottom: surface - ore, light: 0 });
                y = surface - ore;
                c.push(Point { id: ids[4], height: 2, bottom: y - 2, light: 0 });
                y -= 2;
            }
            c.push(Point { id: ids[3], height: (y + 64) as u32, bottom: -64, light: 0 });
            cols.push(c);
        }
    }
    sections_of(cols, &mut seed)
}

/// A section of these columns, of generation steps and compression modes made up.
fn sections_of(cols: Vec<Vec<Point>>, seed: &mut u64) -> Section {
    let mut s = dh::section_from(&cols);
    s.generation = (0..dh::COLUMNS).map(|_| 4 + rng(seed).is_multiple_of(3) as u8).collect();
    s.compression = (0..dh::COLUMNS).map(|i| i.is_multiple_of(97) as u8).collect();
    s
}

/// A database of the level-0 sections of `n` by `n` and their parents up to the level of one
/// section (`n` a power of two), its rows in the order of their levels, then of x and z; None
/// without the sqlite3 tool.
pub fn database(n: i64) -> Option<Vec<u8>> {
    let mut states = States::default();
    let ids: Vec<u32> = STATES.iter().map(|s| states.id(s)).collect();
    states.opaque[ids[3] as usize] = true;
    states.opaque[ids[2] as usize] = true;
    let mut w = Work::default();
    let mut rows: Vec<([i64; 3], [Vec<u8>; 8])> = Vec::new();
    let mut level: Vec<((i64, i64), Section)> =
        (0..n).flat_map(|x| (0..n).map(move |z| (x, z))).map(|(x, z)| ((x, z), ground(x, z, &mut states))).collect();
    let mut l = 0;
    loop {
        for ((x, z), s) in &level {
            let mut b: [Vec<u8>; 8] = Default::default();
            assert!(dh::blobs(s, &states, &mut w, &mut b));
            rows.push(([l, *x, *z], b));
        }
        if level.len() == 1 {
            break;
        }
        let m = (level.len() as f64).sqrt() as i64 / 2;
        let at = |x: i64, z: i64| level.iter().find(|e| e.0 == (x, z)).map(|e| &e.1).unwrap();
        let mut up = Vec::new();
        for x in 0..m {
            for z in 0..m {
                let q: Vec<Section> = (0..4)
                    .map(|k| {
                        let mut out = Section::default();
                        dh::quadrant(at(2 * x + k / 2, 2 * z + k % 2), &states, &mut w, &mut out).unwrap();
                        out
                    })
                    .collect();
                let mut p = Section::default();
                dh::parent([[&q[0], &q[1]], [&q[2], &q[3]]], &states, &mut p).unwrap();
                up.push(((x, z), p));
            }
        }
        level = up;
        l += 1;
    }
    let hex = |d: &[u8]| d.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let z = |d: &[u8]| hex(&aqr_zstd::compress::compress(d, 3).unwrap());
    let mut sql = String::from(
        "create table FullData (DetailLevel tinyint not null, PosX int not null, PosZ int not null, MinY int not null, \
         DataChecksum int not null, Data blob null, ColumnGenerationStep blob null, ColumnWorldCompressionMode blob null, \
         Mapping blob null, DataFormatVersion tinyint null, CompressionMode tinyint null, ApplyToParent bit null, \
         LastModifiedUnixDateTime bigint not null, CreatedUnixDateTime bigint not null, ApplyToChildren bit null, \
         NorthAdjData blob null, SouthAdjData blob null, EastAdjData blob null, WestAdjData blob null, Regenerate bit null, \
         primary key (DetailLevel, PosX, PosZ));\nbegin;\n",
    );
    for (i, ([l, x, zz], b)) in rows.iter().enumerate() {
        sql += &format!(
            "insert into FullData values({l}, {x}, {zz}, -64, {i}, x'{}', x'{}', x'{}', x'{}', 2, 4, 0, 1700000000, 1700000000, 0, x'{}', x'{}', x'{}', x'{}', null);\n",
            z(&b[0]),
            z(&b[5]),
            z(&b[6]),
            z(&b[7]),
            z(&b[1]),
            z(&b[2]),
            z(&b[3]),
            z(&b[4])
        );
    }
    sql += "commit;\n";
    let path = std::env::temp_dir().join(format!("aqr-minecraft-dh-{}-{n}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let mut c = std::process::Command::new("sqlite3")
        .arg(&path)
        .stdin(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    c.stdin.take().unwrap().write_all(sql.as_bytes()).unwrap();
    let out = c.wait_with_output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let d = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    Some(d)
}
