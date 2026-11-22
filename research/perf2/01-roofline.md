# 01 roofline

## Summary
Local 60M runs span 7.77–8.56 s. Published SIMD scan rates describe other kernels, not a lower bound or a diagnosis of this program; the 0.8–1.2 s scan scenario, 1.6–2.0 s design model, and 2.4–3.0 s target below depend on bandwidth, cache residency, miss overlap and scorer startup. Treat them as sensitivity estimates, not evidence that a 3 s implementation fits before freeze.

## Current cost
Measured (00-profile, 60M, `NCPU=8 taskset -c 0-3,8-11`, warm cache; two observations where they differ): open 8; links+events 2811/3425; clicks 528/572; dedup 2712/2561; sessions 1413/1319; write 297/354; internal 7772/8243; native start→exit 8561 ms. Faults (second run): 266k, 110k, 403k, 37k, 10k. `RssAnon` 7.24 GB after dedup; `VmHWM` 12.9 GB. Input ≈5.4 GB (decimal), 60M events + 15M clicks.

Model-free derivation from those numbers (60M events, 4 physical cores):

| Phase | Measured ms | Core-time per event | Cycles @3 GHz (est.) |
|---|---|---|---|
| links+events | 2811–3425 | 187–228 ns | 560–684 |
| dedup | 2561–2712 | 171–181 ns | 513–542 |
| sessions | 1319–1413 | 88–94 ns | 264–282 |
| write | 297–354 | 20–24 ns | 60–71 |

Parse throughput: ~4.5 GB of events in 2.81 s on 4 cores = **0.40 GB/s per core**, versus csveee's published 1.8 GB/s with quoting / 2.1 GB/s without. That 4–5× gap is measured, and it is the main honest headroom.

### Physical floor vs targets (kept separate)
**A. Design-independent floor (only assumes every row must be read, validated, deduped, attributed and written).** Read 5.4 GB once; parse at published SIMD rates (5.4 GB ÷ (4 cores × 1.8–2.1 GB/s) = 0.64–0.75 s); add start/exit. Result **0.8–1.2 s local**. This excludes our intermediate records entirely — a different design could plausibly land here, so this number says nothing about any other entrant.

**B. Our-design floor.** Requires our passes: `EvRaw` 48 B write+read, `Ev` 32 B, per-bucket hash, sort, Parquet. Traffic estimate (line-granularity, **not counters**): compulsory ≈290 B/event (18 GB), today ≈600–800 B/event (36–48 GB) from hash-slot RMW (128–256 B/event, `mod.rs:215`), person probes (`mod.rs:242`), gather (`mod.rs:275-284`) and doubling. At the measured 7.77 s those estimates imply 4.6–6.2 GB/s aggregate (1.2–1.6 GB/s/core) — but that figure is only as good as the model, so treat it as consistency evidence, not a measurement. Floor: **1.6–2.0 s**; realistic target **2.4–3.0 s**.

**C. Bandwidth reference (units checked).** DDR4-2933 = 2933 MT/s × 8 B = 23.5 GB/s per channel; 6 channels per Cascade Lake socket = **~141 GB/s theoretical** (Intel ARK). A 4-core VM cannot approach that; 25–35 GB/s aggregate streaming is an *assumption*, and the honest statement is only that our measured 0.40 GB/s/core parse rate is far below any plausible ceiling.

### Assumption ledger and sensitivity
| Assumption | Value used | Effect if wrong |
|---|---|---|
| DRAM latency | 90 ns | Dedup+lookup stall scales linearly |
| MLP per core | 10 (optimistic) | 140M misses × 90 ns = 12.6 s serial ÷ 4 cores = 3.15 s; at MLP 2 → 1.6 s, at MLP 10 → 0.32 s. **MLP is unmeasured.** |
| Fault cost | 1.5–3 µs (entry/exit + zeroing 4 KiB) | 403k faults = 0.6–1.2 s CPU, 150–300 ms wall; doubles/halves with the constant |
| THP effective | 2 MB pages actually backed | 0 gain if host is `never` or allocation is not huge-page-aligned |
| Exit teardown | see below | 0–318 ms |

