# Perf research, round 2 (run from a pi session, plus /deep-research here)

**Goal:** after Submission #3, find what it takes to cut the **60M** run from ~8 s to ~3 s or less, and say which changes fit the time left before the Sun 09:00 ET freeze. **Research only:** nothing in `src/`, `PLAN.md` or `main` changes. The synthesis session reads the files this round writes and turns them into build cards (`PLAN.md` §6, "Pass 2′").

**Two parallel halves:**
- **A. pi fan-out** (this file, §2–§5): an orchestrator plus one pi **researcher** subagent per track. The number of researchers is **not fixed**: one per track in the final list (§2 step 2).
- **B. /deep-research** (§6): external evidence, run in the synthesis session at the same time. Its report is copied to `research/perf2/deep-research.md`.

**Time box:** start → all reports written **≤ 90 min**. The synthesis follows (≤ 30 min). A track that isn't done by then is marked `failed` in `INDEX.md`, and nobody waits for it.

---

## 0. Ground rules (same as round 1, plus three)

Round 1's rules apply unchanged: `research/perf/README.md` §0 (read-only on the codebase; write only to the output folder; **no timing benchmarks during the fan-out**; correctness non-negotiable; respect fixed constraints; cite sources). Round-2 differences:

1. **Output folder is `research/perf2/`** (not `perf/`). Scratch prototypes go in `research/perf2/scratch/<slug>/`, which is deleted at cleanup.
2. **Size everything at 60M.** The final runs on ~60M events, and some phases scale worse than linearly (dedup 4.0× from 20M to 60M). 20M numbers are only useful to explain that scaling.
3. **No dataset-specific shortcuts, and no other entrants' code.**
   - Fingerprinting, caching or precomputing anything about a particular dataset is out. `rules.md`: "nothing about a dataset can be hard-coded".
   - A fast path for a *common input shape* is allowed only if a runtime check covers every row, and an exact general path handles whatever the check rejects.
   - Don't read, copy or adapt other teams' submission repos.
4. **Read round 1 first.** `research/perf/SYNTHESIS.md` plus your track's round-1 file, if one exists. Don't re-derive what's there: say what changed after Pass 1, and go further.

## 1. Context packet (give to every researcher verbatim, with `00-profile.md` and the track card)

**Task, plan, code:** `rules.md`; `PLAN.md` §4 (decisions), §5a (playbook), §6 (V2 pipeline P0–P4, Pass 1 and Pass 2 cards); `src/v2/{mod,types,scan,split,parse,people,sessions}.rs`, `src/id.rs`, `src/ts.rs`, `src/output.rs`. Round 1: `research/perf/SYNTHESIS.md`, `research/perf/01`–`12`, `research/perf/deep-research.md`.

**Scoring:** wall-clock, container start → exit. The final uses a **fresh ~60M-event dataset (5.4 GB CSV)**, median of 3. Any wrong row scores nothing. Practice board (hidden 20M):
- **V1:** 40.95 s.
- **Submission #3 (Pass 1):** **2.51 s**.
- **Leader:** 0.27 s. About 15 teams sit at 0.29–0.31 s, which is most likely dataset-specific shortcuts. That's not our target (rule 3), but the roofline track (§3 `roofline`) should say how close an honest pipeline can get.

**Scorer:**
- **CPU:** GCP n2, Cascade Lake or newer, **8 vCPU = 4 physical cores × 2 hyperthreads**.
- **Memory:** 24 GB RAM, no swap.
- **Disk and network:** `/data` read-only, page-cache state unknown. No network at run time.
- **Measured ratio:** scorer ≈ **1.1× local**. #3 ran 2.27 s locally vs 2.51 s scored; V1 was 36.2 vs 40.95 s.

**Fixed constraints:**
- Rust, single binary, distroless image.
- `x86-64-v3` target (AVX2; no AVX-512).
- Dependency allow-list: `parquet` (no default features), `rayon`, `memmap2`, `mimalloc`, `hashbrown`, `memchr`. Anything else needs approval: name the crate and say why.
- V1 stays as the fallback and the oracle.

