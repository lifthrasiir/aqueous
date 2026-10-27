//! `pdf-recons` (aqr-pdf).

#![no_main]

aqr_modules::whole!(inputs [in0: 0, in1: 1, in2: 2, in3: 3, in4: 4, in5: 5, in6: 6, in7: 7, in8: 8, in9: 9, in10: 10], |p, _, i, len| aqr_pdf::module::decode(p, i, len, 1));
