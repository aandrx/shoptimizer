# 01 simd-csv

> **Orchestrator validation note (supersedes conflicting claims below):** The 22.21% `FnMut<(usize,)>` frame is the per-bucket dedup loop (`src/v2/mod.rs:129–166`), not the event-line loop. The `split::<7>` and `split::<4>` samples are valid, but the extra newline share and the derived scanner speedup are not established. Measure newline separately; do not use 0.9–1.9 s as a measured or profile-bounded gain.

## Summary
Replace the scalar comma/newline/quote rescans (27.1% `split` + part of 22.2% line-loop samples) with an AVX2 structural bitmask scanner fused into `scan::lines`, validating fields from quote bits without RFC 4180 quote-state. Estimated 60M wall gain 0.9–1.9 s (central ~1.4 s, 10–21%), bounded above by ~2.4 s; main risk is silently changing malformed-line/quote semantics.

## Current cost
Measured (00-profile, 20M, 11.6 s sampled CPU): `csv::split::<7>` 23.72%, `split::<4>` 3.28%, `split::<2>` 0.28% = 27.1%, ~3.16 s CPU. The events fold frame (`closure#5`, 22.21%, ~2.58 s) contains `find_newline` (`src/v2/scan.rs:36,69`), bucket hashing and pushes; newline scanning is plausibly 40–60% of it, ~1.0–1.5 s. Total CSV scanning therefore ≈4.4 s CPU for ~1.8 GB = ~0.4 GB/s/core. At 60M (5.5 GB, 3×) that is ≈13 s CPU, inside an events+links phase of ~3.3 s wall; typed parsing (ts/id/price/category, e.g. `ts::parse_civil`+`days_from_civil`+`parse_ts` 4.9%) is the remainder.

Note `split` is not one pass: `field` (`src/csv.rs:74`) calls `contains(&b'"')` per field, then for quoted fields rescans the interior, so quoted-heavy input is scanned 2–3×.

## Ideas
| # | Idea | Expected gain at 60M (ms, with reasoning) | Effort (hours) | Correctness risk | Files touched | How to verify |
|---|---|---|---|---|---|---|
| 1 | AVX2 32/64-byte classifier producing `,`/`\n`/`"` masks; field spans and quote validity derived from bits; fused into chunk loop | 900–1900 (central 1400). Scanning CPU 13 s → ~2 s; parse CPU ~19 s → ~9 s; events phase 3.3 s → ~1.5 s | 16–24 | med (field count, empty line, CR, quote placement) | `src/csv.rs`, `src/v2/scan.rs`, `src/v2/parse.rs`, `src/v2/mod.rs`, `src/v2/people.rs` | G1 differential test vs scalar `split` over exhaustive short inputs and random bytes; G1 with `SHOPT_CHUNK_BYTES=4096`; G4 on p20; V1 oracle |
| 2 | 8-byte SWAR (u64) scanner as stepping stone/fallback: `haszero` for `,`/`\n`/`"`, `trailing_zeros` | 500–1000; ~1–2 GB/s/core, halves scanning CPU, leaving newer win to idea 1 | 8–12 | low–med (same rules, simpler masking) | same | same differential test; keep scalar path as oracle |
| 3 | SIMD newline search only (`memchr`-style 32-byte compare in `find_newline`) | 200–500 (newline share ~1.0–1.5 s CPU at 20M → ~0.4–0.8 s at 60M, /4 threads) | 2–4 | low (only locates `\n`; `\r` remains data) | `src/v2/scan.rs` | `cargo test scan`; differential chunk/line tests at sizes 1,7,64,4096 |
| 4 | Replace `field()`'s `contains`/interior rescan with a word-at-a-time quote test and validate wrap in one step | 200–450; removes one of 2–3 scans on 20% quoted fields | 3–5 | low (`field()` semantics unchanged) | `src/csv.rs` | existing csv tests + randomized differential vs current `field` |

## Details