**Current code = Pass 1 (`e6f1279`).** Pass 1 added:
- Category lookup only for purchases.
- Narrower Parquet settings.
- No teardown on exit.
- One bucket set per worker with pre-sized buffers.
- Exact-size dedup output.
- Largest buckets first.
- `memchr` newline search plus a single-pass `split`.

Measured locally, `NCPU=8 taskset -c 0-3,8-11`, warm page cache, `SHOPT_IO_STATS=1`:

| Phase | 20M ms | 60M ms | 60M / 20M | 60M minor faults | What it does |
|---|---|---|---|---|---|
| open | 12 | 8 | | | manifest, lookups, mmap |
| links+events | 835 | **2,811** | 3.4× | 113k | union-find (serial) ∥ parallel event parse → `EvRaw` 48 B, scattered by `event_id` hash |
| clicks | 250 | 528 | 2.1× | 100k | click parse + person lookup → `Ck` by person bucket |
| dedup | 675 | **2,712** | **4.0×** | **522k (≈2 GB of fresh 4K pages)** | per event bucket: hashbrown `event_id → EvRaw`, keep min ingest; survivors → `Ev` 32 B by person bucket |
| sessions | 402 | **1,413** | 3.5× | 34k | per person bucket: sort, sessionize, credit, assists |
| write | 95 | 297 | 3.1× | 17k | Parquet, one part per worker |
| **total** | **2,272** | **7,772** | 3.4× | | peak anon 7.4 GB at 60M (dedup), VmHWM 12.5 GB incl. the mapped input |

- **Cold vs warm:** the first 60M run after other work took 11.4 s, with 7.6k major faults. Expect the scorer to land between the two, nearer warm if it keeps `/data` cached across the 3 runs.
- **Projected scorer time at 60M:** ~8.5–9 s.
- **Host settings:** THP is `always` on the dev laptop; the scorer's setting is unknown.
- **Laptop noise:** 27 GB RAM and it had swap in use, so treat 60M timings as ±10%.

**Input shape (from the generator source, `.tools/generator/src/`):**

| Feature | Value |
|---|---|
| Duplicate rate | events 2–4%, clicks 1–3% |
| Quoted fields | 20% of fields quoted |
| Malformed lines | 0.05% |
| Late rows | 0.5–1.5% |
| Bad user | 0.2% |
| Case variants | 5 per million |
| Duplicate placement | Every emitted line goes to a randomly chosen part (`user.rs` `emit`); parts are written through a shuffle stream (`main.rs`). Assume **no locality**: duplicates and a person's rows are spread across files. Verify before relying on it. |
| File sizes | Zipf, so the largest file holds ~20% of rows. Some files are header-only. |

## 2. Orchestrator steps

1. **Serial profile at 60M (no researchers running).**
   - Build: `cargo build --release`.
   - `data/p60` should already exist; if not, generate it: `.tools/generate --seed 9 --events 60000000 --clicks 15000000 --output data/p60`.
   - Warm the cache: `cat data/p60/*/*.csv > /dev/null`.
   - Run once with `SHOPT_IO_STATS=1 NCPU=8 taskset -c 0-3,8-11`, then profile one run with `perf record -g` (or `perf stat` plus the `[phase]` lines if `perf` is unavailable).
   - Write `research/perf2/00-profile.md`: phase table, top-40 symbols, and minor and major faults per phase. Also record `free -g` and swap use at the time, plus the same table for `data/p20`, so the 60M/20M ratios are visible.
2. **Build the track list.** Start from the seed tracks (§3) and adjust using `00-profile.md`:
   - Add a track for any symbol or phase ≥ 5% at 60M that no track covers.
   - Merge or drop tracks under 2% unless they affect memory or the critical path.
   - Record the final list in `research/perf2/INDEX.md` (round-1 format: `research/perf/README.md` §5) **before** spawning.
