;; An extended constant expression (not allowed).
(module
  (import "aqr" "aqr_memory" (memory $work 0))
  (global $g i32 (i32.add (i32.const 1) (i32.const 2)))
  (func (export "aqr_apply_whole") (param $params i64) (param $output i64)))
