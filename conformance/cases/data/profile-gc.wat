;; A type of garbage collection (not allowed).
(module
  (import "aqr" "aqr_memory" (memory $work 0))
  (type $s (struct (field i32)))
  (func (export "aqr_apply_whole") (param $params i64) (param $output i64)
    (drop (struct.new $s (i32.const 1)))))
