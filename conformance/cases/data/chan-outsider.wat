;; The instance takes an end of a channel of a gang it didn't spawn: a task of class 0 makes the
;; channel and spawns the gang (class 1), whose task sends to it; once the task of class 0 is
;; taken, the instance receives from the channel.
(module
  (import "aqr" "aqr_memory" (memory $work 1))
  (import "aqr" "aqr_spawn" (func $spawn (param i32 i32 i32 i32 i32) (result i32)))
  (import "aqr" "aqr_join" (func $join (param i32 i32) (result i32)))
  (import "aqr" "aqr_take" (func $take (param i32 i32 i32 i32)))
  (import "aqr" "aqr_chan" (func $chan (param i32) (result i32)))
  (import "aqr" "aqr_send" (func $send (param i32 i32 i32)))
  (import "aqr" "aqr_recv" (func $recv (param i32 i32 i32) (result i32)))
  (@custom "aqr_task_slots" "\01\81")
  (func (export "aqr_apply_whole") (param $params i64) (param $output i64)
    (local $h i32)
    (local.set $h (call $spawn (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 1)))
    (drop (call $join (local.get $h) (i32.const 0)))
    (call $take (local.get $h) (i32.const 0) (i32.const 0) (i32.const 0))
    (drop (call $recv (i32.const 0) (i32.const 0) (i32.const 16))))
  (func (export "aqr_task") (param $class i32) (param $i i32)
    (local $h i32)
    (if (i32.eqz (local.get $class))
      (then
        (drop (call $chan (i32.const 4)))
        (local.set $h (call $spawn (i32.const 1) (i32.const 0) (i32.const 0) (i32.const 1) (i32.const 1)))
        (drop (call $join (local.get $h) (i32.const 0)))
        (call $take (local.get $h) (i32.const 0) (i32.const 0) (i32.const 0)))
      (else
        (call $send (i32.const 0) (i32.const 0) (i32.const 1))))))
