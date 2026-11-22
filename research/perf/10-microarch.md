# 10 microarch

> **Orchestrator validation note (supersedes conflicting claims below):** The 22.21% `FnMut<(usize,)>` closure is P2 bucket dedup (`src/v2/mod.rs:129–166`), not newline scanning. Any scanner saving derived from that share is unsupported; SIMD frequency and SMT effects need scorer-side evidence.

## Summary
The two largest sample buckets (CSV field split 23.7%, the parse fold that inlines the newline scan 22.2%) are byte-at-a-time loops with data-dependent branches; making both byte-parallel is the best microarch lever, worth an estimated 0.9–1.5 s of 60M wall. Second is batch-prefetched hash probing in dedup + person lookup (~0.5–0.85 s). Keep `target-cpu=x86-64-v3`: AVX-512 is not worth a build-target gamble here, and the scoring host may not even have it.

## Current cost
Measured (00-profile.md, 20M, medians of 3, local Zen 4): open 9, links+events 1229, clicks 257, dedup 807, sessions 428, write 147 ms; internal 2924, wall 2962. Naive 3× at 60M: open 27, links+events 3690, clicks 770, dedup 2420, sessions 1280, write 440 ≈ 8.6 s; ×1.13 scorer factor ≈ 9.8 s (03 in §1 says ~8–9 s; both are estimates, not measurements).

Microarch reading of the profile (hypothesis; gperftools has no PMU and lumps inlined callees into callers): 2900 samples at 250 Hz ≈ 11.6 s CPU. `csv::split::<7>` (src/csv.rs:66–108) plus the fold closure holding `scan::find_newline`/`Lines::next` (src/v2/scan.rs:46–83) is 46% of samples ≈ 5.3 s CPU for ~20.6M event lines ≈ 255 ns/line ≈ 11 cycles per input byte. A well-predicted scalar byte loop is ~1–1.5 cycles/byte, so roughly an order of magnitude is going missing; the only plausible source in this code is mispredicts (two compares per byte at csv.rs:71; `contains` + four quote tests per field at csv.rs:88–93) and dependent-load latency. Treat the magnitude as uncertain; the direction is well supported.

Worker counts and startup bounds: 8 rayon threads on 4 cores × 2 threads (src/main.rs:76–84, `NCPU`), 4× ~1 MB L2 and 32 KB L1D per core, ~35 MB shared L3 on the n2 CPU. Pool creation + 8 × ~900 KB `Acc::camp` zeroing (src/v2/sessions.rs:36) + 3 `map_dir` calls ≈ the measured 9–11 ms `open`. Container exec + glibc/libgcc load in distroless is unmeasured; bound it at ≤ 30 ms, i.e. ≤ 1% of a 9.8 s run — startup micro-work is not worth an hour.

Scorer hardware, measured facts: GCP n2 runs Cascade Lake (Xeon Gold 6268CL: 2.8 base / 3.4 all-core turbo / 3.9 single-core) or Ice Lake (Platinum 8373C: 2.6/3.4/3.5), 2 vCPU per core (docs.cloud.google.com/compute/docs/cpu-platforms). With 4 physical cores busy the relevant ceiling is all-core turbo ≈ 3.4 GHz, and the guest reports base frequency only — so any "we are at 3.9 GHz" reasoning is wrong.

