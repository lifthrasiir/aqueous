;; A table of 64-bit addresses (not allowed).
(module
  (import "aqr" "aqr_memory" (memory $work 0))
  (table i64 1 funcref)
  (func (export "aqr_apply_whole") (param $params i64) (param $output i64)))
