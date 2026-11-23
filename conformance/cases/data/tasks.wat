;; A whole module of a class of tasks of 2 slots: the instance puts its input of 5 bytes at the
;; offset 1024, spawns two tasks of that region, and its output is their results one after the
;; other; the task i gives the region with its first byte made 'A' + i.
(module
  (import "aqr" "aqr_memory" (memory $work 1))
  (import "aqr" "aqr_input0" (memory $in i64 0))
  (import "aqr" "aqr_output0" (memory $out i64 0))
  (import "aqr" "aqr_spawn" (func $spawn (param i32 i32 i32 i32 i32) (result i32)))
  (import "aqr" "aqr_result" (func $result (param i32 i32)))
  (import "aqr" "aqr_join" (func $join (param i32 i32) (result i32)))
  (import "aqr" "aqr_take" (func $take (param i32 i32 i32 i32)))
  (@custom "aqr_task_slots" "\02")
  (func (export "aqr_apply_whole") (param $params i64) (param $input i64) (param $output i64)
    (local $h i32)
    (memory.copy $work $in (i32.const 1024) (i64.const 0) (i32.const 5))
    ;; one region record: src, len, dst
    (i32.store (i32.const 0) (i32.const 1024))
    (i32.store (i32.const 4) (i32.const 5))
    (i32.store (i32.const 8) (i32.const 1024))
    (local.set $h (call $spawn (i32.const 0) (i32.const 1) (i32.const 0) (i32.const 0) (i32.const 2)))
    (drop (call $join (local.get $h) (i32.const 0)))
    (call $take (local.get $h) (i32.const 0) (i32.const 2048) (i32.const 5))
    (drop (call $join (local.get $h) (i32.const 1)))
    (call $take (local.get $h) (i32.const 1) (i32.const 2053) (i32.const 5))
    (memory.copy $out $work (i64.const 0) (i32.const 2048) (i32.const 10)))
  (func (export "aqr_task") (param $arg i32) (param $i i32)
    (i32.store8 (i32.const 1024) (i32.add (i32.const 65) (local.get $i)))
    (call $result (i32.const 1024) (i32.const 5))))
