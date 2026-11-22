# 04 memory-traffic (QA-corrected)

## Summary
At 60M the pipeline moves ≈14 GB of record traffic (≈230 B per event, from source, not counters), and 591 of 8,541 samples (6.92%) sit in `mi_page_free_list_extend` — 2.364 s of that run's 34.164 s sampled CPU; the wall time recoverable on the critical path is unknown. Fault counts cannot be converted into bytes (anon and file faults are mixed, THP state is unknown, and 403k faults × 4 KiB = 1.6 GB is *less* than the 1.9 GB of `ev_out` that dedup must touch), so the actionable lever is removing mimalloc's per-bucket allocation path with one exact-size arena, worth ≈150–300 ms and independent of huge pages, whose A/B is closed without a recorded win. Shrinking `Ev` 32→16 B is the largest traffic cut (≈0.96 GB live anon, ≈1.9 GB less gather) but is 6–8 h and collides with the sessions and dedup tracks.

## Current cost
Measured (`research/perf2/00-profile.md`, one 60M run, `NCPU=8 taskset -c 0-3,8-11`): open 8, links+events 3,425, clicks 572, dedup 2,561, sessions 1,319, write 354, internal 8,243, native start→exit 8,561 ms. CPU **40.867 s user + 12.389 s sys = 53.256 s over 8.243 s = 6.46 threads busy** (20M: 12.251 + 2.059 = 14.310 s over 2.142 s = 6.68). `sys` grows 6.0× from 20M to 60M while wall grows 3.85× — a measured superlinear kernel cost that this track owns; it is an **upper bound** on kernel memory work (faults, zeroing, mmap/munmap and Parquet writes are not separable from the counters available). `RssAnon` 7,243,812 KiB = **6.91 GiB** after dedup; `VmHWM` 12,895,600 KiB = **12.3 GiB** including the 5.4 GB mapping.

Minor faults (measured, with my reading of provenance): open 330, links+events 266,424, clicks 109,605 (**plus 107 major faults**), dedup 402,608, sessions 36,825, write 9,846; plus 3 major faults in write. Dedup and sessions touch no CSV, so those faults are **anonymous**; links+events and clicks are **mixed** (CSV file pages via mmap/fault-around plus `EvRaw`/`Ck` heap). Therefore: (a) the 826k total is not an anon number; (b) per-fault bytes are unknown — THP is `always` on the dev laptop, unverified on the scorer; (c) 402,608 × 4 KiB = 1.57 GB is *smaller* than the 1.9 GB `ev_out` that the phase must write, so some faults are served with more than 4 KiB or pages are recycled. The 107 major faults in clicks are a small but real cold-data tail inside the timed phases (order 10–100 ms; unquantified).

Traffic accounting from source (`src/v2/mod.rs`), estimates scaled to ~63.6M `EvRaw` copies (2–4% duplication, ~1.06 copies/event) and 15M clicks:

| Movement | Code | Bytes at 60M (est.) |
|---|---|---|
| P1 `EvRaw` 48 B → 4 workers × 256 buckets | `mod.rs:88-101`, write `:105` | 3.05 GB |
| P2 read-back of every bucket | `mod.rs:215-233` | 3.05 GB |
| P2 hash slots `(u128,EvRaw)` = 64 B + ctrl, ~8 maps live | `mod.rs:215` | ≈4.4 GB allocated over the phase; ≈139 MB live |
| P2 `Ev` 32 B survivors → 256×256 `Vec<Ev>` | `mod.rs:250` | 1.9 GB live anon |
| P3 gather: read + write | `mod.rs:271-281` | 3.8 GB |
| clicks `Ck` 48 B: scatter + gather | `mod.rs:189-206`, `:280` | 2.2 GB |
| **total** | | **≈14 GB (≈230 B/event)** |

Cross-check of the anon footprint from source, not faults: `ev_out` 1.9 GB + `ck_parts` 0.72 GB + maps ~0.14 GB + still-live `ev_parts` 3.05 GB ≈ 5.8 GB against a measured 6.91 GiB — consistent to within allocator slack, which is why the source caps are the trustworthy number.

