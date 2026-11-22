# 06 critical-path

## Summary
The pipeline is a chain of six phases with exactly one cross-phase overlap (`rayon::join` for links ∥ events), and the measured total CPU per wall second is **6.22 of 8 thread-slots** — a deficit, not proof of recoverable idle, because per-phase utilization is unmeasured. Two overlap candidates exist whose gains are **conditional and may be zero**: running clicks inside dedup (cap 572 ms) and restructuring the sessions→write handoff (cap tens of ms). The honest deliverable is therefore a measurement-first card, not a guaranteed win.

## Current cost

Measured locally at 60M (`research/perf2/00-profile.md`; one observation per size, ±10% laptop noise; warm cache): open 8, links+events 3425, clicks 572, dedup 2561, sessions 1319, write 354, internal total 8243, native start→exit 8561 ms; user/sys CPU **40.867 s / 12.389 s**. 20M: internal 2142, native 2199.

Derived (division of two measurements, not a model): 53.256 CPU-s ÷ 8.561 s wall = **6.22 of 8 thread-slots accruing CPU**, i.e. ~1.8 slots not accruing. This is an *accounting* deficit. It may be runnable-but-stalled threads (memory latency still counts as CPU time), barrier waits, page-fault kernel work, or genuinely sleeping threads — the profile cannot distinguish them, and the deficit is **not guaranteed to be fillable**: overlap only pays if the threads are non-runnable, and the current code has no per-task finish-spread data. Sys is 23.3% of CPU, consistent with 402k dedup minor faults (~1.65 GB at 4 KiB) plus teardown (inference, not attribution).

Structural facts (source, not measurement):
- Six sequential phases; the only cross-phase overlap is `rayon::join(links, events)` (`src/v2/mod.rs:168`). Clicks, dedup, sessions and write have no overlap with each other.
- Clicks need only `PersonMap` (`mod.rs:190-206`, probe at `mod.rs:196`), available when the join returns (`mod.rs:186`); they never read `ev_parts`.
- Sessions ends with `.fold(...).reduce_with(Acc::merge)` (`mod.rs:269-286`). Rayon's `reduce_with` is a **parallel binary tree**, not a serial loop: every merge is a serial `Vec::append` inside one task, but different pairs merge concurrently, so the exposed tail is the root merge (roughly half of the accumulated `users`, ~168 MB of the ~336 MB total, copied while the other subtree's owner is also busy) plus `Vec` growth reallocs. Copy volume across levels is ~n·log₂k, mostly parallel.
- Persons are bucket-local and a bucket is owned by one fold producer, so per-producer `users` sets are disjoint — relevant to any per-accumulator write.
- Write is 8-way `par_chunks` (`mod.rs:310-313`); serial `write_campaigns` is ≤ 2001×14 ≈ 28k rows (`types.rs:96`, `rules.md:111,114`) — a few ms.
- Default chunk size is `32 << 20` = **33,554,432 bytes = 32 MiB** (`mod.rs:157`); 5.4 GB / 33.55 MB ≈ 161 chunks, and chunking is per file, so small files yield small chunks (`scan.rs:41-66`).
- Dedup tasks are 256 hash-uniform buckets (`id.rs:41-49`); expected max/mean ≈ 1.006, so ~15 ms tail. Sessions runs largest-first (`mod.rs:63-68`), so its tail is a small bucket.
- Exit/startup gap: native − internal = 57 ms at 20M but **318 ms at 60M**, with VmHWM 4.7 → 12.9 GB. Some of that is plausibly `exit_mmap` of ~13 GB; not isolable without `strace -c` or a variant that drops state before `exit(0)` (inference).

**Missing measurement:** per-phase occupancy and per-task finish spread. A `SHOPT_IO_STATS=1` line reporting each scatter/fold task's elapsed min/max would settle which phases leave cores non-runnable, and therefore whether any overlap below can pay.

## Ideas

