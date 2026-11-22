# 04 dedup

> **Orchestrator validation note (supersedes conflicting claims below):** Hash truncation alone is not exact. A compact index must represent all colliding full IDs and compare them, including on insertion; verify forced collisions and measure actual slot size/load factor.

## Summary
Best move: replace the per-bucket `HashMap<u128, EvRaw>` with a compact slot array (`u64` hash + `u32` payload index) over a dense `EvRaw` payload, DuckDB-two-part-table style, while raising the dedup partition count so each probe working set fits L2. Expected 60M wall gain 0.7–1.1 s of ~8.9 s local (≈8–12%), risk low, effort 6–9 h. The sort-based alternative may beat both but its bound is 0–1.5 s and I have no measurement to justify the effort.

## Current cost
`dedup` is 744–811 ms at 20M (`00-profile.md` runs 1–3; §1 table says ~650). It is ~22% of user CPU — `run_v2::{closure#5}` (the per-bucket map body, `FnMut<(usize,)>`) is 22.21% flat, and the dedup phase is 650–810 of 2,962 ms wall. Projecting 3× at 60M gives **1.95–2.45 s local, ~2.2–2.8 s on the scorer (×1.13)**; hash tables degrade slightly worse than 3× because the table leaves cache, so the upper end is the better central estimate. Dedup does not overlap another phase (it sits between the `clicks` phase and `sessions` in `src/v2/mod.rs:127-167`), so a millisecond saved there is a millisecond of wall — the only non-additivity is shared-cache/bandwidth effects on the neighbouring phases.

Footprint at 60M: `ev_parts` 60M × 48 B ≈ 2.9 GB (`types.rs:18-33`), plus the map at `mod.rs:133` holding 16 B key + 48 B value per entry at 7/8 load ≈ 4.4 GB. Per bucket at `SHOPT_BUCKET_BITS=8` (`mod.rs:66`) that is **~234K entries ≈ 17 MB** — 17× the 1 MiB L2/core of Cascade Lake (WikiChip), so D14's "each bucket's table fits in L2" (`PLAN.md:106`) is false today at 20M too (78K entries ≈ 5.7 MB). That is the whole cost model: ~26M inserts/s aggregate over 8 threads at 20M, i.e. ~130 ns/insert/core, which is latency, not hashing — the hasher is foldhash (`Cargo.lock:219-228`, hashbrown 0.17 default-hasher), and only 2–4% of copies are duplicates (`rules.md:121`), so virtually every probe is a fresh random write.

## Ideas
| # | Idea | Expected gain at 60M (ms) | Effort (h) | Correctness risk | Files touched | How to verify |
|---|---|---|---|---|---|---|
| 1 | Compact slot array: `HashMap<u64 hash, u32 idx>` (or hand-rolled open addressing) over dense `Vec<EvRaw>`, full-id verify on hit | 500–750 (dedup 1,950→1,200–1,450). Probe stride 65→12 B, payload appended sequentially; ~3.4 GB RSS saved | 6–9 | **Low** — trap: 64-bit hash collision + "kept copy's user counts" (E4) | `src/v2/mod.rs`, `src/v2/types.rs` | G1 + G4 + new unit test with a deliberately colliding hash; see below |
| 2 | Bucket sizing for L2: raise dedup bits to 10–12 (separate `SHOPT_DEDUP_BITS` from person bits) | 200–450 alone; with idea 1, 0.7–1.0 s combined | 1 (env A/B) / 4 (split bits) | **Low** — trap: none semantic; risk is a sessions regression | `src/v2/mod.rs` | G1 at `SHOPT_BUCKET_BITS=1` and default; G3 on p20 |
| 3 | Fuse dedup into the parse fold: per-worker per-bucket maps, merged in the rayon reduce | 150–400 (removes the 2.9 GB write + reread, not the insert work) | 10–16 | **Med** — merge order vs "min ingest, ties identical" | `src/v2/mod.rs` | G1 seeds 2–30; RSS check at 60M |
| 4 | Sort-based dedup: radix sort per bucket on the 64-bit hash, group, compare full ids, keep min ingest | 0–1,500 (central guess 400–900) | 12–20 | **Med** — equal-hash grouping, tie handling | `src/v2/mod.rs` + a sort/radix module | G1 + G4 + collision unit test |

Ranked by gain per hour: 2, then 1, then 3, then 4.

## Details

