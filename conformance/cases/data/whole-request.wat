;; A whole module importing aqr_request, which only a part module may.
(module
  (import "aqr" "aqr_memory" (memory $work 0))
  (import "aqr" "aqr_request" (func $request (param i32 i64)))
  (func (export "aqr_apply_whole") (param $params i64) (param $output i64)))