| # | Idea | Gain at 60M (ms, reasoning) | Effort (h) | Correctness risk + trap | Files | How to verify | Supersedes / conflicts with Pass 2 card | Fits before Sun 09:00 freeze? |
|---|---|---|---|---|---|---|---|---|
| 1 | Run the click phase inside the dedup `rayon::join`, so click parse + person probe can use slots dedup does not occupy | **0–450, unknown sign**. Hard cap 572 (clicks wall), achievable only if dedup leaves cores non-runnable. If dedup is occupancy- or bandwidth-saturated, the gain is ~0 and added contention can cost time. Needs the finish-spread measurement first | 1.5–2.5 | **Low** semantics. Traps: `overflow`/V1 fallback must be tested after both branches, before any output (today `mod.rs:200`); click dedup still "any copy" (`rules.md:61`) | `src/v2/mod.rs` (P1 tail, P2) | G1 all configs + G4; A/B the `clicks`+`dedup` pair wall at 60M, plus peak `RssAnon` (both live at once) | Conflicts textually with **S4-2** (bucket bits at `mod.rs:196`) and **S4-3** (dedup body); do after S4-1/S4-2 | **Yes**, but only if the spread measurement shows idle |
| 2 | Keep fold accumulators; merge only dense `camp`; write one `user_daily` part per accumulator, and pre-size any merged `Vec` | **0–80**. `reduce_with` is already a parallel tree, so the exposed part is the root merge (~half the rows copied in one task) plus doubling reallocs and ~tens of thousands of fresh-page faults; per-accumulator parts also let a finished producer write instead of waiting | 2–3 | **Low**. Accumulators are disjoint by construction (person ⊂ one bucket ⊂ one producer); row order/file count free (`rules.md:72`) | `src/v2/mod.rs:269-297`, `src/output.rs` (already takes a name) | G1 incl. `SHOPT_BUCKET_BITS=1` (many tiny parts) + G4; assert total row count equals the old path | **Complements** S4-1 (same region, rebase); conflicts with cards assuming one `acc` | **Yes** for the pre-sizing subset (~0.5 h); full split 2–3 h |
| 3 | A/B smaller chunks: default 32 MiB → 8 or 16 MiB | **0–96**. A 32 MiB chunk ≈ 33.55 MB ÷ ~0.35 GB/s ≈ 96 ms per thread (that throughput is itself an estimate); pull-based scheduling loses at most about one in-flight chunk per worker, so the tail is bounded by that and may already be small | 0.5 | **None** — `scan::chunks` preserves whole lines at any target; G1 already runs 4096 B | `mod.rs:157` default only | G1 + G4; bench 8/16/32 MiB at 60M | None | **Yes**, one-line revert |
| 4 | Thread-count A/B in **scorer topology**: `taskset -c 0-3` with `NCPU=4,8,16` | **−50…+200, unknown sign**. More runnable threads can raise memory-level parallelism; 16 threads on 4 physical cores can also regress compute-bound parse, and 16 bucket sets raise anon memory. Local 8C part is not evidence | 0.5–1 | **None** (env only) | none (`main.rs:237-242`) | 3 runs each on p60 under the same taskset, compare per-phase medians; watch `RssAnon` | None | **Yes** (pure measurement) |

## Details

**1. Clicks under dedup.** Wrap the existing click `scatter` (`mod.rs:188-206`) and the dedup `par_iter` (`mod.rs:210-260`) in one `rayon::join`, both borrowing `people` and `ctx`. Do not nest a `par_iter` inside dedup's `par_iter` — one extra join arm, not 256. Keep `people.person` at parse (moving it would break E4, `rules.md:121`). Move the `overflow` check below the join. Because the sign is unknown, gate it on the finish-spread numbers: if the scatter tasks already finish within a few ms of each other, expect nothing but contention.

**2. Per-accumulator write.** `fold` returns up to ~2×threads accumulators; collect them instead of `reduce_with`, merge only `camp` (`sessions.rs:44-52`), and pass each `users` slice to `write_user_daily_named` as its own part. Cheap subset: one `reserve_exact(total)` + `extend_from_slice` before the existing reduce, which removes growth reallocs and their faults while leaving the tree reduce intact.

**3. Chunk tail.** `scatter` pulls from a shared counter (`mod.rs:93-119`), so loss is bounded by in-flight chunks at the end of each scatter (events, links, clicks). 8 MiB quarters the per-chunk time; it cannot help phases that are not tail-bound.

