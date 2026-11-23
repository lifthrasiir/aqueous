;; A module importing a function not of the interface.
(module
  (import "aqr" "aqr_memory" (memory $work 0))
  (import "aqr" "aqr_clock" (func $clock))
  (func (export "aqr_apply_whole") (param $params i64) (param $output i64)))
