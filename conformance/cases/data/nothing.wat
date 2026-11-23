;; A whole module of one output that it doesn't import, so that the output is zero bytes; it
;; calls aqr_log on a range out of bounds and aqr_heartbeat, neither of which does anything.
(module
  (import "aqr" "aqr_memory" (memory $work 0))
  (import "aqr" "aqr_log" (func $log (param i32 i32)))
  (import "aqr" "aqr_heartbeat" (func $heartbeat))
  (func (export "aqr_apply_whole") (param $params i64) (param $output i64)
    (call $log (i32.const -16) (i32.const 1000))
    (call $heartbeat)))
