# 05 partition-scatter

## Summary
Both scatters are already structurally sound (per-worker bucket buffers, no locks, cache-resident stream heads); the cheapest real win is pre-sizing and consolidating the second scatter's 65,536 tiny `Vec`s (~40–150 ms at 60M), and the largest is shrinking `Ev` from 32 B to 16 B via a bucket-local `u32` person index (~150–300 ms), which is jointly owned with tracks 06/12. The main risk is that most of this traffic is unavoidable: `EvRaw` is already at its 16-byte-alignment floor, so total expectable saving across all four ideas is ≤ 350 ms, i.e. ~4% of the ~8.9 s local 60M run.

## Current cost
Per-60M traffic, from `src/v2/mod.rs` and the §1 table (measured phase times; byte volumes inferred from `rules.md:105,121-122` rates — duplicates 2–4% with 1–3 extra copies ⇒ ~1.06 copies/event ⇒ 63.6M `EvRaw`; clicks assumed at the practice ratio 0.25/event ⇒ ~15M):

| Movement | Code | Bytes at 60M |
|---|---|---|
| P1 write `EvRaw` into 4×256 bucket `Vec`s | `mod.rs:104-118` | 63.6M × 48 = 3.05 GB |
| P2 read-back per bucket | `mod.rs:132-141` | 3.05 GB (bucket = 11.9 MB ≫ L3 slice) |
| P2 re-scatter `Ev` into 256×256 `Vec`s | `mod.rs:152-161` | 59M × 32 = 1.88 GB |
| P3 gather copy | `mod.rs:170-186` | 1.88 GB read + 1.88 GB write |
| clicks scatter + gather | `mod.rs:123-133`, `181-186` | 0.72 + 0.72 + 0.72 GB |
| **total** | | **≈13.9 GB** |

At an assumed 10–15 GB/s for 4 physical Cascade Lake cores in a shared n2 VM (vs ~25–30 GB/s on the Zen 4 laptop) that is 0.9–1.4 s of the ~10 s scorer run — inference, not a measurement. Cross-check against measured phases: dedup is 650–810 ms at 20M (`00-profile.md`), of which the 1.0 GB read-back is a minor share; `parse::event`'s push sits inside `closure#5` (22.2% of samples) next to `split::<7>` (23.7%). So the scatter share is plausibly 400–700 ms at 60M, not seconds.

Two structural facts the card asks about. (1) Record width: `EvRaw` is field-tight (16+16+4+4+4+2+1 = 47 → 48 B). `Ck` is **48 B, not the ≤40 B in `PLAN.md:246`**, because `align_of::<u128>() == 16` since Rust 1.77/1.78 (`src/v2/types.rs:39-45`; toolchain here is 1.97.1). (2) `bucket()` costs a 2-multiply mix plus a 16-byte load per record (`src/id.rs:40-60`) — <30 ms at 60M, not a lever.

