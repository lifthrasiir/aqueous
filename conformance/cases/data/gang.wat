;; A gang of two tasks: the task 0 sends 'hi' to the channel the instance made, the task 1
;; receives it and gives it; the output is the result of the task 1.
(module
  (import "aqr" "aqr_memory" (memory $work 1))
  (import "aqr" "aqr_output0" (memory $out i64 0))
  (import "aqr" "aqr_spawn" (func $spawn (param i32 i32 i32 i32 i32) (result i32)))
  (import "aqr" "aqr_result" (func $result (param i32 i32)))
  (import "aqr" "aqr_join" (func $join (param i32 i32) (result i32)))
  (import "aqr" "aqr_take" (func $take (param i32 i32 i32 i32)))
  (import "aqr" "aqr_chan" (func $chan (param i32) (result i32)))
  (import "aqr" "aqr_send" (func $send (param i32 i32 i32)))
  (import "aqr" "aqr_recv" (func $recv (param i32 i32 i32) (result i32)))
  (@custom "aqr_task_slots" "\81")
  (func (export "aqr_apply_whole") (param $params i64) (param $output i64)
    (local $c i32) (local $h i32)
    (local.set $c (call $chan (i32.const 1)))
    (local.set $h (call $spawn (i32.const 0) (i32.const 0) (i32.const 0) (local.get $c) (i32.const 2)))
    (drop (call $join (local.get $h) (i32.const 0)))
    (call $take (local.get $h) (i32.const 0) (i32.const 0) (i32.const 0))
    (drop (call $join (local.get $h) (i32.const 1)))
    (call $take (local.get $h) (i32.const 1) (i32.const 2048) (i32.const 2))
    (memory.copy $out $work (i64.const 0) (i32.const 2048) (i32.const 2)))
  (func (export "aqr_task") (param $c i32) (param $i i32)
    (if (i32.eqz (local.get $i))
      (then
        (i32.store16 (i32.const 100) (i32.const 0x6968))
        (call $send (local.get $c) (i32.const 100) (i32.const 2)))
      (else
        (call $result (i32.const 200) (call $recv (local.get $c) (i32.const 200) (i32.const 16)))))))
