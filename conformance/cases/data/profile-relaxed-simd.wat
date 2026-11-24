;; Relaxed SIMD (not allowed).
(module
  (import "aqr" "aqr_memory" (memory $work 0))
  (func (export "aqr_apply_whole") (param $params i64) (param $output i64)
    (drop (i32x4.relaxed_trunc_f32x4_s (v128.const i32x4 0 0 0 0)))))
