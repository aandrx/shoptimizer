# 06 sort-sessionize

> **Orchestrator validation note (supersedes conflicting claims below):** Two 16-bit LSD passes cover only 32 bits. Sorting a general 64-bit `(person_rank, ts)` key needs four such passes unless a narrower bit range is proved. Recompute the claimed traffic and gain; keep the click-ID tie-break.

## Summary
Replace the per-bucket comparison sort with a two-level hash partition by person (complete persons per sub-run, sorted locally), optionally followed by a small LSD radix on a packed `(person-rank, ts)` key. Bounded gain at 60M is ~150–400 ms wall (2–4% of ~8.9 s), almost all inside the sessions phase; the main risk is splitting a person across sub-runs and double-counting his sessions.

## Current cost
Measured 20M (00-profile.md, Ryzen 7 8840HS, 4 cores × 2 threads): sessions median 444 ms of 2,962 ms wall (15.9%). In the flat profile, comparison-sort symbols are `quicksort<Ev>` 3.17%, `small_sort_general<Ev>` 1.48%, `median3_rec<Ev>` 0.17%, `quicksort<Ck>` 0.86%, `small_sort_general<Ck>` 0.52% — **≈6.2% of all sampled CPU**. `run_v2::closure#5` (22.21%) is the P2 dedup map, not sessions (`mod.rs:145` vs `mod.rs:171`); the sessions fold/reduce helpers appear at 3.48% (`sessions::Acc` + `merge`). Sort is therefore plausibly 40–60% of sessions CPU — a hypothesis from symbol attribution, not a phase-level measurement.

Projection to 60M: events survive at ~96%, so ~58M events over 256 buckets ≈ 226k events/bucket (7.2 MB at `size_of::<Ev>() == 32`), vs 78k (2.5 MB) at 20M. Both exceed Cascade Lake's 1 MB L2/core, so sorts and the `person()` walk degrade superlinearly: expect 1.2–1.5 s for sessions, not a clean 3×. Person count is `E/30` (`rules.md:105`) ≈ 2M, ~7,800 persons per bucket; five whales hold 1–2% of all events each (`rules.md:105`), so one bucket can carry ~5× the mean, and a single person cannot be split across threads without breaking session order.

## Ideas
| # | Idea | Expected gain at 60M (ms) | Effort (h) | Correctness risk | Files touched | Verify |
|---|---|---|---|---|---|---|
| 1 | Two-level partition: inside each person bucket, hash-partition Ev and Ck into k sub-runs (k ≈ 32–128), then sort each sub-run (cache-resident) instead of one 7 MB sort | 150–400 (sort CPU ~0.6–0.9 s, 50–70% removable; Amdahl-capped by phase) | 6–10 | Low–med: E16 equal-ts, credit tie; a person split across sub-runs double-counts | `sessions.rs:186-206`, `mod.rs:145-168` | G1 + G4 |
| 2 | LSD radix on packed u64 `(person_rank:u32, ts:u32)`, 2 × 16-bit passes, replacing comparison keys | 80–250 (2 linear passes vs ~18 partition levels; 32 B records) | 8–12 | Med: rank assignment must be collision-free and per sub-run | `sessions.rs`, `types.rs` | G1 + G4 |
| 3 | Assist accounting: day-major `camp[day][campaign]` layout and per-day flush instead of a strided `camp[a*n_days+d][ASSISTED]` write per active campaign | 20–100 (one 640 B-strided cache line per assist today, `sessions.rs:170-172`) | 3–5 | Low–med: assist exactly once per campaign per session, never organic | `sessions.rs`, `mod.rs:239-262` | G1 + G4 |
| 4 | Shrink records: `Ev` 32→16 B and `Ck` 48→32 B using a u32 person rank (u128 kept once per run) | 50–150 (halves sort/concat traffic, `mod.rs:177-186`) | 6–10 | Med: recover the exact u128 for `user_id`; may collide with track 05 | `types.rs`, `mod.rs`, `sessions.rs` | G1 + G4 |

Ranked by gain/hour: 3, then 1, then 4, then 2.

## Details
**1. Hash partition instead of sort.** `run_bucket` currently sorts 226k 32-byte records by `(person:u128, ts)` (`sessions.rs:187`). Because the bucket was already chosen by `id::bucket(person, bits)` (`id.rs:42-48`), every person's events are complete within it. Partitioning by low bits of `id::mix(person)` into k buffers preserves that property (a person maps to one sub-run), so each sub-run can be sorted and walked independently: no merge, no cross-run person. Sub-run size ~3.5k events at k=64 — L1/L2 resident — so `sort_unstable` becomes cheap without touching the algorithm. The extra pass reads/writes ~7.5 MB per bucket (~30–50 ms total at 60M). Walk each sub-run separately, or the `while evs[j].person == person` loop (`sessions.rs:192`) silently truncates a person at the boundary. The same partition must be applied to clicks; a person with no events is harmless. A free preliminary A/B: `SHOPT_BUCKET_BITS=8|12` (`mod.rs:66`) — PLAN.md:202 records **no measurable difference at 8 vs 10 at 20M**, so treat 60M retest as a 0.5 h experiment, not a win.