**4. Threads.** The scorer is 4 cores × 2 HT; `NCPU=16` there is four threads per core. Only a `taskset -c 0-3` (siblings 8-11) A/B answers it. Treat as a measurement, not a change.

## Rejected ideas
- **Fork/exec trick to dodge `exit_mmap`**: the scorer times container→exit, so it either does nothing or is measurement evasion; needs fork plumbing on distroless. Leave to the `roofline` track.
- **Start sessions on a bucket as soon as every dedup task contributed**: correct in principle, but 256 completion counters for an unmeasured tail; effort ≫ budget.
- **Remove the background drop threads (`mod.rs:262,289-290`)**: they add runnable threads on 4 cores, but moving ~5.5 GB of frees onto the chain is worse; fix allocations instead (dedup-arch, memory-traffic).
- **Parallelise `write_campaigns`**: ≤ 28k rows, a few ms.
- **Parallel union-find**: serial `from_pairs` (`people.rs:47-58`), ≤ ~0.9 s projected at 60M, hidden under a 3.4 s parse; no expected wall gain, PLAN already excludes it.

## Dependencies / conflicts
- Idea 1 interacts with `dedup-arch` (a global winner table changes dedup's occupancy entirely) and `scan-avx2` (faster clicks shrink the prize, faster dedup shrinks the window).
- Idea 2 touches the same P2→P3 region as **S4-1**; land S4-1 first.
- Joint effect is **unknown and possibly ~0**; it is bounded by the sum of the individual caps, not additive, and any phase that is bandwidth-saturated can make overlap negative. Measure each idea alone.
- All figures assume the scorer keeps `/data` page-cached across its 3 runs; the measured cold first run was 11.4 s.

## Sources
`research/perf2/00-profile.md` (phase table, CPU-s, faults, sample buckets); `src/v2/mod.rs:157,168,186-206,262,269-297,310-313`; `src/v2/sessions.rs:44-52,183-196`; `src/v2/types.rs:57-64,96`; `src/id.rs:41-49`; `src/main.rs:237-252`; `src/v2/scan.rs:41-66`; `rules.md:61,72,111,114,121`; `PLAN.md` §6 S4-1/S4-2/S4-3, §7 G1/G4, §9 budget; Rayon `reduce_with` (tree reduction over indexed producers) — https://docs.rs/rayon/latest/rayon/iter/trait.ParallelIterator.html#method.reduce_with; `research/perf/11-pipeline-shape.md` (its ideas 2 and 3; idea 1 here replaces its serial-reduction premise).

## Recommended card

**C1 — Measure task spread, then overlap clicks with dedup only if it shows idle** · Scope: `src/v2/mod.rs` (`scatter`, P2, `iostats` call site) · Depends: S3-7 merged · 1 h measure + 2 h change · est. 0–450 ms, sign unknown
- Step 1 (always worth doing): have `SHOPT_IO_STATS=1` log, per phase, the min/max elapsed of its worker tasks (and the reduce tree's last-merge time). No output change.
- Step 2 (only if dedup's task spread shows workers finishing at materially different times and slots not accruing CPU): wrap the click `scatter` (`mod.rs:188-206`) and the dedup `par_iter` (`mod.rs:210-260`) in one `rayon::join`; do not add a nested pool; do not change bucket counts.
- Move the `overflow` check below the join; on overflow return `Ok(false)` before any output, as today.
- Accept: G1 in all three configs (default, `SHOPT_CHUNK_BYTES=4096`, `SHOPT_BUCKET_BITS=1 SHOPT_CHUNK_BYTES=100`) + G4 `MATCH`; at 60M, three runs reporting the `clicks`+`dedup` pair wall, total wall, and peak `RssAnon` ≤ 10 GB; **revert if the pair wall is not lower by more than noise**.
- Traps: E4 (never move the `people.person` lookup), E5/invalid-kept-copy, click copies counted twice, and conclusions drawn from the aggregate 6.22/8 ratio rather than the per-task spread.
- Do not: change `scatter`'s ownership model, remove the background frees, or touch V1.