DRAM is not binding: 14 GB at an assumed 10–15 GB/s for 4 Cascade Lake cores (inference, round-1 `05`) is 0.9–1.4 s of the 8.24 s wall. Allocation bookkeeping is: `mi_page_free_list_extend` 6.92% (2.364 s CPU; ≈0.59 s of that run's wall at 4.03 threads), plus unattributed libc 6.15%. Any CPU second saved is worth ≈0.155 s of wall at 6.46 threads busy.

Profile variance: links+events is 3,425 ms here vs 2,811 ms in `README.md` §1 — ±10%, so rank mechanisms, not 100-ms deltas.

## Ideas
| # | Idea | Gain at 60M (ms, reasoning) | Effort (h) | Correctness risk + trap | Files | How to verify | Supersedes / conflicts with Pass 2 card | Fits before Sun 09:00 freeze? |
|---|---|---|---|---|---|---|---|---|
| 1 | One exact-size bump arena per dedup task for `ev_out` (replaces 256 `Vec::with_capacity` per task set); `MADV_HUGEPAGE` behind an env flag, default off | 150–300. Attacks the measured 6.92% free-list/page-commit path rather than the fault count; the arena removes mimalloc's page bookkeeping for the largest single allocation (1.9 GB) and the 256 separate mappings. Independent of THP | 2–3 | low. Allocation only; no bucket, order or value change. Trap: must leak the mapping past the closure (teardown already skipped); THP-`never` host must be a no-op, not a regression | `src/v2/mod.rs` (P2 out), new `src/v2/arena.rs`; `memmap2` already allow-listed | G1 all configs incl. `SHOPT_ARENA=0`; G4 `MATCH` p20; G3 p20 median; `SHOPT_IO_STATS=1` dedup faults and `RssAnon` at 60M | **Replaces** large-page-first framing of round-1 4b/S3-8; layers on S4-1 (same one-buffer-per-task shape) | **Yes** (2–3 h) |
| 2 | Reuse one `HashMap` per rayon worker across dedup buckets (`clear` + `reserve`, capacity retained) | 40–100. 256 tables of ≈4.4 GB total slots are allocated and freed (`mod.rs:215`); recycling removes the alloc/free cycle, not the probes. Estimate, not measured | 1.5 | none. Dedup is order-insensitive (min `ingest_s`, `rules.md:51`); a cleared map is the same map. Trap: cap retained capacity at the largest bucket seen (whale ⇒ ≤ ~26 MB/worker) | `src/v2/mod.rs` (P2) | G1 all configs (incl. `SHOPT_DEDUP_BITS=1`) + G4; dedup phase time; `MIMALLOC_SHOW_STATS=1` | Neutral to S4-1/S4-3; void if 02-dedup-arch adopts a global winner table | **Yes** (1.5 h) |
| 3 | Shrink `Ev` 32 → 16 B via a per-task dense person index (`{idx:u32, ts:u32, price:u32, cat:u16, purchase:bool}`) | 80–160. 0.96 GB less live anon (60M × 16 B, source-exact) and 1.9 GB less gather traffic; bounded by DRAM time (1.9 GB ≈ 0.13–0.19 s, partly overlapped), not by faults | 6–8 | med. E4 (kept copy's user), E5 (invalid kept copy drops the event); whale persons; identity must resolve exactly at write | `types.rs`, `mod.rs` (P2 out), `sessions.rs` (`run_bucket`), `output.rs` | G1 sweep 2–30 + 41–43; G4 p20; unit test that persons differing only in the high `u64` never merge | Supersedes round-1 `05` #2; **conflicts** with S4-4 and 05-sessions-60m on `run_bucket`; prerequisite for a packed-`u64` radix key | **No** — 6–8 h plus conflicts exceeds the ≤ 9 h budget |

Non-additive: 1 and 3 touch the same 1.9 GB; 2 shrinks what 1 covers. Combined ≈ 200–350 ms.

## Details
**1. Bump arena.** `mod.rs:238-247` already counts survivors exactly, so one mapping of `4 × total` bytes per task suffices; fill it with a bump pointer and hand P3 `&[Ev]` slices by offset. Even with THP off this removes the free-list extension and 256 per-task table allocations that the profile attributes 6.92% of samples to. Huge pages are a separate, *negative-so-far* bet: the S3-8 A/B has **no commit and no recorded win** (`PLAN.md:204`), `MIMALLOC_ALLOW_LARGE_OS_PAGES` needs a hugetlbfs pool (`research/perf/08-io-memory.md`), and the kernel itself calls the fault-count effect "almost completely irrelevant" (`transhuge.rst`). So ship the arena with `SHOPT_HUGEPAGE=0` default and keep one A/B as an optional extra, not as the reason. Do not `MAP_POPULATE`.

**2. Map reuse.** `(u128, EvRaw)` is 64 B plus a control byte (`types.rs:21-33`: id 16, user 16, 8, 8, 2, 1; `u128` forces 16-B alignment). At 7/8 load, 60M distinct keys ≈ 68.6M slots ≈ 4.4 GB of allocation spread over 256 buckets, ≤ ~17 MB live per map, ≈139 MB across 8 workers. Recycling the map removes allocation churn but keeps every probe, so the gain is small and must be measured, not asserted. Report dedup phase time, not fault counts.

**3. `Ev` 16 B.** 27 B payload rounded to 32 by 16-B alignment; `(idx, ts)` becomes one `u64` (O9, `PLAN.md:178`). The dense index is free at `mod.rs:243` (`r.user = people.person(r.user)`), resolved back through a per-task `Vec<u128>` (≈0.6 MB) at write time. `run_bucket` merge-joins events and clicks by `u128` person (`sessions.rs:186-200`), so clicks need the same index space — that is the conflict, not a detail.

## Rejected ideas
- **Large pages as the primary lever.** A/B closed with no recorded win (`PLAN.md:204`); `MIMALLOC_ALLOW_LARGE_OS_PAGES` needs hugetlbfs; effect is host- and pressure-dependent (1.63× TLB-bound vs 1.02× under pressure, NSF measurement). Optional flag only.
- **Pre-faulting from "the idle thread" during the serial union-find.** No idle thread: `rayon::join` puts union-find on one worker and parse on the other seven (`mod.rs:158-172`), and `ev_out`'s size is unknown until parse returns.
- **Fault bytes → allocated bytes (`826k × 4 KiB`).** Invalid: anon/file are mixed, THP is unverified, and 403k × 4 KiB < the 1.9 GB dedup must touch.
- **`MAP_POPULATE`/`MADV_POPULATE_READ` on the 5.4 GB CSVs**: open is 8 ms warm; serialising I/O risks the cold-disk floor (round-1 `08`).
- **`MADV_DONTNEED` drop-behind on consumed CSV spans**: no warm gain; breaks fault amortisation (`scan.rs:42-69`).
- **Reusing `ev_parts` (3.05 GB) as the P3 gather target**: type-incompatible; drop already backgrounded (`mod.rs:262`).
- **Padding `EvRaw` to 64 B**: same line traffic, +33% footprint (round-1 `05`).
- **Shrinking `Ck` 48 → 32 B**: `person` and exact `click_id` are both required for the credit tie-break (`sessions.rs:181`).
- **`mlock`, `O_DIRECT`, file-backed THP**: no applicable gain (THP is anon/tmpfs only).

## Dependencies / conflicts
- Idea 1 should be built **on S4-1** (`PLAN.md:508-512`), which already produces one exact-size buffer per dedup task; doing both separately duplicates work. Extending the arena to clicks contradicts S4-1's "clicks keep their S3-5 buckets".
- Idea 2 is void if 02-dedup-arch selects a global atomic winner table.
- Idea 3 conflicts with S4-4 and 05-sessions-60m on `run_bucket`, and with 02-dedup-arch if a winner table hands out indices differently.
- The 6.15% unresolved libc share (memcpy vs fault stalls) is shared with track 02; do not double-count it.
- Gate is `RssAnon ≤ 10 GB` (`PLAN.md:348`); idea 3 lowers it by ≈0.96 GB, ideas 1–2 are neutral.

## Sources
- `research/perf2/00-profile.md` (phase/fault/RSS tables, 8,541 samples, CPU totals); `research/perf2/README.md` §1; `research/perf2/image` not applicable.
- `PLAN.md:204` (S3-8 no commit or record; 60M Pass-1 numbers), `:348-349`, `:508-512`, `:523-542`, `:178`.
- `src/v2/mod.rs:88-101,105,158-172,189-206,211-233,238-250,262,271-281,289-290`; `src/v2/types.rs:21-42`; `src/v2/sessions.rs:181-200`; `src/main.rs:27-33,128-140` (`RssAnon` vs `VmHWM` semantics).
- `research/perf/05-partition-scatter.md`, `research/perf/08-io-memory.md`, `research/perf/SYNTHESIS.md` §3; `research/perf2/01-roofline.md` (independently claims the same fault floor — treat both as estimates).
- THP is anon/tmpfs-only and `MADV_HUGEPAGE` is per-VMA: https://www.kernel.org/doc/html/latest/admin-guide/mm/transhuge.html ; https://man7.org/linux/man-pages/man2/madvise.2.html
- mimalloc v2.1.7 options (large OS pages need hugetlbfs): https://raw.githubusercontent.com/microsoft/mimalloc/v2.1.7/src/options.c
- `u128` 16-byte alignment: https://blog.rust-lang.org/2024/03/30/i128-layout-update/
- Balkesen et al., PVLDB 7(1) §5.1 (fan-out is TLB-bounded): https://www.vldb.org/pvldb/vol7/p85-balkesen.pdf

## Recommended card
**M-1 — One bump arena for the P2 output** · Scope: new `src/v2/arena.rs`, `src/v2/mod.rs` (P2 output, `mod.rs:238-250`); no `Cargo.toml` change · Depends: S4-1 merged · 2–3 h · est. 150–300 ms, peak anon unchanged, PTE count lower
- Replace the per-task `Vec<Vec<Ev>>` with one anonymous mapping per dedup task sized from the existing exact count (`total_valid × 32`, rounded to 2 MB), filled by a bump pointer; expose `&[Ev]` slices per person bucket via `offsets: Vec<u32>`, so P3's gather is unchanged.
- `SHOPT_ARENA=0|1` (default 1) selects the arena or today's `Vec::with_capacity`; `SHOPT_HUGEPAGE=0|1` (default 0) adds `madvise(MADV_HUGEPAGE)` and is A/B-only, since the S3-8 A/B has no recorded win.
- **Accept:** G1 in all configs (default, `SHOPT_CHUNK_BYTES=4096`, `SHOPT_DEDUP_BITS=1 SHOPT_PERSON_BITS=1 SHOPT_CHUNK_BYTES=100`, plus `SHOPT_ARENA=0`); G4 `MATCH` on `data/p20`; G3 p20 median no worse than +5%; one 60M run with `SHOPT_IO_STATS=1` reporting `dedup` phase time, dedup `minflt`, and peak `RssAnon ≤ 10 GB`, output byte-identical to the pre-change 60M run.
- **Traps:** bucket assignment and survivor order must not change; the arena must outlive the `rayon` closure (leak it deliberately — teardown is already skipped via `on_success`); keep 16-byte alignment so `Ev`'s layout is unchanged; a failed mapping or advice must fall back to `Vec`, never abort; do not `MAP_POPULATE`.
- **Performance traps:** THP off must be the *default* configuration so a negative host result cannot be confused with an arena regression; do not also change `Ev`'s size or the gather in this card, or the measurement loses attribution; report `RssAnon` and `VmHWM` separately (`src/main.rs:27-33`), never `VmHWM` against the 10 GB gate.
