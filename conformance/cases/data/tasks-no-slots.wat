;; A class of no task slots (the low 7 bits of its byte are from 1 to 64).
(module
  (import "aqr" "aqr_memory" (memory $work 1))
  (import "aqr" "aqr_spawn" (func $spawn (param i32 i32 i32 i32 i32) (result i32)))
  (@custom "aqr_task_slots" "\00")
  (func (export "aqr_apply_whole") (param $params i64) (param $output i64))
  (func (export "aqr_task") (param $arg i32) (param $i i32)))
