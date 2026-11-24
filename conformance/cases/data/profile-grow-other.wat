;; memory.grow of a memory other than the working memory (not allowed).
(module
  (import "aqr" "aqr_memory" (memory $work 0))
  (import "aqr" "aqr_output0" (memory $out i64 0))
  (func (export "aqr_apply_whole") (param $params i64) (param $output i64)
    (drop (memory.grow $out (i64.const 1)))))
