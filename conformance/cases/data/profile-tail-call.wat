;; A tail call (not allowed).
(module
  (import "aqr" "aqr_memory" (memory $work 0))
  (func $nothing)
  (func (export "aqr_apply_whole") (param $params i64) (param $output i64)
    (return_call $nothing)))
