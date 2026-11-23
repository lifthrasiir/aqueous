;; Every instance waits: the task receives from a channel the instance never sends to, while the
;; instance joins the task.
(module
  (import "aqr" "aqr_memory" (memory $work 1))
  (import "aqr" "aqr_spawn" (func $spawn (param i32 i32 i32 i32 i32) (result i32)))
  (import "aqr" "aqr_join" (func $join (param i32 i32) (result i32)))
  (import "aqr" "aqr_take" (func $take (param i32 i32 i32 i32)))
  (import "aqr" "aqr_chan" (func $chan (param i32) (result i32)))
  (import "aqr" "aqr_recv" (func $recv (param i32 i32 i32) (result i32)))
  (@custom "aqr_task_slots" "\81")
  (func (export "aqr_apply_whole") (param $params i64) (param $output i64)
    (local $c i32) (local $h i32)
    (local.set $c (call $chan (i32.const 1)))
    (local.set $h (call $spawn (i32.const 0) (i32.const 0) (i32.const 0) (local.get $c) (i32.const 1)))
    (drop (call $join (local.get $h) (i32.const 0)))
    (call $take (local.get $h) (i32.const 0) (i32.const 0) (i32.const 0)))
  (func (export "aqr_task") (param $c i32) (param $i i32)
    (drop (call $recv (local.get $c) (i32.const 0) (i32.const 16)))))
