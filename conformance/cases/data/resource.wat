;; A whole module of a resource and an output: the output is the resource's stored content.
(module
  (import "aqr" "aqr_memory" (memory $work 0))
  (import "aqr" "aqr_resource0" (memory $res i64 0))
  (import "aqr" "aqr_output0" (memory $out i64 0))
  (func (export "aqr_apply_whole") (param $params i64) (param $resource i64) (param $output i64)
    (memory.copy $out $res (i64.const 0) (i64.const 0) (local.get $output))))
