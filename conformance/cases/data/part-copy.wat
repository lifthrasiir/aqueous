;; A part module of one input and one output, each in a window of a page: the output is the input,
;; requested, committed and released a page at a time.
(module
  (import "aqr" "aqr_memory" (memory $work 0))
  (import "aqr" "aqr_input0" (memory $in i64 0 1))
  (import "aqr" "aqr_output0" (memory $out i64 0 1))
  (import "aqr" "aqr_request" (func $request (param i32 i64)))
  (import "aqr" "aqr_release" (func $release (param i32 i64)))
  (import "aqr" "aqr_commit" (func $commit (param i32 i64)))
  (func (export "aqr_apply_part") (param $params i64) (param $input i64) (param $output i64)
    (local $at i64) (local $step i64)
    (block $done
      (loop $next
        (br_if $done (i64.ge_u (local.get $at) (local.get $input)))
        (local.set $step (i64.sub (local.get $input) (local.get $at)))
        (if (i64.gt_u (local.get $step) (i64.const 65536))
          (then (local.set $step (i64.const 65536))))
        (call $request (i32.const 0) (i64.add (local.get $at) (local.get $step)))
        ;; each step starts at a multiple of the window, so at offset 0 of both memories
        (memory.copy $out $in (i64.const 0) (i64.const 0) (local.get $step))
        (call $commit (i32.const 0) (i64.add (local.get $at) (local.get $step)))
        (call $release (i32.const 0) (i64.add (local.get $at) (local.get $step)))
        (local.set $at (i64.add (local.get $at) (local.get $step)))
        (br $next)))))