## Ideas
| # | Idea | Expected gain at 60M (ms) | Effort (h) | Correctness risk (trap) | Files touched | How to verify |
|---|---|---|---|---|---|---|
| 1 | Pre-size every scatter buffer; replace `Vec<Vec<Ev>>` per dedup task with one exact-sized buffer + 1-byte person-bucket tag | 40–150 | 3 | none (no output change) | `src/v2/mod.rs` | G1 incl. `SHOPT_BUCKET_BITS=1`/`CHUNK_BYTES=4096`, G4 `MATCH` |
| 2 | Shrink `Ev` 32 → 16 B using a bucket-local `u32` person index (fused with track 06's u64 sort key) | 150–300 | 8–12 | med (person grouping/whale split; row order is free, `rules.md:72`) | `types.rs`, `mod.rs`, `sessions.rs`, `people.rs` | G1 sweep 2–30 + 41–43, G4 on p20 |
| 3 | Fuse P1→P2: stream 64-B bucket blocks to dedup while parsing, dropping the 3.05 GB read-back | 100–250 | 20–30 | med-high (must stay order-independent: min-`ingest_s` is associative, `rules.md:51`) | `mod.rs`, new ring/queue | G1 twice, G4, plus a 60M RSS check |
| 4 | Allocate all bucket buffers from one aligned arena with uniform stride (TLB fan-out 1024 → a few entries) | 20–80 | 2 | none | `mod.rs` | G1 + `/usr/bin/time -v` RSS |

Non-additive: 1 and 4 overlap (both are allocation/layout), 3 removes the read-back that 1 only cheapens, and 2's traffic saving mostly *is* track 06's sort saving. Sum of all four ≈ 250–350 ms, not 310–780.

## Details

**1. Pre-size and consolidate.** Two separate wastes. (a) `RawVec` grows geometrically — `cap = max(cap*2, required)` (`alloc/src/raw_vec/mod.rs:521-522`) — and each of the 4×256 P1 buckets and 256×256 P2 buckets starts at 0 and doubles: the geometric copy sum is ≈ 2× final size, i.e. up to ~6 GB of copies at 60M, and capacity slack is up to 2× (part of why p20 peak RSS is 5.5 GB against ~2.7 GB of live records). Caveat: the profile does **not** show this cost — `mi_realloc_aligned` 0.10%, `finish_grow` 0.10%, `mi_page_free_list_extend` 3.21% (`00-profile.md`), so mimalloc evidently extends many large blocks in place; any copy cost hides in the 8.69% unresolved libc group (`memcpy`) and cannot be attributed. Hence the wide 40–150 ms band with the low end favoured. (b) The `ev_out` shape allocates 65,536 `Vec<Ev>` per run (`mod.rs:152`), each ~915 records, then P3 re-reads 256 streams per bucket (`mod.rs:174-179`). Emitting `(bucket_u8, Ev)` into one buffer per dedup task and radix-splitting it in P3 keeps the same byte count but replaces 65k growth histories and a 256-way strided gather with 4 contiguous streams. Verify `bucket(person)` still decides grouping; output row order is explicitly free (`rules.md:72`).

**2. `Ev` 32 → 16 B.** `Ev` carries the 16-byte `person` only to keep a person's events together and to re-emit `user_id`. A bucket-local dense index (≤ ~40k distinct persons per bucket at 60M) makes it `{idx: u32, ts: u32, price: u32, cat: u16, purchase: bool}` = 16 B, and `(idx, ts)` becomes one `u64` key — exactly O9's radix key (`PLAN.md:178`). Saves 0.94 GB (write) + 1.9 GB (gather read+write) ⇒ 150–300 ms. Cost: a person→index map at dedup output, which is why it needs track 12's dense `PersonMap` (today `people.rs:11-21` returns a `u128` name from a hash map) and a per-bucket `Vec<u128>` (~0.6 MB) to resolve at write time.

**3. Streaming handoff.** Dedup is order-insensitive (min `ingest_s`; equal copies identical, `rules.md:51`), so bucket `b` can be deduplicated as soon as its blocks arrive, consuming the 3.05 GB from L3 instead of DRAM. Balkesen et al. describe the canonical form: one buffer per partition sized to exactly one 64-B line, with non-temporal 64-B stores (`pvldb vol7 p85`, §5.2) — here a 48-B record is already ~one line, so the gain is the *read-back*, not write-combining. Bound 100–250 ms because the dedup's random inserts (~17 MB table per bucket, `mod.rs:133`) then contend with parse on HT siblings; the alternative 30%-cheap version — parse the first half of the chunks, start dedup meanwhile — captures most of it for a fraction of the effort.

**4. TLB fan-out.** 1024 concurrent 3 MB bucket streams exceed the 64-entry L1 dTLB; Balkesen §5.1 notes fan-out is TLB-bounded. Uniformly strided arena allocation plus THP backing collapses this to a handful of entries; 20–80 ms, purely mechanical.

## Rejected ideas
- **Pad `EvRaw` to 64 B** for one-line writes: 16-byte-aligned 48-B records straddle a line 1 in 4 times ⇒ line traffic 48 + 16 = 64 B/record, identical to padding, at +33% footprint.
- **Software write-combining / NT stores on the 48-B appends**: the 256 resident stream heads are 12 KB, already L1-resident; NT stores would bypass the cache exactly where dedup wants to read next.
- **Swap in `[u64; 2]` IDs** (D13's text, not the code): sizes `EvRaw` and `Ev` identically; saves only 8 B on `Ck`.
- **Tune bucket bits / chunk size**: already measured, no difference (`PLAN.md:202`).
- **Partition events by person in P1**: would delete the second scatter (~5.6 GB) but requires links first (exposes the 250 ms`→`~800 ms serial union-find that now hides under the parse) and hash(person) is skewed: 5 whales × 1–2% of events put 0.6–1.2M records in single buckets (`rules.md:105`), a 5× tail on P3's critical path.
- **Key-only dedup map + separate payload array**: helps track 04 (map entry 64 → 24 B) but turns my sequential payload read into 60M random 32-B probes — net loss here, flag to track 04.

## Dependencies / conflicts
Track 04 (dedup map layout) shares the same 3.05 GB; track 06 supplies the u64 sort key that idea 2 depends on; track 12's dense person index is a hard prerequisite for idea 2 and reduces idea 4's benefit; track 11 owns the pipeline restructuring in idea 3; track 10 should resolve the 8.69% unresolved-libc group before anyone claims a `memcpy` share.

## Sources
- `src/v2/mod.rs:104-118, 123-133, 132-141, 152-161, 170-186`; `src/v2/types.rs:19-45`; `src/id.rs:40-60`; `src/v2/people.rs:11-21`; `rules.md:51, 72, 105, 121-122`; `PLAN.md:177-178, 202, 246, 357-362`; `research/perf/00-profile.md` symbol table.
- Rust `RawVec` doubling: `~/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/lib/rustlib/src/rust/library/alloc/src/raw_vec/mod.rs:521-522`.
- `u128` alignment 16 on x86-64: https://blog.rust-lang.org/2024/03/30/i128-layout-update/ ; https://blog.rust-lang.org/2024/05/02/Rust-1.78.0.html (LLVM 18 ABI change completed).
- C. Balkesen, G. Alonso, J. Teubner, M. T. Özsu, *Multi-Core, Main-Memory Joins: Sort vs. Hash Revisited*, PVLDB 7(1), §5.1 (radix partitioning, TLB-bounded fan-out; Manegold et al. multi-pass) and §5.2 (software-managed buffers, one cache line + write combining/non-temporal stores): https://www.vldb.org/pvldb/vol7/p85-balkesen.pdf.
- `compare.py` is an order-insensitive diff: `scripts/compare.py:2`.
