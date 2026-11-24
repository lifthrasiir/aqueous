;; Grows its working memory by 100 pages (6.25 MiB).
(module
  (import "aqr" "aqr_memory" (memory $work 0))
  (func (export "aqr_apply_whole") (param $params i64) (param $output i64)
    (drop (memory.grow (i32.const 100)))))
