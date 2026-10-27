//! `flate-set` (aqr-flate), its streams and segments decoded as tasks: 8 threads, for batches
//! of 32 segments or about 2 MiB of small streams (the output is the same whatever the number).

#![no_main]

aqr_modules::whole!(resources [res0: 0, res1: 1] inputs [in0: 0, in1: 1, in2: 2], |p, r, i, len| aqr_flate::module::decode(p, r, i, len, 8));
