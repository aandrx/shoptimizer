# 05 sessions-60m

## Summary
Sessions costs **1,319 ms at 60M** (measured), of which sort is ~8.3% of all CPU samples (≈0.55 s wall) and the gather ≈0.25–0.4 s (estimate). The best move is not S4-4: raising **person** bucket bits to 11–12 once S4-2 separates the knobs (with S4-1's counted scatter) makes each bucket L2-resident for ~1–2 h of work. Packed-key radix and a dense person index both need a per-bucket rank pass that costs more than it saves; per-bucket Parquet overlap is real but too small for the freeze budget.

## Current cost
Measured (`00-profile.md` phase table): sessions 392 ms / 1,319 ms at 20M / 60M, 3.36×, 36.8k minor faults; whole run 53.3 CPU-s at 60M (40.9 user + 12.4 sys).

Symbol attribution at 60M (`00-profile.md` top-40, CPU samples, not wall): `quicksort<Ev>` 452 (5.29%), `small_sort_general<Ev>` 93 (1.09%), `median3_rec<Ev>` 15 (0.18%), `quicksort<Ck>` 112 (1.31%), `small_sort_general<Ck>` 32 (0.37%), `median3_rec<Ck>` 3 (0.04%) → **sort ≈ 8.3% of CPU**; `libc` 525 (6.15%, unresolved, plausibly `memcpy` in the P3 gather and sort moves). The fold closure that contains the guarantee-copy, `run_bucket` and `Acc::person` is 3.41%; `Acc::merge` 0.12%.

Bounded estimate of the 1.32 s (8 threads): sort **0.55–0.75 s** (8.3% × 53.3 CPU-s ≈ 4.4 CPU-s, plus part of `libc`); gather **0.25–0.40 s** — 58M `Ev`×32 B = 1.86 GB plus 15M `Ck`×48 B = 0.72 GB copied once from 8 worker parts per bucket (`mod.rs:274-281`); walk/credit/assist/day ~0.2–0.3 s; merge + reduce ~0.02–0.05 s.

Why 7.2 MB sorts behave badly: at `SHOPT_BUCKET_BITS=8` (`mod.rs:158`) 58M survivors / 256 buckets = 226k `Ev` = 7.2 MB, ~10× Cascade Lake's 1 MB L2. The whale bucket is worse (5 people with 1–2% each, `rules.md` "The generator"), but it is one serial person and cannot be split; S3-6 already starts it first (`mod.rs:69`). No person lookups happen in P3 — the `people.person` probes are in P1/P2 (`mod.rs:206,225`); inside P3 the only lookups are `cat_sum`/`camp` array indexing (`sessions.rs:154-172`). Sort answer to the card's first question: sort ≈ 45–55% of the phase, gather ≈ 20–30%, walk ≈ 15–25%.

## Ideas
| # | Idea | Gain at 60M (ms, reasoning) | Effort (h) | Correctness risk + trap | Files | How to verify | Supersedes / conflicts with Pass 2 card | Fits before Sun 09:00 freeze? |
|---|---|---|---|---|---|---|---|---|
| 1 | After S4-2, set `SHOPT_PERSON_BITS` = 11–12 (A/B), keeping dedup bits at 8–10: buckets become 0.45–0.9 MB, so sort/gather/walk are L2-resident | 200–400. Sort is ~0.55–0.75 s; an L2-resident quicksort over 14k–28k records typically removes 30–50% of moves+misses → 170–330 ms; gather chunks shrink from 7.2 MB to 0.45–0.9 MB (+0–30). Offset by +50–150 risk in `clicks` (32k write streams at 4096 buckets vs 2k) → net 150–350 | 1–2 on top of S4-1/S4-2 (0.5 if S4-2 already landed) | Low–med: bits only change partitioning; the same `id::bucket(person, bits)` must be used for events *and* clicks (`mod.rs:158,204,225`). Trap: A4/A5 credit tie if bits diverge; E16 equal-ts unchanged. Memory: per-worker `Vec<Vec<Ck>>` slack stays ≈1.15× actual (`prealloc_cap`, `mod.rs:48-52`), ≈0.8 GB total | `src/v2/mod.rs:158,267-281`, `PLAN.md` D14 | G1 all configs incl. `SHOPT_PERSON_BITS=1`, G4 MATCH; 60M run logging `sessions` and `clicks` phases at bits 10/11/12 | **Supersedes S4-4** for the normal case (no sub-partition pass needed); depends on S4-1 (counted person scatter) so P2 output is contiguous per bucket; conflicts with dedup-arch if it changes P2's output shape | **Yes** |
| 2 | S4-4 reworked: fuse the gather with a person-homogeneous sub-run scatter (k = 8–32, same hash bits disjoint from bucket bits), then sort/walk each ≤0.5 MB sub-run — one pass, not gather-then-partition | 100–250. Saves 30–50% of the sort (same as #1) but pays a second full 2.6 GB scatter (0.2–0.3 s) that #1 avoids; only worth it if person bits must stay ≤10 | 4–6 | Med: a person must stay inside one sub-run or his sessions double-count (A5); sub-runs must be walked separately, else `while evs[j].person == person` (`sessions.rs:191-193`) truncates | `src/v2/sessions.rs:186-206`, `src/v2/mod.rs:267-281` | G1 all configs + G4; unit test person with events in a bucket and clicks only in the same sub-run | Replaces S4-4; conflicts with #1 (do one, not both) | No (only as fallback if #1 regresses) |
| 3 | Per-bucket `user_daily` encode/write overlapped with the sessions tail (writer thread + channel, rows appended per bucket) | 40–120. Sessions is CPU-saturated except the whale tail, so only the tail and the 354 ms `write` phase can overlap; `rules.md` makes row order free | 4–6 | Med: writer lifetime/`close()` and zero-row parts (`src/output.rs` schema test); no duplicate `(user_id,day)` | `src/v2/mod.rs:267-320`, `src/output.rs` | G1 + G4; assert each person-day appears once; 60M wall delta | Conflicts with parquet-output #4 (same idea) and with S4-1's contiguous-bucket output | No |
| 4 | Cheap sort hygiene: `sort_unstable_by` comparator instead of `_by_key` tuples at `sessions.rs:187-188`; day-major `camp[day][campaign]` cells so assists stop striding `sessions.rs:171` | 20–60. Key-function tuples cost a 24-byte build per comparison; assist writes touch an 896 KB strided array ~once per active campaign per session | 1–2 | Low: pure refactor; assists must still count once per campaign per session, never organic | `src/v2/sessions.rs:95-206` | G1 + G4 | None | Yes, if idle time remains |

## Details
**1 — bits, not sub-partition.** `run_bucket` (`sessions.rs:186-188`) sorts two slices whose size is set solely by `nb`. At 2048/4096 buckets a bucket holds 14k–28k `Ev` (0.45–0.9 MB) and ~1–2k `Ck`, comfortably L2; no algorithm change, no tie semantics touched. The blocker today is that one `bits` drives dedup, person, *and* click scatter (`mod.rs:158`), so 4096 buckets blew up the scatter into 4096×4096 `Vec`s (round-1 `SHOPT_BUCKET_BITS=12` regression, `SYNTHESIS.md` §2 "Bucket count" row). S4-2 removes that coupling; S4-1 (`PLAN.md:508-512`) makes P2 output one contiguous `Vec` + `offsets` per task, so 4096 offsets cost 16 KB/task instead of 4096 `Vec`s. Then only the click scatter still has 32k streams: measure `clicks` alongside `sessions`, and pick 11 if clicks regresses. Expect the biggest single-bucket win on the whale bucket's 37 MB (a person's events still cannot be split, so its floor is unchanged; S3-6 protects the tail).

**2 — why not radix.** A packed `u64 (rank, ts)` key needs a dense per-bucket rank, i.e. a hash map probe per event and per click (~58M + 15M probes ≈ 0.06–0.15 s wall) *plus* `rank→u128` tables, and the sort is already L2-resident after #1, where the deep-research 3.7–4.5× advantage of packed-`u64` radix (`SYNTHESIS.md` §2) is mostly cache, not passes.

**3 — overlap.** `write_users` splits the finished `acc.users` into 8 `par_chunks` (`mod.rs:313`); per-bucket encoding would need rows appended to open row groups, and `SerializedFileWriter` is single-threaded per file, so the benefit is bounded by idle cores at the tail.

## Rejected ideas
- Packed-`u64` LSD radix after a per-bucket rank: probe + rank tables cost ≈ the saved comparisons at L2 sizes.
- Dense person index in `Ev`/`Ck`: no global dense id exists (persons come from a union-find `names` map, `people.rs:14-24`); a per-bucket rank is assigned *after* the gather, so the 1.86 GB gather traffic and P2 stores are unchanged.
- Drop `click_id` from the `Ck` sort key (sort by `(person,ts)`, min-scan ties): saves 16 B of 48 B on 0.72 GB, needs per-equal-ts tie resolution; round-1 rejected, still not worth 3 h.
- Split a whale person across threads / sub-runs: sessions are defined per person and ordered by ts (`rules.md` Events 5–6).
- Per-person `Vec<Ev>` during dedup: ~2M allocations, random pushes.

## Dependencies / conflicts
S4-1 (contiguous, counted person scatter) is a hard prerequisite for #1's cheap high-bit scatter; S4-2 for the knob. dedup-arch's global-winner redesign would change `ev_out`'s shape — #1's `bucket_lens`/offsets must be re-derived, but the sessions side is unchanged. If memory-traffic shrinks `Ev` to 16 B, re-run the bits A/B (a 4096 bucket becomes 0.22 MB; 2048 may then win). parquet-output #4 and critical-path overlap #3 exactly — do not build twice. All ideas keep V1 untouched (D11) and need G1 in every config plus G4 MATCH (`PLAN.md:566,569,571`).

## Sources
- `research/perf2/00-profile.md` phase table and top-40 samples (measured 60M).
- `src/v2/sessions.rs:95-206` (walk, credit `partition_point:122`, `day_of:128`, camp stride `:171`, sorts `:187-188`); `src/v2/mod.rs:48-52,158,267-281,313`; `src/v2/types.rs:28-52` (record sizes); `src/id.rs:8-48` (mix/bucket); `src/v2/people.rs:14-24`.
- `rules.md` Events 5–7, Clicks 4–7, Outputs; "The generator" (whales, E/30 people).
- `PLAN.md:202,204` (measured phases, bucket-bits A/B), `:483,491-542` (Pass 2 deps, S4-1/S4-2/S4-4), §7 gates.
- `research/perf/SYNTHESIS.md` §1, §2 ("Sessions sort", "Bucket count" rows), §4 rows 5/7; `research/perf/06-sort-sessionize.md`, `09-parquet-output.md`.
- Parquet-rs writer being one file per `SerializedFileWriter` — `src/output.rs:100-126`.

## Recommended card
**S4-2b — L2-sized person buckets** · Scope: `src/v2/mod.rs` (defaults/tests only), `PLAN.md` D14 · Depends: S4-1, S4-2 merged · 1 h plus one 60M run.
- Change: keep `SHOPT_DEDUP_BITS` at 8–10; set `SHOPT_PERSON_BITS` default after A/B of 10/11/12 at 60M (expect 11 or 12). No `sessions.rs` change.
- Accept: G1 in all configs including `SHOPT_DEDUP_BITS=1 SHOPT_PERSON_BITS=1 SHOPT_CHUNK_BYTES=100`; G4 `MATCH`; 60M: `sessions` ≥ 100 ms faster than the S4-1/S4-2 baseline at the same conditions, `clicks` regression < 50 ms, peak `RssAnon` ≤ 10 GB.
- Traps: the same person bits must drive the event re-partition, the click scatter and `run_bucket`; a bit mismatch silently breaks the merge-join by person (A5). Do not touch equal-ts (`E16`), 30-minute boundaries, or the credit tie. Publish the A/B table, and revert the default if the net is under 100 ms — a documented negative result is worth more than a guessed knob.