**2. Radix.** A 160-bit key is 20 byte-passes — worse than quicksort, so radix is only viable after idea 1 assigns a dense u16/u32 person rank inside the bucket. Then a 2 × 16-bit LSD pass over a u64 key is 2 linear passes per sub-run and needs one temp buffer. DuckDB's implementation makes the same argument (radix beats comparison/merge on fixed-width keys) and notes sortedness barely matters for radix, which also answers the track card's "near-sorted" question: our rows are shuffled across files (`rules.md:115`), and pdqsort already detects real patterns in linear time (pdqsort README), so there is nothing to exploit.

**3. Assists.** Every assist increment is `camp[credit*n_days + d]` with `credit` varying, i.e. a 448–672 B stride (`sessions.rs:171`), one cache line per active campaign. Reordering to `[day][campaign]` makes the active loop sequential; or accumulate `assist_count[a]` in an L1-sized u32 array and flush per (person, day). Semantics must stay "one per campaign per session, including the crediting campaign, organic never assists" (`rules.md:66`); flush before the day changes at `sessions.rs:128` and at person end.

**4. Record shrink.** `Ev.person` is 16 of 32 bytes and constant across a person run; a u32 rank halves sort moves and the P3 concatenation (1.9 GB at 60M). Verify `user_id` is still the byte-wise smallest ID (packed order, `id.rs:2-7`) and that `UserRow.person` is the u128, not the rank.

## Rejected ideas
- Pure LSD radix on a 160-bit `(person, ts)` key: 20 passes, no better than quicksort without a dense rank.
- Exploit near-sorted input: rows are shuffled by the generator (`rules.md:115`); a sortedness check costs a full pass and pdqsort already handles genuine runs.
- Per-person `Vec<Ev>` built during dedup: ~2M allocations and random 32-byte pushes, worse locality than partitioning.
- Dropping `click_id` from the click sort key alone: saves u128 compares but needs a min-scan per equal-ts group; fold it into idea 2 rather than ship separately.

## Dependencies / conflicts
Idea 1 touches P2's emit (`mod.rs:155`) if sub-buckets are created there; conflicts with track 05 (partition-scatter) and track 04 (dedup) on `mod.rs`/`types.rs`. Idea 4 conflicts with both. Idea 3 is self-contained in sessions.rs plus `write_campaigns` (`mod.rs:239`). Raising bucket bits interacts with dedup table sizing (track 04).

## Sources
- `src/v2/sessions.rs:95-206` (person walk, credit, assist loop, both sorts); `sessions.rs:166-172` (camp stride); `src/v2/mod.rs:145-186` (dedup emit, P3 concatenation); `src/v2/types.rs:35-53` (record sizes); `src/id.rs:10-48` (pack order, bucket).
- `PLAN.md:107-108` (D15/D16), `PLAN.md:178-179` (O9/O10), `PLAN.md:202` (bucket-bits measurement), `PLAN.md` §7 (G1/G4), §9 traps/self-checks; `rules.md:54-66,74-95,105-115`; `00-profile.md` (phase table, flat profile).
- DuckDB, "Fastest Table Sort in the West" — https://duckdb.org/2021/08/27/external-sorting (radix sort, sortedness has little effect).
- pdqsort — https://github.com/orlp/pdqsort (linear-time pattern detection).
- Balkesen et al., "Multi-Core, Main-Memory Joins: Sort vs. Hash Revisited", VLDB 2014 — https://www.vldb.org/pvldb/vol7/p85-balkesen.pdf (cache-aware partitioning/sorting).
- Satish et al., "Engineering a Multi-core Radix Sort", Euro-Par 2011 — https://link.springer.com/chapter/10.1007/978-3-642-23397-5_16 (multi-pass radix throughput).
- Rust `sort_unstable_by_key` docs — https://doc.rust-lang.org/std/primitive.slice.html#method.sort_unstable_by_key (key evaluation is not free; comparator alternative).

Caveats: all 60M numbers are extrapolations; the profile has no PMU counters and no per-phase CPU split, so sort's share of sessions is inferred from flat samples. Bundle-bits A/B and any timing must wait for a quiet machine (no fan-out benchmarks). The whale-person serial tail is a hypothesis from `rules.md:105` and needs per-person instrumentation to confirm.
