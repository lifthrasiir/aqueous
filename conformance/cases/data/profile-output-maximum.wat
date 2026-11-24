;; A whole module's output memory of a maximum (only a part module's may have one, its window).
(module
  (import "aqr" "aqr_memory" (memory $work 0))
  (import "aqr" "aqr_output0" (memory $out i64 0 1))
  (func (export "aqr_apply_whole") (param $params i64) (param $output i64)))