**1. Bitmask scanner.** Per 32 B: load, `vpcmpeqb` against `,` `\n` `"`, three `vpmovmskb`. Iterate comma bits to close fields, newline bits to close lines; carry `(field_start, quote_count, quote_first, quote_last)`. A field is quoted-valid iff `quote_count == 2 && quote_first == start && quote_last == end-1`; this accepts `"a"`, `""`, rejects `"a`, `a"`, `"a"x`, `"a""`, and a lone `"` without a separate interior scan. Crucially, **commas are delimiters regardless of quotes** (`rules.md:22`, test `src/csv.rs:209`), so no PCLMUL quote-state/prefix-XOR is needed — unlike simdcsv's RFC 4180 path (`github.com/geofflangdale/simdcsv` README step 2–3). Chunks already end on `\n`/EOF (`src/v2/scan.rs:42`), so no quote or line state crosses chunks. Lines longer than a block just continue accumulating bits and field state; bail as soon as a 7th comma/8th field appears. Estimated Cascade Lake throughput 2–4 GB/s/core: simdcsv reports 0.55 cycles/byte and 5.3 GB/s on a ~3 GHz desktop (`deepwiki.com/geofflangdale/simdcsv/7.3-performance-metrics`), while the n2 Xeon Gold 6230N is 2.3/2.7 GHz (`cpu-benchmark.org/cpu/intel-xeon-gold-6230n/`), so ~4.5 GB/s is a ceiling and memory/HT lower it.

**2. SWAR.** Same state machine on 8-byte words with `(x - 0x0101..) & !x & 0x8080..`; ~1–2 GB/s/core. This is the safer first landing because it exercises the state machine and differential tests without intrinsics, and remains the portable fallback under `cfg(target_feature = "avx2")` (build target is x86-64-v3, `.cargo/config.toml:2`).

**3. Newline-only.** Independent, tiny change; useful if idea 1 slips. `chunks()` also calls `find_newline` once per 32 MiB (`scan.rs:42-67`), negligible.

**4. Quote fast path.** Keep `split` API; replace `value.contains(&b'"')` with an 8-byte SWAR membership test and check wrap arithmetically. Subsumed by idea 1 but low-risk and independently shippable.

**Amdahl/overlap bounds.** Savings are parse-CPU only; dedup/sessions/write are untouched (~4.4 s of ~8.9 s at 60M). Events parse overlaps the serial links union-find (~0.75 s at 60M, `src/v2/mod.rs:76`); if parse wall falls below that, further gains are hidden, capping this track near 2.4 s. Additional non-additivity: if track 02 halves typed parse, this track's share shrinks; if the scanner becomes memory-bound, track 08's bandwidth work is a prerequisite. Hyperthreads mean CPU savings do not divide cleanly by 8.

**Traps.** Empty line must be malformed, not one null field (`csv.rs:181`); trailing comma keeps field count (`csv.rs:170`); final line without `\n` is a line; `\r` stays in the last field (`csv.rs:135`); wrong field count, quote not wrapping a whole field (`csv.rs:186`); spaces are data; `id::pack` overflow must still surface as `Overflow` (`parse.rs:20,57`); every line is either `Skip`/`Overflow`/`Rec` exactly as now.

## Rejected ideas
- RFC 4180 quote-state (PCLMUL prefix-XOR): wrong dialect; commas inside quotes are not special here.
- `memchr` crate: not on the dependency allow-list and gains are subsumed by the fused scanner.
- AVX-512: A/B-only per PLAN T14; Cascade Lake downclock and complexity for simple compares.
- Two-pass "store all masks then parse" (simdcsv 4-chunk buffering): extra traffic; fusion already provides ILP.
- SIMD numeric/date parsing: belongs to track 02.

## Dependencies / conflicts
Depends on D9 chunking and its whole-line invariant (`PLAN.md:101`, `scan.rs:42`); any change in track 08/11 to chunking or file-per-thread splitting must preserve it. Conflicts with track 02 if both change `parse::event`/`click` signatures — coordinate a shared field-slice/offset interface. Interacts non-additively with 02, 05 (scatter), 08 (bandwidth).

## Sources
- `src/csv.rs:48` `split`, `:74` `field`; `src/v2/scan.rs:36,42,69`; `src/v2/parse.rs:13,95`; `src/v2/mod.rs:76-114`; `src/v2/people.rs:38-40`.
- `rules.md:20-22` CSV dialect, `:132` quoted fields 20%, `:133` malformed 0.05%.
- `PLAN.md:172` O3, `:101` D9, `:176` O7, `:357` G1, `:360` G4; `research/perf/00-profile.md` symbol table; `.cargo/config.toml:2`.
- https://github.com/geofflangdale/simdcsv (README pipeline, AVX2); https://deepwiki.com/geofflangdale/simdcsv/7.3-performance-metrics (0.55007 cycles/byte, 5.29778 GB/s); https://arxiv.org/abs/1902.08318 (simdjson bitmask stage 1); Mühlbauer et al., "Instant Loading for Main Memory Databases", VLDB 2013; https://cpu-benchmark.org/cpu/intel-xeon-gold-6230n/ (Cascade Lake clocks).

Uncertainty: the newline share of `closure#5` and the post-SIMD scanner rate are hypotheses derived from samples and published numbers, not measurements in this checkout; no timing was run during the fan-out.
