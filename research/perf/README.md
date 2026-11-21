# Perf research fan-out (run from a pi session)

**Goal:** find every credible way to make V2 faster on the **final 60M-event run**, ranked by expected gain per hour of work and correctness risk. This is **research only**: no changes to `src/`, `PLAN.md` or `main`. A separate session synthesizes the results from the files this fan-out writes.

**Who runs this:** one pi session acting as the **orchestrator**, using pi's **researcher** subagent for each track. The number of researchers is **not fixed**: it is one per track in the final track list (step 2), which the orchestrator derives from the seed tracks plus the profile in step 1.

---

## 0. Ground rules (for the orchestrator and every researcher)

1. **Read-only on the codebase.** Never edit `src/`, `PLAN.md`, `rules.md`, `fixtures/`, `reference/`, `scripts/`, `Makefile`, `Cargo.*`, `Dockerfile`. Never commit to `main`.
2. **Write only to `research/perf/`.** Each researcher writes exactly one file: `research/perf/<NN>-<slug>.md` (template in §4).
3. **No timing benchmarks during the fan-out.** Researchers run concurrently on one laptop, so wall-clock numbers taken during the fan-out are meaningless. Estimates must come from reasoning, published numbers, or the serial profile from step 1. A researcher may write a tiny *correctness* prototype under `research/perf/scratch/<slug>/` (its own Cargo project, never part of the main crate), but must not report timings from it.
4. **Correctness is non-negotiable.** Any idea that changes output semantics is out. Every idea must say how it would be verified (gates G1/G4 in `PLAN.md` §7) and which `rules.md` trap it could break (`PLAN.md` §9 trap list).
5. **Respect the fixed constraints** below; an idea that needs to break one must say so explicitly and justify it.
6. Cite sources (URLs, paper/project names, file:line in this repo) for every non-obvious claim.

## 1. Context packet (give this to every researcher verbatim, plus their track card)

**Task:** `rules.md` (full spec). **Plan, decisions and V2 design:** `PLAN.md` §4 (decisions D1–D17), §5a (optimization playbook O1–O12), §6 (V2 pipeline P0–P4, record layouts, status). **Code:** `src/v2/{mod,types,scan,parse,people,sessions}.rs`, `src/id.rs`, `src/csv.rs`, `src/ts.rs`, `src/output.rs`.

**Scoring:** wall-clock, container start → exit, on a **fresh ~60M-event dataset** (~5.5 GB CSV), median of 3; any wrong row scores nothing.

**Scorer machine:** GCP n2, Intel Xeon **Cascade Lake or newer**, **8 vCPU = 4 physical cores × 2 hyperthreads**, 24 GB RAM, no swap, no network at run time, `/data` read-only (page-cache state unknown), image build ≤ 10 min with network. Measured: scorer ≈ **1.13× slower** than the dev laptop (AMD Ryzen 7 8840HS, Zen 4, 8C/16T).

**Fixed constraints:** Rust single binary; dependency allow-list `parquet` (no default features), `rayon`, `memmap2`, `mimalloc`, `hashbrown` (others need approval: say which and why); `x86-64-v3` build target by default (AVX2; AVX-512 is an A/B candidate only); distroless runtime image; V1 must stay as the fallback and oracle.

**Current V2 (commit `bf0d6d5`) on the 20M practice-size set, `NCPU=8 taskset -c 0-3,8-11` (4 cores × 2 threads):**

