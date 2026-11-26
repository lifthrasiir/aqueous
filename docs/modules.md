# Modules: working rules

Archives carry real Wasm modules built from the recompressors' code; native decoding is an
optimization picked by hash. The rules of execution are the spec's ("Module execution", "Tasks");
the rationale is in `xform-design.md` (§5.5 threads) and `arena-design.md` (memory plans). This
file is what a change to a module's code must keep in mind. CLAUDE.md has the short form.

## Building

- Codec crates: feature `encode` (default) brings in `formula`, aqr-formula, zstd and rayon (never
  aqr-archive: their tests and tools take it as a dev-dependency); `module.rs` is the decoding
  code without aqr-formula. Natively they stay plain libraries.
- aqr-modules: one `[[bin]]` per module (never one module doing several things). `build.py` builds
  them through aqr-ld and writes `wasm/*.wasm`, `*.block` (the module blocks' data),
  `src/known.rs` (name, version, SHA-512/256) and the registry's table in `spec/index.bs`. Every
  host builds the same bytes: aqr-ld, as rustc's wrapper there, makes the crates' `-C metadata`
  (of their paths, with the host's separators, by Cargo) of no path (`src/metadata.rs`).
- After changing any code a module links, commit the modules rebuilt: `cargo aqr` runs build.py
  where that code changed and names the modules it changed (run build.py by hand otherwise); a
  test in aqr-decompress catches stale ones. If you don't rebuild, say so in the commit message.
- Size: aqr-ld trims modules itself (prefer automating in aqr-ld over toolchain flags);
  `AQR_LD_NAMES=1` keeps the name section for size debugging. Module error types must implement
  `Display` (`Debug` of `&str` pulls in core's Unicode tables). Moving code between crates changes
  a module's bytes even when it does nothing new.
- Monomorphize only where a measurement shows it hot; elsewhere use `dyn`: generic copies bloat
  the modules that ship in archives. E.g. aqr-flate's table shapes and the token loop of
  Player/Recorder stay specialized natively (4–8% each otherwise; in Wasm one table of any shape,
  `AnyTable`), while Adaptive uses `dyn Search` (`Corr::SPECIALIZED = false`).

## Running

- aqr-decompress: a known hash runs natively when the application's declared limits hold its plan
  (`needed_by`: the same function the encoder declares with), anything else as WebAssembly.
- Whole modules take their inputs at once; part modules (`entry! … windows(…)`, spec "Module
  execution") stream them through windows of fixed pages, so their memory does not grow with the
  data. Big streams' modules are part modules: those of `crates/aqr-modules/src/bin` with
  `windows(`. Natively some still decode whole on threads (`Run::Threaded` in aqr-native's
  `NATIVE`, not held to the plan), some stream (`Run::Streamed`), some run the part entry through
  aqr-api's emulation (`Run::Part`).
- No variable arity: use Split and separate modules (png-against, leveldb-recons, …).
- Model data (aqr-flate's) is a resource, not embedded in the module.

## Threads

- Fork-join tasks (`aqr_spawn(class, count, records, arg, n)`, the only spawn;
  `aqr_join`/`aqr_take`; `aqr_common::pool::par_map_apart` with `wire::Wire`) and channels
  (flac/pcm pipelines, prores's stripes). Other `par_map`/`scope` uses run serially in Wasm.
- aqr-api's `task::spawn` is class 0 with one region record for all of the working memory;
  `spawn_class(class, ..)` and `spawn_gang(class, ..)` name the class. The spawn record must stay
  in a never-inlined frame (`spawn_record`), or the task's stack clobbers the caller's locals.
- A module spawning tasks declares its classes in the custom section `aqr_task_slots` (one byte
  each: slots 1..=64 in the low 7 bits, a gang class with the top bit; aqr-ld checks it):
  `whole!(slots N, …)` for one class of N slots, its code given N as its threads, or
  `whole!(gang N, …)` for one gang class of a slot, whose gang is N tasks. A gang's tasks start
  together and its limit covers their memories and the messages queued in its channels; the
  channels made (`aqr_chan`) belong to the instance's next spawn, which must be of a gang class
  (the emulation checks the same, by `spawn_gang`), and only that gang's tasks and the spawner
  take their ends. Tasks spawn only classes above their own.
- The decoder picks each class's concurrency from 1 to its slots (`choose_concurrency`: lowered to
  fit `-M` when one is given).
- Port a pipeline only when its speedup is worth the module's growth; report both (png, psd and
  mp3 were measured and not ported).

## Memory

- Each Module carries the exact plan of its working memory (`Module::new(..).with_plan`, or
  `with_task_plan` for a module spawning tasks; the codec's `formula.rs`/`plan.rs`,
  `aqr_api::stack`): the limit of the application's instance, and of a task of each class.
  aqr-compress declares them (`crates/aqr-compress/src/memory.rs`) in the Apply instruction
  (always opcode 16, one task limit for each class of the module).
- An application over `--application-bound` (256 MiB compressing, 512 MiB decompressing) is not
  used / fails; a recompression runs under a heap allowance (`aqr_common::allowance`), each
  format's `heap` in aqr-compress (`src/formats.rs`) turning inputs away up front.
- A module allocates on the stack in the order its plan follows; changing what it allocates means
  changing its plan, which the `*_plans_hold` tests in aqr-decompress's `tests/wasm.rs` check to
  the page, instance and each class (`plans_hold_on_samples` in `tests/samples.rs`, ignored, does
  on `scripts/module-memory.sh`'s samples). The recurring pitfalls are in `arena-design.md`.

## Released modules

The earlier Apply (opcode 11, reserved) and two-parameter `aqr_spawn` are accepted only for the
modules released before a fixed point, those of the releases up to v0.0.20261110 (the `revs` of
`scripts/released-modules.sh`), and their memory limit is not enforced
(`crates/aqr-archive/src/released.rs`: those releases' hashes that `known.rs` no longer has).
Later releases are not added: their archives never hold opcode 11. Run the script
again only when a rebuild takes one of those modules out of `known.rs`.

## Planned

Module bytes are a per-archive cost that dominates small archives. Planned: built-in dictionaries
in the spec (u8-sized module blocks as hash references, dictionary blocks as references to a
built-in dictionary; two 64 KiB dictionaries, module and text, about 60 KB in all). A text
dictionary needs natural language as well as code; a trained dictionary's entropy tables hurt
CJK, so the compressor must be able to use it as raw content too.
