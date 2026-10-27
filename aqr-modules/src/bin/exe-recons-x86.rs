//! `exe-recons/x86` (aqr-exe), without std as aqr-exe is.

#![no_std]
#![no_main]

aqr_modules::whole!(inputs [in0: 0, in1: 1, in2: 2, in3: 3, in4: 4, in5: 5, in6: 6, in7: 7, in8: 8, in9: 9, in10: 10, in11: 11, in12: 12, in13: 13, in14: 14, in15: 15, in16: 16, in17: 17, in18: 18, in19: 19, in20: 20, in21: 21, in22: 22, in23: 23, in24: 24, in25: 25, in26: 26, in27: 27, in28: 28, in29: 29, in30: 30, in31: 31, in32: 32, in33: 33, in34: 34, in35: 35, in36: 36, in37: 37, in38: 38, in39: 39, in40: 40, in41: 41, in42: 42, in43: 43, in44: 44, in45: 45], |p, _, i, len| aqr_exe::module::x86(p, i, len));
