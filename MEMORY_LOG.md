# ESP32-S3 Memory Footprint Log

Cumulative flash and RAM cost per commit (debug build unless P=r).

- **Flash** = `.flash.text` + `.flash.rodata` + `.dram0.data` + `.flash.appdesc`
- **DRAM**  = `.dram0.data` + `.dram0.bss` (runtime RAM)
- **IRAM**  = `.iram0.text` + `.iram0.vectors` (fast RAM / ISR code)
- **P**: `d` = debug, `r` = release
- **Δ columns**: bytes vs previous commit's build (0 = no embedded build change)

| Date             | P | Flash KB | ΔFlash B | DRAM KB | ΔDRAM B | IRAM KB | ΔIRAM B | Subject |
|------------------|---|----------|----------|---------|---------|---------|---------|---------|
| 2026-06-13 19:48 | d |      663 |        0 |      15 |       0 |      56 |       0 | fix: scope serde_json::Value import to dispatch tests |
| 2026-06-13 19:49 | d |      663 |        0 |      15 |       0 |      56 |       0 | fix: scope serde_json::Value import to dispatch tests |
| 2026-06-13 19:49 | d |      663 |        0 |      15 |       0 |      56 |       0 | fix: scope serde_json::Value import to dispatch tests |
| 2026-06-13 23:41 | r |      476 |        0 |      15 |       0 |      56 |       0 | docs: document footprint git hooks installer for fresh clones |
| 2026-06-14 00:23 | r |      476 |        0 |      15 |       0 |      56 |       0 | feat: add Rust host CLI examples (host_client, watch_pin_led) |