3. **Fan out.** Spawn one pi **researcher** per track, all at once if pi allows, otherwise queued.
   - Each researcher gets §0, §1, `00-profile.md`, its track card and the template (§4).
   - Output path: `research/perf2/<NN>-<slug>.md`.
4. **Collect and check.**
   - Every file follows §4, and every idea has gain at 60M, effort, risk, verification and a "fits before freeze?" answer.
   - Re-run a malformed track once, inside the time box.
5. **Clean up.**
   - Delete any persisted researcher sessions, using pi's own session commands; check `pi --help`, don't guess.
   - Delete `research/perf2/scratch/`.
   - Record both in `INDEX.md`.
6. **Hand back.** Same machine: say done. Otherwise commit only `research/perf2/` to a new branch `research-perf2` and push it. Never push to `main`.

## 3. Seed tracks

| Slug | Question | Where to look |
|---|---|---|
| `roofline` | Physical lower bound for an honest full pipeline at 20M and 60M on the scorer. Cover: bytes moved per event per phase, DRAM bandwidth (n2: ~4 channels), AVX2 scan GB/s per core, random-access count (dedup inserts, person probes) × miss latency, ÷ 4 cores (HT +10–20%). Also container start and exit: does the scorer's clock include kernel teardown of ~12 GB of mappings (`exit_mmap`), and would a fork-and-exit trick legally hide it? Conclude with a target time per phase, and how far an honest pipeline can get toward the 0.27 s practice leader. | whole pipeline, `src/main.rs`, `Dockerfile` |
| `dedup-arch` | Dedup is 2.7 s at 60M, scales 4.0×, and makes 2 GB of fresh-page faults. Compare four designs **at 60M**: (a) Pass 2's S4-1 to S4-3 (counted scatter + compact table on L2-sized partitions); (b) a **global atomic winner table** built during parse: `fetch_min` on a packed `(ingest, copy tag)` per `event_id`, so the `EvRaw` scatter and the second scatter disappear and every event checks "am I the winner?" later; (c) **two-pass parse**: keys and ingest first, full parse of winners only; (d) radix-sort dedup. For each: exact handling of E4, E5 and malformed-before-dedup, memory peak, and effort. | `src/v2/mod.rs` P1/P2, `src/v2/types.rs`, `PLAN.md` §6 Pass 2 |
| `scan-avx2` | links+events is 2.8 s at 60M: ~4 GB of events at ~1.4 GB/s aggregate, i.e. ~0.35 GB/s per vCPU. Pass 1 already uses `memchr` plus a single-pass split. Next: a full AVX2 structural scanner (fixed field count; quote validity from bitmasks; commas and newlines are always structural in this dialect), fused with field decode: timestamp SWAR, price digits, `event_type` match, ID packing. What GB/s per core is realistic on Cascade Lake, and what's the minimal correct version that fits in ~6 h? | `src/v2/split.rs`, `src/v2/parse.rs`, `src/v2/scan.rs`, `src/ts.rs`; round-1 `01`, `02` |
| `memory-traffic` | Account for every byte written and read per event across P1–P4 (`EvRaw` 48 B ×2, `Ev` 32 B ×2, sort passes, gathers), and for page faults (522k in dedup). Options: smaller records, reusing buffers across phases (P1 buffers → P2 output), an arena, `MADV_HUGEPAGE` on large anon buffers, pre-faulting from an idle thread during the serial union-find, `MAP_POPULATE`. Gain at 60M for each, and the effect on peak anon. | `src/v2/mod.rs`, `src/v2/types.rs`, `src/main.rs`; round-1 `05`, `08` |
| `sessions-60m` | Sessions is 1.4 s at 60M, scaling 3.5×. Sort cost vs sessionize walk vs person lookups. Re-check Pass 2's S4-4 (sub-partition) against packed-key radix sort and against grouping by dense person index (must keep `u128` person order in events *and* clicks; see `src/v2/sessions.rs` `run_bucket`). Can `user_daily` Parquet encoding run per bucket, overlapped with the tail of the sessions phase? | `src/v2/sessions.rs`, `src/v2/mod.rs` P3/P4, `src/output.rs`; round-1 `06`, `09` |
| `critical-path` | Wall time vs CPU time per phase. Where are cores idle (serial union-find, barriers between phases, the largest-bucket tail, the single-threaded `campaign_daily` write, exit)? Which overlaps are safe: clicks during dedup, links during parse (already done), writing during sessions? Thread count: 8 vs 16 rayon threads on 8 vCPU for memory-bound phases. | `src/v2/mod.rs`, `src/main.rs`; round-1 `11` |
| `input-shape` | From the generator source and `data/p60`, measure the share of rows each fast path would cover: quote-free lines, ID lengths, timestamp format mix (F1/F2/F3 shares), price shapes, `event_type` values. For each proposed fast path, give the per-row runtime check and the exact fallback. Verify the "no locality" claim in §1. Anything that only works for one dataset is out (rule 3). | `.tools/generator/src/*.rs`, `data/p60` (read-only sampling) |

