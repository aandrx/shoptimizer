# 11 pipeline-shape

> **Orchestrator validation note (supersedes conflicting claims below):** The generator is locally available under `.tools/generator/src/` (from `generator.zip`); the statement that it cannot be inspected is not applicable here. Do not assume favorable file locality without checking it.

## Summary
The pipeline is CPU-work-bound, not barrier-bound: parse+dedup+sessions dominate and every phase already uses all 8 rayon threads, so restructuring the skeleton is worth roughly 250–700 ms at 60M (3–8% of ~8.9 s), not 2×. The best single idea is fusing the dedup→sessions handoff (drop the 256-way gather copy and the 65k growing buckets), worth ~120–300 ms. Risks are low, but the gains are non-additive with tracks 04–06 and cannot compensate for parse cost.

## Current cost
From `00-profile.md` (median 2,962 ms wall at 20M) and the §1 table, ×3 at 60M: open 27, links+events ~3,700 (incl. ~750 ms serial union-find), clicks ~780, dedup ~2,400, sessions ~1,330, write ~390 → ~8.9 s. Bounds below are on that denominator.

Evidence the shape costs little *by itself*: `rayon::join` is work-stealing and "attempts to only execute code in parallel when there are idle CPUs" (docs.rs), so the existing links∥events overlap already fills idle slots; the phases after it are plain `par_iter` over 256 buckets (`src/v2/mod.rs:129`, `:172`), each saturating the pool. What remains is pass count and handoff traffic: at 60M, EvRaw ≈ 2.9 GB written then read (`Ev` 32 B, mod.rs:149), ~1.9 GB of `Ev` copied again in the gather (`evs.extend_from_slice`, mod.rs:179), plus per-dedup-task `vec![Vec::new(); nb]` growth (mod.rs:149) and ~65,536 small `Vec`s. That is a few GB of extra DRAM traffic against ~9 s of CPU — material but secondary.

Hard constraint found in the source and spec: dedup must group by `event_id` *globally* before any person assignment, because "for about 1 in 64 duplicated events the later copies name a different user" (`rules.md:121`) and the kept copy's user counts (E4). Any "partition once by person" design is therefore wrong; the two-level partition (event_id → person) is not accidental.

## Ideas
| # | Idea | Expected gain at 60M (ms, with reasoning) | Effort (hours) | Correctness risk | Files touched | How to verify |
|---|---|---|---|---|---|---|
| 1 | Fuse dedup→sessions handoff: dedup writes `Ev` into fixed per-person-bucket arenas (atomic bump cursors), sessions sorts in place; no `ev_out`, no gather, no per-bucket `Vec` growth | 120–300. Removes ~1.9 GB gather copy + ~1.9 GB of doubling copies + 65k allocs; memcpy-bound, ~40–60 GB/s aggregate on 4 cores, plus latency effects. Hard cap: total `Ev` traffic | 10–16 | med — must keep exact surviving-record multiset; traps E4/E5, min-ingest tie | `src/v2/mod.rs:129-194`, `src/v2/sessions.rs:186` | G1 (incl. `SHOPT_CHUNK_BYTES=4096`, `SHOPT_BUCKET_BITS=1`), G4 |
| 2 | Parse clicks inside the dedup phase (they only need `PersonMap`, which exists after the join at mod.rs:76-100) so click parsing's memory-stall slots interleave with dedup's random probes | 100–300. Cap = click phase 780; work is conserved, so only idle/SMT slack is recoverable. Head-of-line removal ~1 barrier | 6–10 | low — person lookup order changes nothing; click window/credit unchanged | `src/v2/mod.rs:102-167` | G1, G4; click rows sampled from fixture |
| 3 | Stream output: write `user_daily` parts directly from the sessions fold instead of building `Vec<UserDaily>` + per-column vectors after a global `reduce_with` | 80–200. Cap = write phase 390; saves 2 of 3 materializations of ~7M rows and one barrier | 8–12 | low — row order and file count are explicitly free (`rules.md:72`) | `src/v2/mod.rs:192-204`, `src/output.rs` | G1, G4, `make check-dev` |
| 4 | Partition-then-parse: pass 1 hashes only the `event_id` field into 256 buckets of 16 B line handles (+ ingest_s), pass 2 parses and dedups per bucket with a 20 B table; never materialize 2.9 GB of `EvRaw` | 0–600, very uncertain. Saves a 2.9 GB write+read, costs a second cache-line-wide scan of 5.5 GB and random re-reads | 30–40 | high — dedup-before-validation must hold for invalid copies (E5); late/invalid paths | `src/v2/{mod,parse}.rs` | G1 all variants, G4, cold-cache run |

## Details

