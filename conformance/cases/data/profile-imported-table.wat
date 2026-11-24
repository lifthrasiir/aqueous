;; An imported table (not allowed).
(module
  (import "aqr" "aqr_memory" (memory $work 0))
  (import "aqr" "aqr_table" (table 0 funcref))
  (func (export "aqr_apply_whole") (param $params i64) (param $output i64)))
