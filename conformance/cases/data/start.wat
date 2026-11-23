;; A module of a start function.
(module
  (import "aqr" "aqr_memory" (memory $work 0))
  (func $start)
  (start $start)
  (func (export "aqr_apply_whole") (param $params i64) (param $output i64)))
