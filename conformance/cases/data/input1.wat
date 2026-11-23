;; A whole module importing the input 1: an application of one input can't give it.
(module
  (import "aqr" "aqr_memory" (memory $work 0))
  (import "aqr" "aqr_input1" (memory $in i64 0))
  (import "aqr" "aqr_output0" (memory $out i64 0))
  (func (export "aqr_apply_whole") (param $params i64) (param $input i64) (param $output i64)
    (memory.copy $out $in (i64.const 0) (i64.const 0) (local.get $output))))