| Phase | ms | What it does |
|---|---|---|
| open | ~8 | read manifest/products/campaigns, mmap all CSVs |
| links+events | ~1,100 | **links union-find (serial, ~250 ms) overlapped with parallel event parse** → `EvRaw` (48 B) scattered into 256 buckets by `event_id` hash |
| clicks | ~260 | parallel click parse + person lookup → `Ck` (48 B) by person bucket |
| dedup | ~650 | per event bucket: `hashbrown` map `event_id → EvRaw`, keep min ingest; survivors → `Ev` (32 B) by person bucket |
| sessions | ~400 | per person bucket: `sort_unstable` events by (person, ts), clicks by (person, ts, click_id); sessionize, credit, assists; dense campaign×day counters |
| write | ~120 | `user_daily` (~2.2M rows at 20M) as one Parquet part per worker; `campaign_daily` |
| **total** | **~2,700** | peak RSS 5.5 GB (incl. 1.8 GB mapped input). V1 single-threaded: 36 s. |

At 60M expect roughly 3× each phase (sorts and hash tables slightly worse), i.e. ~8–9 s locally unless something changes.

## 2. Orchestrator steps

1. **Serial profile first (no researchers running).** Build `cargo build --release`, generate `data/p20` if missing (`.tools/generate --seed 7 --events 20000000 --clicks 5000000 --output data/p20`), then profile the V2 binary on `data/p20` with `NCPU=8 taskset -c 0-3,8-11` using `perf record -g` (or `perf stat` + `[phase]` timings if `perf` is unavailable). Save the top-40 symbols and the phase timings to `research/perf/00-profile.md`. This file goes to every researcher.
2. **Build the track list.** Start from the seed tracks in §3. Add a track for any hotspot in `00-profile.md` that ≥ 5% of total time and isn't covered; merge or drop tracks the profile shows are negligible (< 2% of total) unless they matter at 60M (memory, I/O). Record the final list in `research/perf/INDEX.md` (§5) **before** spawning.
3. **Fan out:** spawn one pi **researcher** subagent per track, concurrently (as many at once as pi allows; queue the rest). Each gets: §0, §1, `00-profile.md`, its track card, and the §4 template. Output path: `research/perf/<NN>-<slug>.md` where `NN` is the track's number in `INDEX.md`.
4. **Collect and check:** every track has a file that follows §4 (all sections present, every idea has gain/effort/risk/verification). Re-run any researcher whose file is missing or malformed (once). Mark status in `INDEX.md`.
5. **Clean up:** delete every researcher session this fan-out created, using pi's own session management (check `pi --help` for the delete/remove command; don't guess). Delete `research/perf/scratch/` too. Record the deleted session IDs in `INDEX.md`.
6. **Hand back:** if this pi session runs on the same machine as the synthesis session, just say done. Otherwise commit `research/perf/` (only that folder) on a new branch `research-perf` and push it. Never push to `main`.

## 3. Seed tracks

Each is one researcher, unless step 2 merges, splits, adds or drops tracks.

