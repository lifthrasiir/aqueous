// Made by build.sh.

use crate::{Known, hex};

pub const KNOWN: &[Known] = &[
    Known {
        name: "leveldb-recons",
        version: 1,
        sha512_256: hex("790d7155f6ca4df0e91f92631edc92a34c040ed2c787872d7d327fa0a23a478d"),
        wasm: include_bytes!("../wasm/leveldb-recons.wasm"),
    },
    Known {
        name: "lz4-recons",
        version: 1,
        sha512_256: hex("0e75353bbf3545811a461aa98e54f87c31d4581e4934464ff65671db1b8620ea"),
        wasm: include_bytes!("../wasm/lz4-recons.wasm"),
    },
    Known {
        name: "snappy-recons",
        version: 2,
        sha512_256: hex("90c46a690886d086ef2fb16d8dc63524644cf634d35c82108ac561ec087c9159"),
        wasm: include_bytes!("../wasm/snappy-recons.wasm"),
    },
];
