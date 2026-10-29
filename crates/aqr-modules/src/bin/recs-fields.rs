//! `recs-fields` (aqr-infer-recs).

#![no_main]

aqr_modules::whole!(inputs [planes: 0], |p, _, i, len| aqr_infer_recs::module::decode_fields(p, i, len));