| Slug | Question | Where to look |
|---|---|---|
| `simd-csv` | How fast can the CSV field split (`csv::split`, newline search in `scan.rs`) go with SWAR/AVX2 bitmask scanning (simdcsv/simdjson-style), given quotes in ~20% of fields and strict malformed-line rules? Estimate GB/s per core on Cascade Lake. | `src/csv.rs`, `src/v2/scan.rs`, `src/v2/parse.rs` |
| `field-parsing` | Fast paths for F1/F2/F3 timestamps (fixed offsets, SWAR digit validation, branch-light calendar check), price digits, event_type match, product/campaign lookup. What's the per-row cost floor? | `src/ts.rs`, `src/v2/parse.rs`, `src/v1.rs` (`parse_price`, `kind_of`) |
| `parse-work-avoidance` | Which bytes/fields can be skipped or deferred per row without breaking dedup-before-validation (O4)? E.g. skip parsing `user`/`product`/`price` for duplicate copies, validate lazily, early-reject paths ordered by frequency. | `src/v2/parse.rs`, `rules.md` Events/Clicks |
| `dedup` | Faster event dedup: sort-based vs hash, smaller keys (64-bit hash + verify), dedup fused into the parse scatter, bucket sizing for L2 (Cascade Lake: 1 MB L2/core), duplicate rate 2–4%. | `src/v2/mod.rs` P2 |
| `partition-scatter` | Memory traffic of the two scatters: record compaction (EvRaw 48 B → ?), software write-combining buffers, pre-sizing vs Vec growth, single-pass alternatives, NUMA-free radix partitioning best practice. | `src/v2/mod.rs` P1/P2, `src/v2/types.rs` |
| `sort-sessionize` | Session phase: LSD radix sort on (person, ts) packed keys vs `sort_unstable`, exploiting near-sorted input, grouping persons by hash instead of sort, cache behaviour of the per-person walk and assist counters. | `src/v2/sessions.rs` |
| `links-unionfind` | Links phase (~250 ms serial at 20M, ~0.75 s at 60M): parallel union-find, better ID→index maps, or avoiding the map; does overlap with event parse already hide it at 60M? | `src/v2/people.rs` |
| `io-memory` | mmap vs `read` into buffers, `MAP_POPULATE`/`madvise`, transparent huge pages, page-fault cost for 5.5 GB, cold vs warm page cache on GCP persistent disk, peak RSS at 60M and cgroup accounting of mapped file pages. | `src/v2/scan.rs`, `PLAN.md` §9 |
| `parquet-output` | Output cost at 60M (~7M user rows): the `parquet` crate's overhead vs a minimal hand-written writer (fixed schema, PLAIN, uncompressed), parallel parts, avoiding per-row string allocs. | `src/output.rs`, `src/v2/mod.rs` P4 |
| `runtime-startup` | Fixed costs: container start, binary load, rayon pool spin-up, mimalloc vs glibc for this allocation pattern, static linking, thread count (8 vs 16 workers on 8 vCPU with HT). | `src/main.rs`, `Dockerfile`, `Cargo.toml` |
| `microarch` | Cascade Lake specifics: AVX2 vs AVX-512 downclocking for short bursts, hyperthread sharing of the parse loop, prefetching in scatters/hash probes, branch-misprediction hotspots, `target-cpu` choices that are safe on "Cascade Lake or newer". | whole pipeline, `.cargo/config.toml` |
| `pipeline-shape` | Whole-pipeline restructuring: fusing phases, overlapping click parse with event parse/dedup, streaming instead of materializing, reducing the number of full passes over 60M records. | `src/v2/mod.rs` |

## 4. Researcher output template (`research/perf/<NN>-<slug>.md`)

```markdown
# <NN> <slug>

## Summary
Three sentences max: the best idea, its expected gain at 60M, and the main risk.

## Current cost
What this area costs now (from 00-profile.md and the §1 table), at 20M and projected at 60M, with reasoning.

## Ideas
| # | Idea | Expected gain at 60M (ms, with reasoning) | Effort (hours) | Correctness risk (none/low/med/high + which trap) | Files touched | How to verify |
|---|---|---|---|---|---|---|

## Details
One subsection per idea: how it works, why it applies to this data, code sketch if useful, pitfalls.

## Rejected ideas
Ideas considered and dropped, one line each with the reason.

## Dependencies / conflicts
Ideas from other tracks this one depends on or conflicts with.

## Sources
URLs, papers, projects, file:line references.
```

## 5. `research/perf/INDEX.md` (orchestrator maintains)

```markdown
# Perf research index
Run started: <ISO time>   Base commit: <sha>   Machine: <hostname or "same as synthesis">

| NN | Slug | Status (pending/done/rerun/failed) | File | Researcher session ID | Deleted |
|---|---|---|---|---|---|

## Track list changes vs seed (§3)
Added / merged / dropped tracks and why (cite 00-profile.md).

## Cleanup
Sessions deleted: <list>. Scratch deleted: yes/no.
```

## 6. Done when

- `00-profile.md` and `INDEX.md` exist, every track in `INDEX.md` is `done` with a template-conformant file, all researcher sessions are deleted and listed, `research/perf/scratch/` is gone.
- Nothing outside `research/perf/` changed (`git status` shows only that folder).
