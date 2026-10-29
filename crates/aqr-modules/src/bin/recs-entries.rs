//! `recs-entries` (aqr-infer-recs).

#![no_main]

aqr_modules::whole!(inputs [heads: 0, tails: 1], |p, _, i, len| aqr_infer_recs::module::decode_entries(p, i, len));
