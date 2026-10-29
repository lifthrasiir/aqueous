//! `sqlite-recons` (aqr-sqlite), the first pass over the tables and the orders of the indexes
//! as tasks: given 8 threads (the output is the same whatever the number).

#![no_main]

aqr_modules::whole!(slots 8, inputs [in0: 0, in1: 1, in2: 2, in3: 3, in4: 4, in5: 5, in6: 6, in7: 7, in8: 8, in9: 9, in10: 10, in11: 11, in12: 12, in13: 13, in14: 14, in15: 15, in16: 16, in17: 17], |p, _, i, len, threads| aqr_sqlite::module::decode(p, i, len, threads));
