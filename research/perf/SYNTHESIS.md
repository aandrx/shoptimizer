# Perf research synthesis (pi fan-out × deep research)

**Inputs:** `00-profile.md` + tracks `01`–`12` (pi, code-level, profile-driven) and the deep-research notes in `research_notes/Rust pipeline performance techniques/` (external evidence; final report → `deep-research.md`). Base code: `bf0d6d5` (V2.0).

**Baseline:** p20 ≈ 2.9 s local (4 cores × 2 threads). Naive 60M projection ≈ **8.9 s local / ~10 s on the scorer**. All gains below are **estimates at 60M, local**, and **not additive** where noted.

---

## 1. What the profile actually says (p20, user-CPU samples)

| Share | Where | Meaning |
|---|---|---|
| **23.7%** + 3.3% | `csv::split::<7>` + `split::<4>` | byte-at-a-time field split: the #1 cost |
| **22.2%** | `run_v2::{closure#5}` | **dedup** bucket work (`HashMap<u128, EvRaw>` inserts). Track 10 mislabels this as newline scan; INDEX corrects it |
| 8.7% | libc (unresolved) | most likely `memcpy`/`memmove` from Vec doubling and the sessions gather; needs instrumentation (08 #4) |
| 8.2% / 4.6% | event / click parse folds | newline search + scatter pushes (inlined) |
| **5.1%** | `HashMap<u128,u16>::get` | **product → category lookup on every event** (+ campaign lookup on clicks) |
| ~4.9% | `parse_civil` / `days_from_civil` / `parse_ts` | timestamps |
| 4.7% / 1.4% | `sort_unstable` Ev / Ck | sessions sort |
| 3.2% | `mi_page_free_list_extend` | fresh-page allocation (page faults / zeroing) |
| ~2% | parquet writer | output |

## 2. Where the two research efforts agree

| Topic | pi (from our code + profile) | Deep research (external evidence) | Verdict |
|---|---|---|---|
| CSV scanning | #1 lever, 0.9–1.9 s (01, 10) | Dialect needs no quote-state: commas/newlines are always real; AVX2 scan 2–4 GB/s/core; csveee (VLDB'26, Rust) 1.8–2.1 GB/s/core with fixed field count | **Top priority.** Do it in two steps (SWAR/cheap first, AVX2 second) |
| Dedup | 0.7–1.1 s: compact `u64 hash + u32 idx` table + L2-sized partitions (04) | Buckets of 11 MB / tables of 17–34 MB at 60M are DRAM-bound; 1024–4096 partitions; reuse the partition hash; prefetch gains vanish once it fits in cache | **Second priority.** Shrink partitions *and* the table |
| Sessions sort | 0.15–0.4 s: sub-partition each bucket, then sort (06) | **Measured locally:** at 256 buckets std sort is already best; at 4096 buckets packed `u64` + radix is 3.7–4.5× faster | Sub-partition first; radix second |
| Bucket count | — | — | **Our own measurement:** `SHOPT_BUCKET_BITS=12` made V2 *slower* (dedup 0.9 s, sessions 1.4 s) because the second scatter becomes 4096 × 4096 `Vec`s. So **don't just raise the global bits**: split dedup bits from person bits, or sub-partition inside a bucket, and replace `Vec<Vec<…>>` with counted/pre-sized buffers (05 #1, 11 #1) |
| Parquet | dictionary off for `user_id`, stats off: 60–130 ms (09) | **Measured locally:** single-thread encode 124 → 52 ms (2.2M rows) with dictionary off + stats off; building `Vec<ByteArray>` costs more than encoding | Cheap win; do it |
| Timestamps | 150–220 ms: SWAR shape check + window day table (02) | SWAR 8-digit, Neri–Schneider calendar; 0.2–0.5 s CPU-wall | Medium priority |
| Build target | keep `x86-64-v3` (10) | AVX-512 downclocks Cascade Lake; n2 may also be Ice Lake | **Keep v3. No v4 A/B.** (frees two submission slots) |
| Threads | A/B 4/6/8 (10) | +11–18% avg from HT; partitioning gains little | Keep 8; low-priority A/B |
| PGO | — | 3–8%, inside the 20% board noise | **Skip** |
| I/O | warm headroom ≤ 250 ms; cold disk 4.6–40 s regardless (08) | cold n2 PD ≥ 6.6 s for 5.5 GB at the 800 MiB/s cap; issue many reads early; `MADV_WILLNEED` is a no-op when cached; huge pages only help anon memory | Can't control cold disk. Keep mmap; add instrumentation |

## 3. Things only one side found (worth keeping)

- **pi 03: look up product category only for purchases.** `Ev.cat` is read only for purchases, so ~97% of events pay a `HashMap<u128,u16>` probe for nothing. **250–400 ms for 1–2 h, low risk.** Best gain per hour in the whole study. (Validity still requires a non-empty product; only the *lookup* moves.) This makes pi 12's dense product array mostly moot.
- **Deep research: skip teardown at exit.** After the Parquet writers are closed, `std::process::exit(0)` skips dropping GBs of Vecs (mimalloc frees + `munmap`). Estimated 50–340 ms; 30 minutes of work. Kernel teardown still happens, so measure.
- **Deep research: mimalloc / huge pages for anon memory.** `mi_page_free_list_extend` is 3.2%. `MIMALLOC_ALLOW_LARGE_OS_PAGES=1` (Dockerfile `ENV`, zero code) and pi 08 #1 (`purge_delay=-1`). 30–150 ms; A/B only.
- **pi 08: the T15 memory gate is mis-specified.** Peak RSS includes the 5.5 GB mapped input. Gate on `RssAnon` (anonymous memory) instead. Cached input can't OOM-kill us; anon peaks during Vec doubling can.
- **pi 11 #2: parse clicks inside the dedup phase** to fill dedup's memory stalls. 100–300 ms, low risk, but only after the scatter redesign.
- **pi 07 / 10: person-map probes** (~70M random probes into a table that leaves L3 at 60M). Prefetch/batch: 100–500 ms, hypothesis only. Measure after dedup is fixed.

## 4. Ranked list (gain per hour, risk-adjusted)

| Rank | Idea | Gain at 60M (local) | Effort | Risk | Source |
|---|---|---|---|---|---|
| 1 | Category lookup only for purchases | 250–400 ms | 1–2 h | low | pi 03 |
| 2 | Parquet: dictionary off (`user_id`), statistics off, offset index off | 60–130 ms | 1 h | low | pi 09, DR (measured) |
| 3 | `process::exit(0)` after writers close | 50–300 ms | 0.5 h | low | DR |
| 4 | Cheap scan wins: SIMD/SWAR newline search + one-pass quote check in `field()` | 400–900 ms | 4–8 h | low | pi 01 #3–4 |
| 4b | Huge pages for anonymous memory (`MIMALLOC_ALLOW_LARGE_OS_PAGES=1` or `MADV_HUGEPAGE` on big buffers); A/B only, zero if the host has THP off | 150–500 ms | 1–2 h | none | DR report #1, pi 08 |
| 5 | Scatter redesign: **one rayon `fold` accumulator per thread** (today one per *split*, each with 256 Vecs), buffers pre-sized from file-size row estimates (no doubling, which re-copies the 2.9 GB of `EvRaw` ~log₂n times); then one counted buffer per task instead of nb² growing Vecs; separate dedup bits from person bits | 200–400 ms for the first two parts, **enables 6–7** | 3–6 h | low | DR report #2, pi 05 #1, 11 #1, our bits=12 result |
| 5b | Process the largest buckets first (whale bucket shouldn't finish last) | 20–60 ms | 0.5 h | none | DR report #9 |
| 6 | Dedup: compact table (`u64` hash + `u32` index, full-ID verify) on L2-sized partitions | 700–1,100 ms | 6–9 h | low | pi 04, DR |
| 7 | Sessions: sub-partition each person bucket (k≈16–64, whole persons), then sort; later packed-`u64` radix | 150–400 ms | 6–10 h | low–med | pi 06, DR (measured) |
| 8 | Full AVX2 structural scanner (fixed field count, quote validity from bitmasks) | 900–1,900 ms total, *includes* rank 4 | 16–24 h | med | pi 01 #1, DR |
| 9 | Timestamp fast path: SWAR shape check + window day table + dedicated ingest F1 parser | 150–220 ms | 5–6 h | med | pi 02, DR |
| 10 | Instrumentation: per-phase faults, `RssAnon`, `VmHWM` behind `SHOPT_IO_STATS=1` | 0 (enables decisions) | 2 h | none | pi 08 #4 |
| 11 | mimalloc large pages / `purge_delay=-1` A/B | 30–150 ms | 1–2 h | none | pi 08, DR |
| 12 | Parse clicks during dedup | 100–300 ms | 6–10 h | low | pi 11 #2 |

**Don't do (this round):** AVX-512 / `x86-64-v4`, PGO/BOLT, hand-written Parquet writer (high format risk for ~250 ms), fused streaming P1→P2, partition-then-parse, parallel union-find (already hidden under parse), Bloom filter in front of the person map, dense product array (superseded by rank 1).

**Realistic combined target:** ranks 1–7 ≈ **2.0–3.0 s** off the 8.9 s local 60M projection (≈ 6–7 s local, ~7 s scorer). Adding rank 8 ≈ another 0.5–1.0 s.

## 5. Proposed next submissions

Time check: freeze Sun 09:00 ET. Keep every step behind G1 (both chunk sizes + bits=1) and G4.

- **Submission #3 (fast, low risk, ~8–12 h):** ranks 1, 2, 3, 4, 4b (A/B), the first half of 5 (one accumulator per thread + pre-sizing), 10. Expected ≈ 1.0–2.0 s off at 60M.
- **Submission #4 (structural, ~10–15 h):** the rest of 5, then 5b, 6, 7. Expected ≈ another 1.0–1.8 s.

**Constraint from the DR report (code reading):** any dense per-bucket person index used for sorting must preserve `u128` person order in *both* events and clicks, because the session walk merge-joins them by comparing `person` (`src/v2/sessions.rs` run_bucket).
- **If time remains:** rank 8 (full AVX2 scanner), then 9.
- **Before the final pick, regardless:** T15, a 60M run checking `RssAnon` and timing.

## 6. Corrections to carry into PLAN.md

- T15's "peak RSS ≤ 8 GB" → "peak `RssAnon` ≤ 10 GB" (mapped input isn't the risk).
- T14 (v3/v4 A/B): drop it. Both sources say keep v3.
- D14 (`SHOPT_BUCKET_BITS`): raising it naively regresses; split into dedup and person bits after the scatter redesign.
