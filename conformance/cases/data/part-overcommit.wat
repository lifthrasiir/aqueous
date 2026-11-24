;; A part module committing past its output's declared size.
(module
  (import "aqr" "aqr_memory" (memory $work 0))
  (import "aqr" "aqr_input0" (memory $in i64 0 1))
  (import "aqr" "aqr_output0" (memory $out i64 0 1))
  (import "aqr" "aqr_request" (func $request (param i32 i64)))
  (import "aqr" "aqr_commit" (func $commit (param i32 i64)))
  (func (export "aqr_apply_part") (param $params i64) (param $input i64) (param $output i64)
    (call $request (i32.const 0) (local.get $input))
    (memory.copy $out $in (i64.const 0) (i64.const 0) (local.get $input))
    (call $commit (i32.const 0) (i64.add (local.get $output) (i64.const 1)))))