**1. Compact slot array.** Today the entry insert at `mod.rs:134-148` writes 48 B into a probed slot; the value is the record itself. Split it: append the `EvRaw` to a per-bucket dense `Vec`, and keep `HashMap<u64, u32>` where the key is `id::mix(r.id)` (already computed for bucketing, `src/id.rs:42-57`) and the value is the payload index. On a hit, compare `payload[idx].id == r.id` and keep the smaller `ingest_s`, rewriting the payload in place. This is DuckDB's two-part aggregate hash table: a separately allocated pointer array over payload blocks, so probes touch only the slot array (their blog: linear probing favoured because entries are walked linearly and "the next entry will very likely be in the CPU cache"). Per-bucket probe set at 60M, B=8: 234K × 12 B / 0.875 ≈ 3.2 MB (5× smaller); with B=10, ~0.8 MB. This preserves the argument that dedup output order is irrelevant: `sessions.rs:187` sorts by `(person, ts)` and `rules.md:55` makes equal-`ts` order irrelevant, so a different iteration order is safe — but the *value* must still be the winning copy's own `user`, `flags` and fields (E4: a later copy may name another user; "the kept copy's user counts", `PLAN.md:425-426`).
*Collision trap:* with 64M distinct keys, a 64-bit hash gives an expected ~0.11 true collisions (birthday), so "ignore it" is not a correctness argument. Either probe past hash matches until the payload id matches (open addressing with the full id as the real key), or push the collision into a tiny auxiliary map — it will hold ~0 keys at 60M.

**2. Bucket sizing.** `mod.rs:66` uses one `bits` for both the event-id dedup partition and the person partition. Splitting them (dedup 10–12, person 8) makes each table 4.3 MB / 1.1 MB instead of 17 MB at 60M. Two costs: 4,096 tiny person buckets make `run_bucket` per-bucket overhead visible (`sessions.rs:186-188`) and add rayon task overhead, and `ev_out`/`ck_parts` become 4,096 `Vec`s of length ~15K — the sort track (06) must re-measure. Non-additive with idea 1: they attack the same misses, so sum ≈ 60–70% of the parts. This is also the direct fix for the D14 claim being wrong (`PLAN.md:106`).

**3. Fusion.** Fold directly into `Vec<HashMap<u64, EvRaw>>` per worker instead of `Vec<Vec<EvRaw>>` (`mod.rs:81-103`), then merge maps in the reduce. It removes the 48 B scatter and the re-read, but each merge is another insert pass, so the bound is memory traffic only: 2×2.9 GB at ~10 GB/s ≈ 0.3–0.6 s upper bound, realistically 150–400 ms after paying for merge. Memory risk: 8 workers × 256 pre-sized maps.

**4. Sort-based.** Per bucket, radix-sort `(hash64, idx)` and keep the first (min-ingest, full-id compare) of each run. Working set 3–11 MB is L3-resident on the scorer's 4-core slice (~5.5 MiB L3), which is where the 130 ns/insert can plausibly be beaten. But Balkesen et al. found radix-hash generally beats sort for main-memory multi-core joins (VLDB 7:85), and DuckDB's own aggregation post argues O(n) inserts over O(n log n) sorting even with a fast radix sort. Keep as the A/B after 1+2 if hours remain.

## Rejected ideas
- *Person-lookup bitset filter in the survivor loop (`mod.rs:150-155`)*: maybe 100–250 ms, but it belongs to track 12 and is unmeasured here.
- *Defer parsing `user`/`price` for duplicate copies*: `PLAN.md:184` already rules it out (random re-reads).
- *Bloom filter instead of a table*: cannot decide min-ingest, which requires state per duplicated id.
- *Flush-and-sort by `event_id` globally with radix*: supersedes 1+2, subsumed by idea 4.

## Dependencies / conflicts
Needs nothing from other tracks. Idea 2 changes `nb`, so it collides with 06 (sort/sessionize) and 05 (partition scatter) measurements — land 2 first only if 06 re-measures. Idea 1 changes RSS (−3.4 GB), which helps 08 (io-memory) and T15's ≤ 8 GB target (`PLAN.md:346`). Idea 3 touches the parse fold, i.e. 05's territory.

## Sources
Repo: `src/v2/mod.rs:66,76-103,127-167`; `src/v2/types.rs:18-33`; `src/v2/sessions.rs:186-188`; `src/id.rs:42-57`; `rules.md:51-53,55,121`; `PLAN.md:99-107,173-178,229-233,345-346,357-360,425-431`; `00-profile.md` (phase table, `closure#5` 22.21%); `Cargo.lock:219-228`.
Web: https://docs.rs/hashbrown/0.17.0/hashbrown/ (SwissTable port; `DefaultHashBuilder` page — foldhash default) · https://en.wikichip.org/wiki/intel/microarchitectures/cascade_lake (L2 1 MiB/core, L3 1.375 MiB/core) · https://duckdb.org/2022/03/07/aggregate-hashtable (two-part table, linear probing) · https://www.vldb.org/pvldb/vol7/p85-balkesen.pdf (sort vs radix-hash) · https://abseil.io/blog/20180927-swisstables.
Gates for every idea: G1 (`cargo test`, `make fixtures`, `make check-dev`, `diff-seeds SEEDS=2-30`, then seeds 41–43 at 2M), run twice at default and `SHOPT_CHUNK_BYTES=4096`, plus `SHOPT_BUCKET_BITS=1`; G4 `make oracle` + `MATCH` on p20; G3 for ≥3% (`PLAN.md:357-360,337`). New tests: same `event_id` with equal `ingest_ts` in both input orders; smaller-ingest copy invalid (E5); hash-collision path; 60M RSS check.
