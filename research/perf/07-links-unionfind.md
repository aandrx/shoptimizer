# 07 links-unionfind

> **Orchestrator validation note (supersedes conflicting claims below):** With 4M keys, 2 MiB and three hashes, the usual Bloom approximation is (1−exp(−3×4M/16,777,216))³ ≈ 13%, not 1.5%. The filter cannot fit a 1 MiB per-core L2. Gains and overlap must be re-estimated against measured miss rate and a four-core SMT critical path.

## Summary
The best move is to compact and pre-size the union-find so the whole links build is one cache-sequential pass: one structure instead of `index`+`ids`+`parent`, a correctly sized `names` table, and a single finalize loop. Expected 60M wall gain is only ~40–100 ms today, because the build is already hidden under the event parse (`src/v2/mod.rs:76-100`), but it is cheap insurance against the parse-speedup tracks, where the same edit is worth 0.3–0.8 s. The larger, unmeasured exposure is not the union-find but the ~70M `PersonMap::person` probes in clicks and dedup, which grow superlinearly once the name table leaves L3.

## Current cost
Track card: ~250 ms serial at 20M, "~0.75 s at 60M" (3×); the phase containing it is 1,100 ms at 20M (`PLAN.md` §6, `research/perf/README.md` §1). Decomposing the 250 ms from the code at `src/v2/people.rs:33-80`: ~750k link pairs at 20M (≈1.04 links/person × ~670k people, from `rules.md:106`), 1.5M `HashMap<u128,u32>` probes into a 1.35M-entry table (the `hint` is pair count, not ID count, so it rehashes at least once), then two full passes over all IDs doing random `find` walks plus ~670k inserts into `HashMap::new()` — an unpresized table that regrows from zero (`src/v2/people.rs:76`). Projected at 60M: ~4.1M distinct IDs, ~2M name entries, ~74 MB table at hashbrown's ~37 B/entry (16+16 payload at 7/8 load, per hashbrown's SwissTable design), so 0.75 s is optimistic; 1.2–2 s is plausible.

The read side is bigger and unattributed: `person()` is called once per valid surviving event (`src/v2/mod.rs:154`) and once per click (`src/v2/mod.rs:111`), i.e. ~23M at 20M and ~70M at 60M probes into a random-keyed 16–74 MB table. `00-profile.md` puts the dedup closure (`closure#5`) at 22.21% of samples (~0.63 s of 2.82 s); the probe cost is inside that number and cannot be separated from the profile.

**Conflict to resolve before spending hours:** at 250/2816 ms the links build should be ~8.9% of samples, ~257 of 2,900, yet `00-profile.md`'s top-40 contains no union-find, `HashSet`/`HashMap<u128,u32>` or `people::build` frame above 0.10%. Either the 250 ms is wall time *waiting in the join* rather than CPU, or monomorphized frames were resolved elsewhere, or the number is stale. Add a `[phase]` timer around `people::build` plus a `find`/interner counter before trusting either figure.

Amdahl for the overlap: if event parse alone is P wall with 8 threads (W = 8P thread-ms) and links is serial L, the phase takes T = (W + L)/8 while links finishes at L (hidden iff L < T). 20M: W≈7200, L=250 → T=931, penalty vs 900 ≈ **31 ms (0.1% of 2.96 s)**. 60M: W≈21600, L=750 → T=2794, penalty ≈ **94 ms**; L=1500 → 188 ms, still hidden. The cliff appears only when parse gets faster: with W=5400 (4× parse speedup) and L=1500, T=863 but the join waits for L=1500 → **+640 ms**. Non-additivity: link savings do not sum with parse savings; they only matter once L approaches T.

