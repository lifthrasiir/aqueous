;; A whole module that traps.
(module
  (import "aqr" "aqr_memory" (memory $work 0))
  (func (export "aqr_apply_whole") (param $params i64) (param $output i64)
    unreachable))
