;; A working memory of 64-bit addresses.
(module
  (import "aqr" "aqr_memory" (memory $work i64 0))
  (func (export "aqr_apply_whole") (param $params i64) (param $output i64)))
