;; NaNs that arithmetic makes, written out: 0/0, a signalling NaN plus 1, the square root of -1
;; and the demotion of a NaN of a payload, of f32 and f64, and 0/0 of each lane of f32x4. Under
;; the deterministic profile each is the canonical NaN of positive sign (44 bytes in all).
(module
  (import "aqr" "aqr_memory" (memory $work 0))
  (import "aqr" "aqr_output0" (memory $out i64 0))
  (func (export "aqr_apply_whole") (param $params i64) (param $output i64)
    (f32.store $out (i64.const 0) (f32.div (f32.const 0) (f32.const 0)))
    (f32.store $out (i64.const 4) (f32.add (f32.const nan:0x200001) (f32.const 1)))
    (f64.store $out (i64.const 8) (f64.div (f64.const 0) (f64.const 0)))
    (f32.store $out (i64.const 16) (f32.demote_f64 (f64.const -nan:0x8000000000001)))
    (f64.store $out (i64.const 20) (f64.sqrt (f64.const -1)))
    (v128.store $out (i64.const 28)
      (f32x4.div (v128.const f32x4 0 0 0 0) (v128.const f32x4 0 0 0 0)))))
