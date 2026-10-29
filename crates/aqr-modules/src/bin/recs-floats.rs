//! `recs-floats` (aqr-infer-recs).

#![no_main]

aqr_modules::whole!(inputs [lifted: 0], |p, _, i, len| aqr_infer_recs::module::decode_floats(p, i, len));
