;; Exception handling (not allowed).
(module
  (import "aqr" "aqr_memory" (memory $work 0))
  (tag $t)
  (func (export "aqr_apply_whole") (param $params i64) (param $output i64)
    (block $caught
      (try_table (catch $t $caught)
        (throw $t)))))
