;; A whole module of the parameters, one input and two outputs: the parameters, then the input.
(module
  (import "aqr" "aqr_memory" (memory $work 0))
  (import "aqr" "aqr_params" (memory $params i64 0))
  (import "aqr" "aqr_input0" (memory $in i64 0))
  (import "aqr" "aqr_output0" (memory $out0 i64 0))
  (import "aqr" "aqr_output1" (memory $out1 i64 0))
  (func (export "aqr_apply_whole")
        (param $params_len i64) (param $input i64) (param $output0 i64) (param $output1 i64)
    (memory.copy $out0 $params (i64.const 0) (i64.const 0) (local.get $output0))
    (memory.copy $out1 $in (i64.const 0) (i64.const 0) (local.get $output1))))