### Exit gap and kernel teardown
The 318 ms gap is `native 8561 − internal 8243`, and it is **unattributed**: process spawn, ELF/runtime init, rayon pool creation, and `exit_mmap` of 7.2 GB anon all live there. Teardown is synchronous in the dying task (`exit_mm → mmput → __mmput → exit_mmap`), and a parent's `wait` returns only after it (LKML RFC), so the container's measured lifetime includes it. External points disagree in scale: ~280 ms for 16 GB (~18 ms/GB) and 56 ms/GB for a 320 GB munmap; scaling 7.2 GB by the first gives ~125 ms, which the 318 ms gap cannot confirm. So **teardown ∈ [0, 318] ms, best estimate 50–150 ms** — an assumption, not a measurement. Fork-and-exit cannot hide it: the container's lifetime includes its child (1BRC #138), and the parent cannot reap before the child's `exit_mm`. S3-3's `process::exit(0)` (`main.rs:252`) already removed the userspace half; only less mapped anon helps.

## Ideas
| # | Idea | Gain at 60M (ms, reasoning) | Effort (h) | Correctness risk + trap | Files | How to verify | Supersedes / conflicts with Pass 2 card | Fits before Sun 09:00 freeze? |
|---|---|---|---|---|---|---|---|---|
| 1 | Explicit anonymous arena for `ev_out` (and the biggest maps) with `MADV_HUGEPAGE`; keep plain `Vec` fallback | 100–300 (dedup 403–522k faults ≈1.6–2 GB of 4 KiB pages; at 1.5–3 µs each = 0.6–1.2 s CPU ÷ 4; 2 MB pages cut count ~50×). All constants assumed | 3–4 | low; traps: THP `never` → 0, alignment/compaction stalls, don't touch `RssAnon` gate | `src/v2/mod.rs` P2 out, new `src/v2/arena.rs` (`memmap2`) | p60 `SHOPT_IO_STATS=1`: dedup `minflt` ≤100k, dedup ≤2.3 s; G1 + G4 | Refines S4-1 output allocation; overlaps 04 memory-traffic | Yes |
| 2 | Remove the `EvRaw` pass: dedup winner table built in P1 (`fetch_min` on packed `(ingest, copy)`), resolve person after links, emit `Ev` once | 1000–1800 (est.): deletes 48 B write + 48 B read/event (5.8 GB) and most slot amplification; dedup 2.56 s → 0.8–1.1 s. Model-dependent, not a bound | 6–9 | med; traps E4 (kept copy's user), E5 (invalid kept copy drops event), malformed lines never win, equal-ingest ties | `src/v2/mod.rs` P1/P2, new `src/v2/dedup.rs`, `src/v2/types.rs` | Differential vs current `HashMap`; G1 bits=1 + 4096-B chunks; G4 p20; p60 | Supersedes S4-1 dedup side and S4-3; owns 02 | Only the first half (≤4 h) |
| 3 | Leave exit alone except for less anon; no fork/subprocess trick | 0–150, most likely 50–100; bounded above by the 318 ms gap | 0.5 | none; trap: background frees can raise peak anon | none (`main.rs:252` already) | Time `[phase] total` → process exit on p60 with an external timer; G4 | Extends S3-3 | Yes |
| 4 | Roofline stop rule + per-phase targets in PLAN §9 | 0 directly; avoids spending hours within 30% of a floor | 0.5 | none (stderr/docs only) | `PLAN.md` §9, `src/main.rs` | One p60 run reproduces §Current cost | Amends S3-7/S3-9 acceptance numbers | Yes |

## Details
**1 Arena.** mimalloc commits segments in small slices, which is why 403k faults appear; `MIMALLOC_ALLOW_LARGE_OS_PAGES` requires a hugetlbfs pool and is not viable here (`research/perf/08-io-memory.md:32`), and S3-8 recorded no adoption (`PLAN.md:204`). So back the exact-size P2 output (count pass already exists, `mod.rs:238-247`) with one `mmap` + `madvise(MADV_HUGEPAGE)` and carve slices; if THP is off, fall back to today's `Vec<Vec<Ev>>`. Gain is the fault share only, not the whole dedup time.

**2 Remove a pass.** The only idea that attacks our design's compulsory traffic rather than its amplification. Roofline caps it at ~1.8 s because dedup's probes remain. Correctness lives in E4/E5 ordering: winner selection precedes validation exactly as now (`types.rs` `EvRaw`, PLAN D12).

**3 Exit.** Keep `process::exit(0)`; do not add processes. The only lever is mapped anon, and the 318 ms gap is an upper bound, not an attribution.

**4 Stop rule.** Freeze targets (local ms): dedup ≤1.0 s, links+events ≤1.4 s, sessions ≤0.7 s, write ≤0.3 s, total ≤4.5 s. A phase within 30% of its floor is not worth further work.

## Rejected ideas
- Any conclusion about other entrants' 0.27 s: our traffic model bounds *our* design only, so it cannot explain or delimit another implementation. Chasing it is out by rule 3, on grounds of dataset hard-coding, not on a roofline inference.
- `MIMALLOC_ALLOW_LARGE_OS_PAGES` in the Dockerfile: needs hugetlbfs; S3-8 recorded no adoption.
- `MAP_POPULATE` on 5.4 GB: serialises I/O and faults into `open` (round-1 08).
- Fork/subprocess teardown: container lifetime includes the child; no legal gain.
- `MADV_WILLNEED`-style whole-file prefault: same serialisation problem in `open`.
- Radix sort at 256 buckets: measured slower than `sort_unstable` at 7.4 MB buckets (DR sort benchmark).

## Dependencies / conflicts
Idea 1 depends on S3-7 and S4-1's counted output; conflicts with 04's arena design (do one, not both) and with the T15 wording (`RssAnon`, not `VmHWM`). Idea 2 is 02's design space — do not build both. Idea 3 conflicts with any buffer-reuse plan that raises peak anon. Idea 4's numbers must be restated if 02/03 change phase boundaries.

## Sources
`research/perf2/00-profile.md` (phase/fault/RSS tables); `research/perf/SYNTHESIS.md` §1–4; `research/perf/01-simd-csv.md`; `research/perf/08-io-memory.md:32` (large-page ENV not viable); `research/perf/deep-research.md` (660 cycles/row, csveee rates, 33.5 MB tables); `PLAN.md:37-41,204,392-441,562`, D8/D12/D14; `src/v2/mod.rs:99,105,215,238-250`; `src/v2/sessions.rs:187-188`; `src/v2/scan.rs:29-30`; `src/main.rs:242,252`; `src/v2/types.rs` `EvRaw`. External: LKML RFC `https://lkml.iu.edu/2607.2/06062.html` (synchronous teardown; ~280 ms/16 GB); patchwork 56 ms/GB `https://patchwork.kernel.org/comment/22057615/`; 1BRC #138; csveee PVLDB 2026 `https://db.in.tum.de/~ellmann/papers/csveee.pdf`; Intel ARK 2nd-gen Xeon (6 channels, Cascade Lake); kernel THP doc; NSF THP measurement `https://par.nsf.gov/servlets/purl/10578888`; Docker "Running containers".

## Recommended card
**R2-1 — Huge-page arena for the dedup output, plus the stop rule** · Scope: new `src/v2/arena.rs`, `src/v2/mod.rs` (P2 output and its exact-size count pass), `PLAN.md` §9 targets · Depends: S3-5, S3-7 merged · 3–4 h · est. 100–300 ms.

Change: allocate the P2 survivor buffer as one anonymous mapping sized by the existing count pass, `madvise(MADV_HUGEPAGE)`, and hand `run_bucket` slices instead of `Vec<Vec<Ev>>`; keep today's `Vec` path when the mapping or THP advice fails. Do not add a Dockerfile `ENV`, and leave the exit path as it is. Then write the freeze targets from §Current cost into PLAN §9 with the note that they are design-specific, not physical.

Accept: G1 in all three configs, G4 `MATCH` on p20; one p60 run with `SHOPT_IO_STATS=1` showing dedup `minflt` ≤100k (from 403–522k) and dedup ≤2.3 s; `RssAnon` peak ≤7.4 GB (unchanged), output byte-identical to the pre-change p60 run.

Traps: THP `never` on the scorer makes the gain 0, so the fallback must be automatic; huge-page alignment must not change record order or slice boundaries; a failed `madvise` is not an error; do not gate on `VmHWM`; the gain is fault cost only, so if `minflt` does not fall, revert rather than tune.