## Ideas
| # | Idea | Expected gain at 60M (ms, with reasoning) | Effort (h) | Correctness risk | Files touched | How to verify |
|---|---|---|---|---|---|---|
| 1 | Compact single-structure union-find: `HashTable`/`RawTable` slot or entry index as the dense ID (no `ids` Vec), `reserve(2×pairs)`, `names` with capacity, one finalize pass, union by rank | 40–100 now (removes ~40% of L=750–2000 ms serial → /8); 300–800 if the parse branch drops below L | 2–3 | low — root choice changes, name = min over component is unchanged; trap "upper-cased IDs sort first and can become a person's name" | `src/v2/people.rs` | `cargo test v2::people` (400-link chain, randomized vs V1), G1 (`cargo test`, fixtures, `diff-seeds 2-30`, 2M seeds 41–43), G4 `MATCH` on p20 |
| 2 | Measure the read-side probe, then a 2 MB L2-resident negative filter (Bloom, ~1.5% FP) fronting `PersonMap::person` | 0–500 (hypothesis; ~70M probes × saving of 20–50 ns × 20–50% rejection) | 1 measure + 3 impl | low — negatives only, false positives fall through to the map | `src/v2/people.rs`, `src/v2/mod.rs` | same gates; plus a unit test that filter-miss ⇒ absent |
| 3 | Software-prefetch the person probe K records ahead in the dedup survivor loop and the click parse | 100–500 (hypothesis; hides part of a DRAM-latency random probe, ~70M probes) | 1–2 | none — pure scheduling hint; trap "E4: kept copy's user counts" unaffected | `src/v2/mod.rs:111,154` | G1 + G4; byte-identical output |
| 4 | Fully parallel build: parallel hash-partition interning + sharded atomic union (or parallel component labeling), 256 shards of ~16k IDs | ≤75 now (bounded by idea 1's ceiling: the work is hidden), up to 800 in the cliff scenario | 8–10 | low — result depends only on connected components | `src/v2/people.rs` | as 1, plus a fuzz test comparing against the serial builder on random link sets |

## Details
**1. Compact UF.** Today the interner is `HashMap<u128,u32>` with a parallel `ids: Vec<u128>` and `parent: Vec<u32>` (`src/v2/people.rs:52-54`), and finalization walks all IDs twice (`:70-80`), the second time inserting into a zero-capacity map. Replace with one `hashbrown::HashTable` whose insert returns an index into `parent` (rank/size in a side `Vec<u8>`); `parent[ra]=rb` at `:68` becomes union-by-rank, which is safe because the name is recomputed as the component minimum, not the root. Finalize in slot order (sequential memory) computing `min` per root and writing `names` with `with_capacity(ids)`. Saves one full random pass and all rehashes; preserves D6 order semantics.

**2. Read-side filter.** Clone-free `person()` is a single SwissTable probe (`src/v2/people.rs:15-18`), so per-call savings must come from *not probing*. A 2^24-bit filter (2 MB, ~1.5% FP at 4M keys × 3 hashes) rejects IDs absent from `names` — i.e. name IDs and single-ID people. Bound it by measurement first: temporarily return `user` and diff `[phase] dedup` / `clicks`. If the delta is <150 ms at 20M, drop the idea.

**3. Prefetch.** In `for r in kept.values()` the next record's `r.user` is already in cache, but the map slot is not; `mix(user)` + `prefetch` 4–8 records ahead converts serialized misses into overlapped ones. Same in the click fold. Watch the `id::mix` cost doubling.

**4. Parallel build.** Because `rules.md:43-44` defines the person as the transitive closure with min-ID naming, decomposition is semantically free: partition IDs by `id::bucket`, sort+dedup per shard in parallel for dense indices, then edge union in parallel with atomic parent CAS (contention is negligible at ~1 edge/ID). Effort is the problem: it buys nothing until links is on the critical path.

## Rejected ideas
- **8-byte keyed map (low `u64` of the packed ID) to halve entries**: a suffix collision would merge two people and break the "a link dirt upper-case side can split a person" trap; the collision-detect fallback is high risk for ≤0.3 s.
- **`(u64 tag, u32 index) + names Vec`**: two random loads per hit instead of one.
- **Sorted array + binary search for `person()`**: log n cache misses per call versus ~1.
- **Replacing the map with a per-bucket resolve in sessions**: needs an extra 1.9 GB pass over `Ev`.
- **`rayon::scope` with links split into parse/finalize sub-tasks**: parse is already parallel; the remaining serial parts are inherently ordered.

## Dependencies / conflicts
- Idea 1 and 4 depend on nothing; idea 4 is wasted work unless track 01/02/03 cut parse below L.
- Idea 3 conflicts with the `dedup` track (04) and `microarch` (10), which own the same loop; coordinate before implementing.
- Idea 2 interacts with `partition-scatter` (05): any record-size or ordering change shifts the map's cache behaviour.
- The links branch runs on the `rayon::join` calling thread (`src/v2/mod.rs:76`), so it competes with 7 parse threads on 4 physical cores; all bounds assume that.

## Sources
- `src/v2/people.rs:15-18` (`person`), `:23-31` (`find`, path halving), `:33-50` (parallel parse), `:52-54` (interner + `ids` + `parent`), `:60-69` (union, no rank), `:70-80` (two finalize passes, unpresized `names`).
- `src/v2/mod.rs:70-100` (`rayon::join` links/events), `:111` (click lookup), `:154` (dedup lookup), `:147-149` (VALID gate before lookup).
- `src/id.rs:29-40` (`mix`/`bucket`), `rules.md:40-46` (Linked IDs, min-ID naming), `rules.md:106-108` (ID/chain/shared-device distribution), `rules.md:127` (link dirt), `PLAN.md` §7 gates, §9 trap list, §6 record sizes.
- `research/perf/00-profile.md` (phase medians, 22.21% `closure#5`, no union-find symbol above 0.10% — unresolved conflict), `research/perf/README.md` §1 track card.
- hashbrown 0.17 `HashMap` docs — default hasher `foldhash`, "quadratic probing and SIMD lookup", 7/8 load factor: https://docs.rs/hashbrown/0.17.0/hashbrown/struct.HashMap.html
- hashbrown repository (SwissTable port): https://github.com/rust-lang/hashbrown
- Disjoint-set data structure, union by rank + path compression, O(α(n)): https://en.wikipedia.org/wiki/Disjoint-set_data_structure
