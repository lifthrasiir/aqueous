//! `exe-recons/macho-symtab` (aqr-exe), without std as aqr-exe is.

#![no_std]
#![no_main]

aqr_modules::whole!(inputs [types: 0, sects: 1, descs: 2, values: 3, sizes: 4, names: 5, backs: 6, prefixes: 7, suffixes: 8], |p, _, i, len| aqr_exe::module::macho_symtab(p, i, len));
