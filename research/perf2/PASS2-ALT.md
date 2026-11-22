# Pass 2′ alternative cards (Claude Code session)

These cards are referenced by `SYNTHESIS-alt.md`. They use the same format as `PLAN.md` §6, and the gates are those of §7 plus the rules below. Line 1 can take A0–A4 on top of the other session's W1–W4. Line 2 is B1→B3.

## Rules
- **Benchmark at 60M:** `make bench DATA=data/p60 REF=data/p60.ref RUNS=3` (A0 adds `REF` and a lock).
- **Local machine state for every benchmark:**
  - THP set to `madvise`, matching the GKE default: `echo madvise | sudo tee /sys/kernel/mm/transparent_hugepage/{enabled,defrag}`.
  - Swap close to 0 (`sudo swapoff -a && sudo swapon -a` after closing heavy apps).
  - Record the `free -g` line in the PR.
- **Gates:**
  - G4 (V1 oracle) stays on p20.
  - On p60, output must be identical to `data/p60.ref`, which is the `e6f1279` output.
- **Old paths stay behind knobs** until H4.

---

**A0 — Bench at 60M** · Scope: `scripts/bench.sh`, `Makefile`, `.gitignore` · 45 min
- `REF=<dir>` overrides the default `$DATA.v1out` comparison target.
- Wrap the timed runs in `flock "$ROOT/.bench.lock"`, and add `.bench.lock` to `.gitignore`.
- Create `data/p60.ref` once from `e6f1279`'s output on `data/p60`.
- Record the p60 baseline median.
- Accept: `make bench DATA=data/p60 REF=data/p60.ref` prints 3 runs, the median and `MATCH`. A second bench started at the same time waits.

**A1 — System-time diagnosis** · Scope: `src/main.rs` / `phase()` (`SHOPT_IO_STATS` only) · Depends: A0 · 1 h
- **Per phase, log:**
  - `getrusage(RUSAGE_SELF)` user and system deltas.
  - `/proc/vmstat` deltas: `pgfault`, `thp_fault_alloc`, `thp_fault_fallback`, `compact_stall`, `pgscan_direct`, `pswpin`, `pswpout`.
  - `AnonHugePages` from `/proc/self/smaps_rollup`.
- **Exit timing:** log `[exit] t=<ms>` right before `process::exit(0)`. The bench wall minus that number is the exit teardown.
- **Runs:** 60M under THP `madvise` and `always`, swap cleared. Append the table to `00-profile.md`.
- **Decision:** system time < 3 CPU-s with swap clear means it was a laptop artifact.

**A2 — Huge pages for large allocations** · Scope: new `src/hugealloc.rs`, `src/main.rs` (`#[global_allocator]`), `Cargo.toml` (`libc`, already a transitive dependency via memmap2; add it to D3) · Depends: A1 · 2–3 h · est. 0.2–0.5 s on a `madvise` scorer, ~0 on a THP-`always` host
- **The allocator:** a `GlobalAlloc` wrapping mimalloc.
  - **`alloc`:** if size ≥ `SHOPT_HUGE_MIN` (default 2 MiB), `mmap` anonymous memory rounded up to 2 MiB. Over-map by 2 MiB and trim, so the start is 2 MiB-aligned. Then `madvise(MADV_HUGEPAGE)`.
  - **`realloc` of a large block:** `mremap(MREMAP_MAYMOVE)`.
  - **`dealloc`:** `munmap`.
  - **`alloc_zeroed`:** mmap'd blocks are already zero.
  - **Fallback:** any failure goes to mimalloc. `SHOPT_HUGE=0` disables the wrapper.
- **Tracking:** decide the owner from size alone (≥ threshold ⇒ mmap). `realloc` across the threshold copies between the two allocators.
- **Accept:**
  - G1 in all configs, plus `SHOPT_HUGE=0`.
  - V1 fallback test, and G4.
  - 60M under THP `madvise`: dedup `minflt` down ≥ 3×; `AnonHugePages` ≥ 4 GB; wall ≥ 3% better.
  - Under THP `always`: no regression beyond noise.
- **Traps:**
  - Alignment > 2 MiB.
  - `realloc` shrink.
  - `vm.max_map_count`: assert the count of mmap'd blocks stays < 20k.
  - Never let mimalloc free a block it didn't allocate.

**A3 — Shape-first timestamp parse** · This is the `07-input-shape.md` "Recommended card", as written · 2.5 h · est. 0.1–0.3 s.

**A4 — Unmap the input in the background** · Scope: `src/v2/mod.rs` · 0.5–1 h · est. 50–80 ms
- After P1 (events and clicks), nothing references the input bytes.
- Move `event_maps`, `click_maps` and `link_maps` (and the chunk slices borrowing them) into `std::thread::spawn(move || drop(...))` before P2.
- Accept: G1 + G4; the A1 exit gap at 60M shrinks.

