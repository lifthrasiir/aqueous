;; A spawn of region records past the end of the working memory.
(module
  (import "aqr" "aqr_memory" (memory $work 1))
  (import "aqr" "aqr_spawn" (func $spawn (param i32 i32 i32 i32 i32) (result i32)))
  (import "aqr" "aqr_join" (func $join (param i32 i32) (result i32)))
  (import "aqr" "aqr_take" (func $take (param i32 i32 i32 i32)))
  (@custom "aqr_task_slots" "\01")
  (func (export "aqr_apply_whole") (param $params i64) (param $output i64)
    (local $h i32)
    (local.set $h (call $spawn (i32.const 0) (i32.const 1) (i32.const 65530) (i32.const 0) (i32.const 1)))
    (drop (call $join (local.get $h) (i32.const 0)))
    (call $take (local.get $h) (i32.const 0) (i32.const 0) (i32.const 0)))
  (func (export "aqr_task") (param $arg i32) (param $i i32)))