**1. Arena handoff.** Replace `out: Vec<Vec<Ev>>` (mod.rs:149) with one flat arena per person bucket per worker, reserving runs by `fetch_add` on a per-bucket cursor, then a prefix-sum pass so sessions sees contiguous `&mut [Ev]` per bucket. Sessions then drops its 256-iteration gather (mod.rs:177-185) and sorts arenas in place (`sessions.rs:187`). Pitfall: false sharing on cursors — pad to a cache line, or reserve 4 KB blocks. Nothing semantic changes: the record multiset and the `(person, ts)` sort isomorphism are identical.

**2. Clicks under dedup.** Today clicks start only after `rayon::join` returns (mod.rs:76-100) because `people.person(user)` is applied at parse (mod.rs:117). Keep that lookup but move the click `par_iter` into the dedup scope (`rayon::scope`, or `rayon::join(|| clicks(), || dedup())`). The tempting variant — scatter clicks by *user* hash during the event parse and remap later — is wrong: a click's user may be linked to another person, so it would land in the wrong bucket; remapping would add back a full re-partition pass. Note dedup is `(0..nb).into_par_iter()` with nb=256 tasks; merging clicks there needs the click loop as a second par_iter inside the same join, not nested par_iter.

**3. Streaming write.** `write_users` (mod.rs:204) first maps `UserRow`→`UserDaily` (~7M×48 B) then `output.rs` builds per-column `Vec<i32>/Vec<i64>`. Have `Acc::merge`-time output instead consume `acc.users` by chunks while the last sessions buckets are still running, or write per-fold-thread parts as soon as the fold finishes (fold tasks at mod.rs:173). Only constraint: `write_user_daily` currently takes `&[UserDaily]`; it needs an iterator or a streaming writer.

**4. Partition-then-parse.** Attractive because dedup's per-bucket table (60M/256 ≈ 234k entries × 64 B ≈ 15 MB) currently misses L2 heavily; 20 B entries would fit. But the second pass re-touches every 5.5 GB cache line to read `event_id`, and survivors (~97%) still parse fully, in file-random order — which converts the current sequential mmap read into random reads, exactly the cold-`/data` hazard in `PLAN.md` §9 ("Cold-disk read … could add 5–20 s"). Treat as a contingency only if track 03 shrinks parse work first.

## Rejected ideas
- **One pass: scatter events by person at parse.** Breaks E4/E5 (`rules.md:121`: later duplicate copies can name a different user; kept copy decides).
- **Super-chunk streaming through parse→dedup→sessionize.** A session can bridge chunk boundaries and a person's events span all files; merging partial sessions needs 30-min-gap boundary logic (`rules.md:54`) — high risk, no clear gain over idea 1.
- **Global concurrent dedup hash map with sharded locks.** Adds contention where the current per-bucket maps are lock-free.
- **Oversubscribing beyond 8 threads.** Rays already 8 on 4 cores; nothing to gain without measurements.
- **Removing the links∥events overlap.** 250 ms serial at 20M would sit on the critical path for no benefit.

## Dependencies / conflicts
- Overlaps track 05 (partition/scatter) on arena design and nb tuning; conflicts with track 04 if dedup entries shrink (idea 4 becomes moot) and with track 06 if sorting moves to radix (gather removal still helps).
- Track 03 (parse work avoidance) shrinks the dominant cost, raising the *relative* value of ideas 1–3 by Amdahl — but their absolute ms do not grow.
- Non-additivity: 1+2+3 individually sum to 300–800 ms, jointly 250–600 ms; they share the same idle slots, memory bandwidth and process tail.

## Sources
- `research/perf/00-profile.md` (phase medians, symbol sample: `split::<7>` 23.7%, `mi_*` ~4%, sort ~5%).
- `src/v2/mod.rs:76-100` (join), `:102-125` (clicks, person map at parse), `:129-167` (dedup, per-bucket `Vec` growth, gather), `:172-194` (sessions gather + `reduce_with`), `:204` (write_users). `src/v2/sessions.rs:186-188` (sorts).
- `rules.md:48-54` (dedup order, sessionize), `:72` (row order/file count free), `:121` (duplicate dirt; later copies may name a different user).
- `PLAN.md` §5a O1/O2/O8/O10/O12, §6 pipeline P0–P4, §7 G1/G4, §9 cold-disk risk and trap list.
- Leis et al., "Morsel-Driven Parallelism", SIGMOD 2014 — https://db.in.tum.de/~leis/papers/morsels.pdf (verified).
- Neumann, "Efficiently Compiling Efficient Query Plans for Modern Hardware", VLDB 2011 — https://www.vldb.org/pvldb/vol4/p539-neumann.pdf (verified).
- Rayon `join` (work stealing, idle-CPU execution) — https://docs.rs/rayon/latest/rayon/fn.join.html (verified).

Caveats: all 60M figures are 3× extrapolations, not measurements; no CI/PMU data exists for the phase boundaries; input ordering (whether a person's events cluster in file regions) is unverified — no generator is in the repo, so idea-4 locality risk is a hypothesis.
