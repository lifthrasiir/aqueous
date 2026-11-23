;; A module defining a memory of its own.
(module
  (import "aqr" "aqr_memory" (memory $work 0))
  (memory $own 0)
  (func (export "aqr_apply_whole") (param $params i64) (param $output i64)))
