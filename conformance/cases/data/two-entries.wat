;; A module exporting both entry points.
(module
  (import "aqr" "aqr_memory" (memory $work 0))
  (func $entry (param $params i64) (param $output i64))
  (export "aqr_apply_whole" (func $entry))
  (export "aqr_apply_part" (func $entry)))
