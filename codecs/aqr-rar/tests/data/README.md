# Test data

Archives of generated inputs (no third-party content): `words.txt` (40,046 B of words from
random syllables), `code.bin` (30,002 B of x86-like bytes with `E8` calls), `table.bin`
(20,000 B of four interleaved channels), `audio.raw` (24,000 B of 16-bit sines),
`small.txt` (20 B). Made on 2026-11-01 with RARLAB's rar 7.23 (`r5_*`) and rar 6.24 with
`-ma4` (`r4_*`, RAR 3.x's format), all with `-ep`:

| archive | switches | inputs |
|---|---|---|
| `r5_m3.rar` | `-m3 -mt1` | words, code |
| `r5_solid.rar` | `-s -m5 -mt1 -mcE+ -mcD+` | words, code, table |
| `r5_stored.rar` | `-m0` | small |
| `r4_m3.rar` | `-ma4 -m3 -mt1` | words, code |
| `r4_solid.rar` | `-ma4 -s -m5 -mt1 -mcE+ -mcD+` | words, code, table |
| `r4_ppmd.rar` | `-ma4 -m5 -mct+` | words |
| `r4_audio.rar` | `-ma4 -m5 -mcA+` | audio |
| `r4_stored.rar` | `-ma4 -m0` | small |