## 4. Researcher output template

Round 1's template (`research/perf/README.md` §4), with two extra columns in the Ideas table:

| # | Idea | Gain at 60M (ms, reasoning) | Effort (h) | Correctness risk + trap | Files | How to verify | **Supersedes / conflicts with Pass 2 card** | **Fits before Sun 09:00 freeze?** |
|---|---|---|---|---|---|---|---|---|

Plus a final section **"Recommended card"**: the one change you would build tonight, written as a `PLAN.md` §6 card. Include scope (files), depends, effort, the change itself, accept criteria and traps.

## 5. Done when

- `00-profile.md` and `INDEX.md` exist.
- Every track is `done` or `failed` inside the time box.
- Persisted sessions and `scratch/` are deleted.
- `git status` shows changes only under `research/perf2/`.

## 6. /deep-research half (run in the synthesis session)

The synthesis session runs it, in parallel with the fan-out. Report → `reports/`, copied to `research/perf2/deep-research.md`. Question to pass:

> Research external, state-of-the-art evidence for making a Rust CSV-to-Parquet clickstream pipeline 3× faster at **60M events (5.4 GB CSV)** on GCP n2 (Cascade Lake, 8 vCPU = 4 cores × 2 HT, 24 GB, x86-64-v3, no AVX-512). The current run takes 7.8 s locally. Phases: parse 2.8 s, dedup 2.7 s (hash table on `u128` event IDs, 2–4% duplicates, keep min ingest timestamp, 2 GB of fresh-page faults), sessions 1.4 s (sort by person and timestamp), write 0.3 s. Context: `research/perf2/README.md` §1 and `research/perf/SYNTHESIS.md` in /home/rue/Documents/Projects/shoptimizer. Focus on what round 1 didn't cover:
> 1. **One Billion Row Challenge** (Java, Rust and C entries): which techniques gave the top results, and which transfer to a multi-phase pipeline. Cover chunking, SWAR and branchless parsing, hash tables, and the subprocess trick for skipping munmap.
> 2. **Concurrent dedup at 10⁷–10⁸ keys:** lock-free and atomic hash tables (folly AtomicHashMap, `fetch_min` on packed values, linear probing with CAS), throughput on 4–8 threads, vs radix-partitioned dedup (DuckDB, Umbra and HyPer papers).
> 3. **Linux memory costs at multi-GB scale:** anon page-fault throughput, THP and `MADV_HUGEPAGE` in Docker on GCP (Container-Optimized OS defaults), and process-exit teardown cost for ~12 GB of mappings. Does Docker's container-exit time include it?
> 4. **Hyperthreading** on memory-bound hash and scatter workloads (Intel Skylake-SP and Cascade Lake measurements).
> 5. **Fastest honest results published** for comparable "parse + dedup + group + aggregate" workloads, to set a realistic floor.
>
> Output a ranked list of ideas: gain at 60M, effort, correctness risk, and sources.