---

**B1 — Thin-tuple dedup + streaming person scatter** · Scope: `src/v2/mod.rs` (P1 events, P2), new `src/v2/dedup.rs`, `src/v2/types.rs` (`Thin`) · Base: `e6f1279` (doesn't need W1 or W2); rebase on Line 1 before merging · **5–7 h** · est. dedup 2.6 s → 0.6–1.0 s at 60M, plus a cheaper P1 event scatter

**Knobs:**
- `SHOPT_DEDUP=thin|wide` (default `thin` once accepted). `wide` is today's P2, kept unchanged.
- `SHOPT_DEDUP_BITS` (default 11).
- `SHOPT_PERSON_BITS` (default 8).
- `SHOPT_BUCKET_BITS=n` sets both.
- **The same `PERSON_BITS` drives the click scatter, the P2 output and P3.**

**Steps:**
1. **P1 events.** Each worker appends every parsed `EvRaw` to its own `recs: Vec<EvRaw>` in parse order (no scatter), pre-sized to bytes/70/workers × 1.15.
   - It also pushes `Thin { id_lo: u64, id_hi: u64, key: u64 }` (24 B, `#[repr(C)]`) into `thin[id::bucket(id, DEDUP_BITS)]`.
   - `key = (((ingest_s as u32) ^ 0x8000_0000) as u64) << 32 | (w << 28 | offset)`.
   - If `offset ≥ 2^28` or there are more than 16 workers, set `overflow` (V1 fallback).
   - `Skip` lines produce nothing.
2. **P2a: dedup.** Run `par_iter` over partitions; each task reuses one `HashMap<u128, u64>` (`with_capacity(max partition)`, `clear()` between partitions).
   - **Vacant:** insert `key`.
   - **Occupied:** push the larger key's low 32 bits to a task-local `losers: Vec<u32>`, and keep the min.
   - **Ignore `VALID`** here (E5).
3. **Loser bitmaps.** Allocate per-worker `Vec<u64>` bitmaps and set every loser bit. Drop `thin` in the background.
4. **P2b: streaming person scatter.** Run `par_iter` over ~1M-record blocks of `recs`.
   - **Pass 1:** skip the record if its loser bit is set or `flags & VALID == 0` (today's survivor test, `mod.rs:238-243`). Otherwise set `r.user = people.person(r.user)` and bump `counts[id::bucket(r.user, PERSON_BITS)]`.
   - **Allocation:** exact-size `Vec<Ev>` per person bucket.
   - **Pass 2:** fill them, building the `Ev` as `mod.rs:248-255` does.
   - **Output shape:** `ev_out` keeps today's shape, so P3 is unchanged. Drop `recs` in the background.
5. **No change to `sessions.rs`, clicks or output.**

**Accept:**
- **Unit test:** the kernel vs a `HashMap<u128, EvRaw>` reference with duplicate groups of 2–4 copies, equal ingest, an invalid earliest copy (E5), and a later copy naming another user (E4).
- **G1** in configs:
  - default
  - `SHOPT_DEDUP=wide`
  - `SHOPT_CHUNK_BYTES=4096`
  - `SHOPT_DEDUP_BITS=1 SHOPT_PERSON_BITS=1 SHOPT_CHUNK_BYTES=100`
  - `SHOPT_DEDUP_BITS=12 SHOPT_PERSON_BITS=3`
- **G4** `MATCH` on p20.
- **p60:** `MATCH` vs `data/p60.ref`.
- **60M bench:** `dedup` ≤ 1.3 s; `links+events` no more than 150 ms slower; peak `RssAnon` ≤ 10 GB.

**Traps:**
- **E4:** the person comes from the winning record's own `user`.
- **E5:** an invalid winner drops the event.
- **Malformed lines** never produce a tuple.
- **Equal ingest:** copies are identical (`rules.md` step 2).
- **Overflow** returns `Ok(false)` before any output.

**Go/no-go:** if B1 hasn't passed G1 by **Sun 03:30**, drop it.

**B2 — Bits A/B** · Depends: B1 · 1 h
- At 60M: `SHOPT_DEDUP_BITS` ∈ {10, 11, 12} × `SHOPT_PERSON_BITS` ∈ {8, 10, 11}. Screen with one run each; take 3 runs for the top two.
- Keep `PERSON_BITS=8` unless the gain is ≥ 100 ms.

**B3 — 60M check + G2** · As S3-9: peak anon ≤ 10 GB, 5M seeds 51–53, p60 `MATCH`, `make docker-fixtures` + `make run-dev`.

**H8 — Submit #5** · Human · §8 protocol.

**C1 (optional) — Four-thread P2a pool A/B** · 1 h · adopt only if the 60M wall improves ≥ 3%.