## Ideas
| # | Idea | Expected gain at 60M (ms, with reasoning) | Effort (hours) | Correctness risk (none/low/med/high + which trap) | Files touched | How to verify |
|---|---|---|---|---|---|---|
| 1 | Branchless 64-bit SWAR (or AVX2 `movemask`) newline + comma scanning, replacing the per-byte predicate loops | −900 to −1500. Parsing is ~3.69 s of the phase; if scan/mispredict is 60–70% of it and a SWAR/SIMD scan is 2–3× faster on 77-byte lines, ~2.2 s → 0.9–1.3 s. Bounded below by the serial links union-find it overlaps (~0.75 s at 60M) | 6–10 | med. `\r` is data (E20), final line without `\n`, header-only files, empty line is malformed, wrong field count → `None`, quote must wrap the whole field | src/csv.rs, src/v2/scan.rs | G1 (`cargo test`, fixtures, `check-dev`, `diff-seeds`, `SHOPT_CHUNK_BYTES=4096`, `SHOPT_BUCKET_BITS=1`), G4 |
| 2 | Batch (8–16) hash probes with `_mm_prefetch` on the control bytes and slot, in dedup and `people.person` | −500 to −850 of dedup's 2.42 s. Two dependent random probes per event (~120M at 60M) into tables far larger than L2; raising MLP from ~1–2 to 6–8 is the classic 20–35% win on latency-bound hash joins | 8–12 | med. Dedup must still keep min `ingest_s` and count invalid copies (trap: dedup before validation, E5); resizing must stay pre-sized so prefetched addresses stay valid | src/v2/mod.rs, src/v2/people.rs | G1, G4 |
| 3 | Pin the build target and forbid host-dependent codegen; no AVX-512 in the shipped binary; runtime-dispatch only if a kernel ever needs it | −0 to −100, mainly a variance/zero-score control. 256-bit AVX2 stays in the L0 license (no transition at all); any 512-bit instruction drops the core to L1 for ~650–750 µs after the last one, and short bursts pay that dwell without amortizing it | 2–3 | low. A wrong `target-cpu` is an instant SIGILL = zero | .cargo/config.toml | G1 + G4 on the exact built artifact; `cargo bench`/`objdump` sanity that no `zmm` appears |
| 4 | SMT-aware worker/bucket sizing: A/B `NCPU=4/6/8` and `SHOPT_BUCKET_BITS=6/8/10` per phase | −0 to −300 (uncertain sign). 8 threads × 256 live bucket streams per worker may halve each sibling's line-fill buffers; if so 6 threads can win; if not, SMT fills mispredict bubbles and 8 wins by 1.15–1.35× | 3–4 | low. Thread count changes the number of part files, not their content (scripts/compare.py compares rows, order-insensitive) | src/main.rs env only, src/v2/mod.rs | G1 at both chunk sizes, G4 |

## Details
**1. Branchless scanning.** `find_newline` iterates byte by byte with `position` (scan.rs:47–51) and `split` tests `i == len || line[i] == b','` per byte plus a `contains(&b'"')` rescan per field (csv.rs:71, 88). Replace both with an 8-byte SWAR mask (`x ^ 0x2c2c...`, `(x - 0x0101..) & !x & 0x8080..`) and `trailing_zeros`, or 32-byte AVX2 `vpcmpeqb` + `vpmovmskb` — that is inside x86-64-v3, so no new dependency (`memchr` would need D3 approval; its docs claim "over an order of magnitude" throughput over a naive byte search). Field count, quote rules and `\r`-is-data must be preserved exactly: build the delimiter bitmask first, then validate field count and quoting from the mask, never by relaxing the dialect.

**2. Prefetch.** `kept.entry(r.id)` (mod.rs:139) and `people.person` (people.rs:19) are dependent chains: `mix` → ctrl byte → slot → compare. Buffering 16 records, hashing them all, prefetching all ctrl words, then probing turns the chain into MLP. Hashbrown/SwissTable probes one 16-byte metadata group per cache line (abseil.io/about/design/swisstables), so one prefetch per candidate covers the whole group. Keep the tables pre-sized (`with_capacity(total)`, mod.rs:135) so a rehash cannot invalidate prefetched addresses.

**3. target-cpu.** x86-64-v3 = AVX2/FMA/BMI, no 512-bit; that is the right default. Evidence on short bursts: a single 512-bit instruction forces L1 for ~650 µs, and the transition itself halts the core ~10 µs (travisdowns.github.io/blog/2020/01/17/avxfreq1.html). "Light" 512-bit integer/shuffle code costs ~15% frequency at high core counts, heavy FP ~40% (lemire.me, AVX-512 post) — so the usual "AVX-512 always downclocks" story is too strong, but the availability risk is not: "Cascade Lake or newer" does not guarantee AVX-512 on every future SKU. If an AVX-512 scanner is ever wanted, use `is_x86_feature_detected!("avx512bw")` + `#[target_feature]`, never a v4 build.

