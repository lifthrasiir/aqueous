;; An output memory of a minimum of 1 page (the minimum is 0).
(module
  (import "aqr" "aqr_memory" (memory $work 0))
  (import "aqr" "aqr_output0" (memory $out i64 1))
  (func (export "aqr_apply_whole") (param $params i64) (param $output i64)))
