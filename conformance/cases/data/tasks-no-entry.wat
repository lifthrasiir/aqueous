;; A module importing aqr_spawn without the task entry point.
(module
  (import "aqr" "aqr_memory" (memory $work 1))
  (import "aqr" "aqr_spawn" (func $spawn (param i32 i32 i32 i32 i32) (result i32)))
  (@custom "aqr_task_slots" "\02")
  (func (export "aqr_apply_whole") (param $params i64) (param $output i64)))