**4. SMT/buckets.** Phase costs are latency-bound, not bandwidth-bound (60M touches ~2.9 GB of `EvRaw` once, ~20 GB/s is available to 4 CLX cores), which is exactly the regime where SMT wins — but the two siblings share 12 fill buffers and one 1 MB L2, so the 2048 live bucket `Vec`s in the scatter may hurt. This is a knob sweep, not a rewrite.

## Rejected ideas
- **x86-64-v4 default:** zero-score risk on any host without AVX-512, plus L1 dwell on short bursts; the gain is unproven for 77-byte lines.
- **Software prefetch inside the scatter:** the target address depends on the record just parsed, so a prefetch would need a second pass over the buckets (that is track 05's blocked scatter, not a microarch tweak).
- **Non-temporal stores for `EvRaw`:** 48-byte records into 256 streams never fill a full line; NT stores would emit partial-line writes and lose the read-modify-free benefit.
- **`-C target-cpu=native`:** the builder host may not be the scoring host.
- **Startup trimming** (thread pool, mimalloc arenas, mmap): < 30 ms total bound, not worth an hour.
- **`days_from_civil` magic-number division** (1.7% of samples, ts.rs:232–240): real but tiny; fold into track 02.

## Dependencies / conflicts
Idea 1 is the microarch justification for track 01 (SIMD CSV) and track 02 (field parsing); its 0.9–1.5 s must not be counted twice — attribute the gain to track 01 and treat this file as the mechanism. Idea 2 overlaps track 04 (dedup) and track 12 (dimension lookup). Idea 4 conflicts with track 05 (partition scatter), which changes the bucket layout it sweeps. Idea 3 is a precondition for track 01's AVX2 kernel. All four are sequential-phase savings, so they add on wall time, but only down to the point where the parse becomes memory-latency-bound; the ~0.75 s serial links union-find (track 07) is a hard floor on the links+events phase.

## Sources
- Measured profile and phase table: research/perf/00-profile.md (median wall 2962 ms; sample shares).
- Code: src/csv.rs:47–108, src/v2/scan.rs:30–83, src/v2/mod.rs:96–160, src/v2/people.rs:12–30, src/v2/sessions.rs:20–46, src/main.rs:76–84, src/ts.rs:20–45 and 232–240, .cargo/config.toml, Cargo.toml:18–22, scripts/compare.py:9–30, PLAN.md D5/D14.
- Travis Downs, "Gathering Intel on Intel AVX-512 Transitions" (2020): https://travisdowns.github.io/blog/2020/01/17/avxfreq1.html — L0/L1/L2 licenses, ~10 µs transition halt, ~650 µs L1 dwell, 256-bit `vpor` never leaves L0.
- D. Lemire, "AVX-512: when and how to use these new instructions" (2018): https://lemire.me/blog/2018/09/07/avx-512-when-and-how-to-use-these-new-instructions/ — light vs heavy, ~15% vs ~40%, per-core locality.
- GCP CPU platforms (n2 = Cascade Lake 6268CL 2.8/3.4/3.9 GHz, Ice Lake 8373C 2.6/3.4/3.5 GHz, 2 vCPU per core, guest sees base frequency): https://docs.cloud.google.com/compute/docs/cpu-platforms
- Abseil SwissTables design notes (16-byte metadata group per probe, SSE compare): https://abseil.io/about/design/swisstables
- `is_x86_feature_detected!` (runtime AVX-512 detection, `avx512bw`/`avx512vbmi2` listed): https://doc.rust-lang.org/std/arch/macro.is_x86_feature_detected.html
- `memchr` crate, vectorised multi-byte search and its throughput claim: https://docs.rs/memchr/latest/memchr/
- Not fetched this session: x86-64 psABI microarchitecture levels (v3 excludes AVX-512 by definition) — gitlab.com/x86-psABIs/x86-64-ABI.
