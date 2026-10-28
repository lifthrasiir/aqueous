//! `mp3-recons` (aqr-mp3).

#![no_main]

aqr_modules::whole!(resources [res0: 0] inputs [in0: 0, in1: 1, in2: 2, in3: 3, in4: 4, in5: 5, in6: 6, in7: 7, in8: 8, in9: 9], |p, r, i, len| aqr_mp3::module::decode(p, r, i, len, 1));
